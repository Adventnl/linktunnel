//! Power actions and Wake-on-LAN.
//!
//! Actions run the operating system's own tools (`systemctl`, `pmset`/`osascript`, `shutdown`,
//! `rundll32`) instead of calling privileged APIs, which keeps this crate free of `unsafe` and
//! means the OS decides whether the calling user is allowed to do it. The mapping from action to
//! command line is a pure function ([`command_for`]) so it is tested on every platform; running
//! the commands is only exercised for real on the platform at hand.

#![forbid(unsafe_code)]

mod wake;

use std::process::{Command, Stdio};

pub use wake::{WakeError, magic_packet, parse_mac, send_magic_packet};

/// Something a person might want to do to a machine remotely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerAction {
    /// Lock the screen (the session stays running).
    Lock,
    /// Suspend to RAM.
    Sleep,
    /// Suspend to disk.
    Hibernate,
    /// Reboot.
    Restart,
    /// Power off.
    Shutdown,
    /// End the user's session.
    LogOff,
}

impl PowerAction {
    /// All actions.
    pub const ALL: [Self; 6] = [
        Self::Lock,
        Self::Sleep,
        Self::Hibernate,
        Self::Restart,
        Self::Shutdown,
        Self::LogOff,
    ];

    /// True for actions that end the controller's connection (so the result is sent first).
    #[must_use]
    pub fn drops_connection(self) -> bool {
        !matches!(self, Self::Lock)
    }
}

/// Operating systems the command table knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux and other Unixes with systemd.
    Linux,
}

impl Os {
    /// The OS this binary runs on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    /// Executable name or path.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

fn cmd(program: &str, args: &[&str]) -> CommandLine {
    CommandLine {
        program: program.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    }
}

/// The command that performs `action` on `os`, or `None` if that OS cannot do it.
#[must_use]
pub fn command_for(os: Os, action: PowerAction) -> Option<CommandLine> {
    use PowerAction::{Hibernate, Lock, LogOff, Restart, Shutdown, Sleep};
    Some(match (os, action) {
        (Os::Windows, Lock) => cmd("rundll32.exe", &["user32.dll,LockWorkStation"]),
        (Os::Windows, Sleep) => cmd("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        (Os::Windows, Hibernate) => cmd("shutdown", &["/h"]),
        (Os::Windows, Restart) => cmd("shutdown", &["/r", "/t", "0"]),
        (Os::Windows, Shutdown) => cmd("shutdown", &["/s", "/t", "0"]),
        (Os::Windows, LogOff) => cmd("shutdown", &["/l"]),

        (Os::MacOs, Lock) => cmd(
            "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession",
            &["-suspend"],
        ),
        (Os::MacOs, Sleep) => cmd("pmset", &["sleepnow"]),
        // macOS has no user-triggerable hibernate that differs from sleep with the default
        // hibernatemode; report it as unsupported rather than do something else.
        (Os::MacOs, Hibernate) => return None,
        (Os::MacOs, Restart) => cmd(
            "osascript",
            &["-e", "tell application \"System Events\" to restart"],
        ),
        (Os::MacOs, Shutdown) => cmd(
            "osascript",
            &["-e", "tell application \"System Events\" to shut down"],
        ),
        (Os::MacOs, LogOff) => cmd(
            "osascript",
            &["-e", "tell application \"System Events\" to log out"],
        ),

        (Os::Linux, Lock) => cmd("loginctl", &["lock-session"]),
        (Os::Linux, Sleep) => cmd("systemctl", &["suspend"]),
        (Os::Linux, Hibernate) => cmd("systemctl", &["hibernate"]),
        (Os::Linux, Restart) => cmd("systemctl", &["reboot"]),
        (Os::Linux, Shutdown) => cmd("systemctl", &["poweroff"]),
        (Os::Linux, LogOff) => cmd("loginctl", &["terminate-user", &whoami()]),
    })
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "".to_owned())
}

/// Why an action did not happen.
#[derive(Debug, thiserror::Error)]
pub enum PowerError {
    /// This OS (or this build) cannot do that.
    #[error("{0:?} is not supported on this operating system")]
    Unsupported(PowerAction),
    /// The tool could not be started.
    #[error("could not run {program}: {reason}")]
    Spawn {
        /// The program.
        program: String,
        /// The OS error.
        reason: String,
    },
    /// The tool ran and failed.
    #[error("{program} failed: {detail}")]
    Failed {
        /// The program.
        program: String,
        /// Its error output.
        detail: String,
    },
}

/// Performs power actions. The trait lets the session code be tested without rebooting anything.
pub trait PowerBackend: Send + Sync {
    /// Does it. Returns once the action has been *started* (a reboot returns before the machine
    /// goes down).
    fn perform(&self, action: PowerAction) -> Result<(), PowerError>;
    /// What this backend can do.
    fn supported(&self) -> Vec<PowerAction>;
}

/// The real thing.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPower;

impl PowerBackend for SystemPower {
    fn perform(&self, action: PowerAction) -> Result<(), PowerError> {
        let line = command_for(Os::current(), action).ok_or(PowerError::Unsupported(action))?;
        tracing::warn!(?action, program = %line.program, "performing power action");
        let out = Command::new(&line.program)
            .args(&line.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| PowerError::Spawn {
                program: line.program.clone(),
                reason: e.to_string(),
            })?;
        if out.status.success() {
            Ok(())
        } else {
            Err(PowerError::Failed {
                program: line.program,
                detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    fn supported(&self) -> Vec<PowerAction> {
        PowerAction::ALL
            .into_iter()
            .filter(|a| command_for(Os::current(), *a).is_some())
            .collect()
    }
}

/// Records requests instead of acting on them (tests, dry runs).
#[derive(Debug, Default, Clone)]
pub struct RecordingPower {
    done: std::sync::Arc<parking_lot::Mutex<Vec<PowerAction>>>,
}

impl RecordingPower {
    /// An empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been requested, in order.
    #[must_use]
    pub fn actions(&self) -> Vec<PowerAction> {
        self.done.lock().clone()
    }
}

impl PowerBackend for RecordingPower {
    fn perform(&self, action: PowerAction) -> Result<(), PowerError> {
        self.done.lock().push(action);
        Ok(())
    }

    fn supported(&self) -> Vec<PowerAction> {
        PowerAction::ALL.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_pair_has_a_command_and_the_gaps_are_deliberate() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            for action in PowerAction::ALL {
                let c = command_for(os, action);
                if let Some(c) = c {
                    assert!(!c.program.is_empty(), "{os:?} {action:?}");
                } else {
                    assert_eq!((os, action), (Os::MacOs, PowerAction::Hibernate));
                }
            }
        }
    }

    #[test]
    fn destructive_commands_are_what_the_os_documents() {
        let line = |os, a| {
            let c = command_for(os, a).unwrap();
            format!("{} {}", c.program, c.args.join(" "))
        };
        assert_eq!(line(Os::Linux, PowerAction::Restart), "systemctl reboot");
        assert_eq!(line(Os::Linux, PowerAction::Shutdown), "systemctl poweroff");
        assert_eq!(line(Os::Windows, PowerAction::Shutdown), "shutdown /s /t 0");
        assert_eq!(line(Os::Windows, PowerAction::Restart), "shutdown /r /t 0");
        assert_eq!(line(Os::MacOs, PowerAction::Sleep), "pmset sleepnow");
    }

    #[test]
    fn only_lock_keeps_the_connection() {
        for a in PowerAction::ALL {
            assert_eq!(a.drops_connection(), a != PowerAction::Lock);
        }
    }

    #[test]
    fn the_recorder_records_and_a_missing_tool_is_an_error() {
        let r = RecordingPower::new();
        r.perform(PowerAction::Sleep).unwrap();
        r.perform(PowerAction::Lock).unwrap();
        assert_eq!(r.actions(), [PowerAction::Sleep, PowerAction::Lock]);
        let missing = CommandLine {
            program: "definitely-not-a-program-xyz".into(),
            args: vec![],
        };
        let err = Command::new(&missing.program).output().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
