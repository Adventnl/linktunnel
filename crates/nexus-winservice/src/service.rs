//! The service control manager glue.
//!
//! Verification status: source-only Windows revision; no builds or tests (docs/STATUS.md).

use std::ffi::OsString;
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use tracing::{error, info, warn};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::launcher::{self, Child};
use crate::supervisor::{Action, Event, Supervisor};
use crate::{SERVICE_NAME, ServiceError};

static CHILD_ARGS: OnceLock<Vec<String>> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub(crate) fn run(child_args: Vec<String>) -> Result<(), ServiceError> {
    let _ = CHILD_ARGS.set(child_args);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| ServiceError::Os {
        what: "service dispatcher (run this only from the service control manager)",
        detail: e.to_string(),
    })
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        error!(error = %e, "the Nexus service failed");
    }
}

/// What the control handler tells the main loop.
enum Control {
    Stop,
    SessionChange,
}

fn status(state: ServiceState, accept: ServiceControlAccept, wait: Duration) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: wait,
        process_id: None,
    }
}

fn run_service() -> Result<(), ServiceError> {
    let (tx, rx) = mpsc::channel::<Control>();
    let handler_tx = tx.clone();
    let handle = service_control_handler::register(SERVICE_NAME, move |ctl| match ctl {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            let _ = handler_tx.send(Control::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            let _ = handler_tx.send(Control::SessionChange);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|e| ServiceError::Os {
        what: "register the service control handler",
        detail: e.to_string(),
    })?;

    let accept = ServiceControlAccept::STOP
        | ServiceControlAccept::SHUTDOWN
        | ServiceControlAccept::PRESHUTDOWN
        | ServiceControlAccept::SESSION_CHANGE;
    let set = |s: ServiceState, a: ServiceControlAccept| {
        let _ = handle.set_service_status(status(s, a, Duration::from_secs(10)));
    };
    set(ServiceState::StartPending, ServiceControlAccept::empty());

    let exe = std::env::current_exe().map_err(|e| ServiceError::Os {
        what: "locating nexus-agent.exe",
        detail: e.to_string(),
    })?;
    let args = CHILD_ARGS.get().cloned().unwrap_or_default();

    // The agent's state holds the identity key: make sure only SYSTEM and Administrators can read it.
    let paths = nexus_core_paths();
    if let Err(e) = std::fs::create_dir_all(&paths)
        .map_err(|e| e.to_string())
        .and_then(|()| crate::acl::restrict_to_system_and_admins(&paths).map_err(|e| e.to_string()))
    {
        return Err(ServiceError::Os {
            what: "restrict service state directory",
            detail: e,
        });
    }

    set(ServiceState::Running, accept);
    info!("Nexus service running");

    let mut sup = Supervisor::new();
    let mut child: Option<Child> = None;
    let mut timer: Option<Instant> = None;
    let mut pending = sup.on_event(Event::Start {
        console: launcher::interactive_session(),
    });

    'outer: loop {
        // Carry out what the supervisor asked for.
        let todo = std::mem::take(&mut pending);
        for action in todo {
            match action {
                Action::Launch(session) => match launcher::launch(&exe, &args, session) {
                    Ok(c) => {
                        info!(session, "agent started");
                        child = Some(c);
                    }
                    Err(e) => {
                        warn!(session, error = %e, "could not start the agent");
                        // Treated like an immediate crash, so it backs off and retries.
                        pending = sup.on_event(Event::ChildExited {
                            ran_for: Duration::ZERO,
                        });
                        continue 'outer;
                    }
                },
                Action::Kill => {
                    if let Some(c) = &child {
                        c.kill();
                    }
                }
                Action::Wait(d) => timer = Some(Instant::now() + d),
                Action::Exit => break 'outer,
            }
        }

        if let Some(c) = &child
            && let Err(e) = c.dispatch_secure_attention()
        {
            if matches!(&e, ServiceError::Os { what, .. } if *what == "revert SAS impersonation") {
                return Err(e);
            }
            warn!(error = %e, "could not dispatch secure attention");
        }
        // Poll as well as consume notifications: a missing console at boot/user switching must
        // be queried again before a retry, rather than retrying a permanently cached None.
        let selected = launcher::interactive_session();
        if selected != sup.running_in() && sup.running_in().is_some() {
            pending = sup.on_event(Event::ConsoleChanged { console: selected });
            if !pending.is_empty() {
                continue;
            }
        }
        // Wait for something to happen: a control, the timer, or the child dying.
        let tick = Duration::from_millis(250);
        let mut event = None;
        match rx.recv_timeout(tick) {
            Ok(Control::Stop) => event = Some(Event::Stop),
            Ok(Control::SessionChange) => {
                event = Some(Event::ConsoleChanged {
                    console: launcher::interactive_session(),
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => event = Some(Event::Stop),
        }
        if event.is_none()
            && let Some(c) = &child
            && c.has_exited()
        {
            let ran_for = c.ran_for();
            info!(code = ?c.exit_code(), ?ran_for, "agent exited");
            child = None;
            event = Some(Event::ChildExited { ran_for });
        }
        if event.is_none()
            && let Some(t) = timer
            && Instant::now() >= t
        {
            timer = None;
            // Update the desired session before consuming the timer. Do not bypass crash
            // backoff with a duplicate ConsoleChanged notification for an unchanged session.
            let changed = sup.on_event(Event::ConsoleChanged {
                console: launcher::interactive_session(),
            });
            if changed.is_empty() {
                event = Some(Event::Timer);
            } else {
                pending = changed;
                continue;
            }
        }
        if let Some(ev) = event {
            pending = sup.on_event(ev);
        }
    }

    set(ServiceState::StopPending, ServiceControlAccept::empty());
    drop(child);
    let _ = handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        Duration::ZERO,
    ));
    info!("Nexus service stopped");
    Ok(())
}

/// `%ProgramData%\Nexus`.
fn nexus_core_paths() -> std::path::PathBuf {
    std::env::var_os("ProgramData")
        .map_or_else(
            || std::path::PathBuf::from(r"C:\ProgramData"),
            std::path::PathBuf::from,
        )
        .join("Nexus")
}
