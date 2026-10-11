//! `nexus forward`: reach a service on a paired device through a local port.

use std::process::ExitCode;

use anyhow::{Result, bail};
use nexus_core::i18n::{tr, tr_args};
use nexus_proto::v1::OpenSession;

use crate::AppContext;
use crate::agent_cmds::{connect_with_events, resolve};

/// A parsed forward specification.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    /// Port to listen on locally (0 = any free port).
    pub local: u16,
    /// Host as seen from the device.
    pub host: String,
    /// Port on that host.
    pub port: u16,
}

fn port(s: &str) -> Option<u16> {
    s.parse::<u16>().ok().filter(|p| *p != 0)
}

/// Parses `PORT`, `LOCAL:PORT`, `HOST:PORT` or `LOCAL:HOST:PORT` (IPv6 hosts in brackets).
pub(crate) fn parse_spec(text: &str) -> Option<Spec> {
    // Pull a bracketed IPv6 host out first so its colons do not confuse the split.
    let (text, bracketed) = match (text.find('['), text.find(']')) {
        (Some(a), Some(b)) if a < b => (
            format!("{}H{}", &text[..a], &text[b + 1..]),
            Some(text[a + 1..b].to_owned()),
        ),
        _ => (text.to_owned(), None),
    };
    let parts: Vec<&str> = text.split(':').collect();
    let host_of = |p: &str| {
        bracketed
            .clone()
            .filter(|_| p == "H")
            .unwrap_or_else(|| p.to_owned())
    };
    match parts.as_slice() {
        [p] => {
            let p = port(p)?;
            Some(Spec {
                local: p,
                host: "localhost".into(),
                port: p,
            })
        }
        [a, b] => {
            let remote = port(b)?;
            if let Some(local) = port(a) {
                Some(Spec {
                    local,
                    host: "localhost".into(),
                    port: remote,
                })
            } else if a.is_empty() {
                None
            } else {
                Some(Spec {
                    local: remote,
                    host: host_of(a),
                    port: remote,
                })
            }
        }
        [a, h, b] if !h.is_empty() => Some(Spec {
            local: port(a)?,
            host: host_of(h),
            port: port(b)?,
        }),
        _ => None,
    }
}

pub(crate) async fn forward(ctx: &AppContext, device: &str, spec: &str) -> Result<ExitCode> {
    let l = ctx.locale;
    let Some(spec) = parse_spec(spec) else {
        bail!("{}", tr(l, "cli.forward.bad_spec"));
    };
    let (client, mut events) = connect_with_events(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(l, &devices, device)?;
    let opened = client
        .open_session(OpenSession {
            device_id: dev.device_id.clone(),
            want_video: false,
            ..Default::default()
        })
        .await?;
    let started = client
        .start_forward(opened.session_id, spec.local, &spec.host, spec.port)
        .await;
    let started = match started {
        Ok(s) => s,
        Err(e) => {
            let _ = client.close_session(opened.session_id).await;
            return Err(e.into());
        }
    };
    println!(
        "{}",
        tr_args(
            l,
            "cli.forward.listening",
            &[
                ("local", &started.local_port.to_string()),
                ("remote", &format!("{}:{}", spec.host, spec.port)),
                ("name", &dev.name)
            ]
        )
    );
    // Stay up until interrupted or the session ends.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = events.next() => match ev {
                Some(ev) => {
                    if let Some(nexus_proto::v1::ipc_event::Body::SessionEnded(e)) = ev.body
                        && e.session_id == opened.session_id
                    {
                        eprintln!("{}", e.reason);
                        break;
                    }
                }
                None => break,
            },
        }
    }
    let _ = client.stop_forward(started.forward_id).await;
    let _ = client.close_session(opened.session_id).await;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(local: u16, host: &str, port: u16) -> Option<Spec> {
        Some(Spec {
            local,
            host: host.into(),
            port,
        })
    }

    #[test]
    fn forms_parse() {
        assert_eq!(parse_spec("5432"), spec(5432, "localhost", 5432));
        assert_eq!(parse_spec("8080:80"), spec(8080, "localhost", 80));
        assert_eq!(
            parse_spec("db.internal:5432"),
            spec(5432, "db.internal", 5432)
        );
        assert_eq!(
            parse_spec("9000:db.internal:5432"),
            spec(9000, "db.internal", 5432)
        );
        assert_eq!(parse_spec("[::1]:3000"), spec(3000, "::1", 3000));
        assert_eq!(parse_spec("7000:[::1]:3000"), spec(7000, "::1", 3000));
    }

    #[test]
    fn junk_is_rejected() {
        for bad in ["", "0", "70000", "a:b:c:d", "host:", ":80", "x:y:z"] {
            assert_eq!(parse_spec(bad), None, "{bad}");
        }
    }
}
