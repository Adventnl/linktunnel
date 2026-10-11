//! `nexus audit`: read and verify this device's tamper-evident log.

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Subcommand;
use nexus_core::i18n::{tr, tr_args};

use crate::AppContext;

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Show the most recent entries.
    Tail {
        /// How many entries.
        #[arg(short, default_value_t = 20)]
        n: usize,
    },
    /// Check the whole log for edits, deleted entries and a cut-off end.
    Verify,
}

pub(crate) fn run(ctx: &AppContext, cmd: &AuditCommand) -> Result<ExitCode> {
    let dir = ctx.paths.data_dir.join("audit");
    if !dir.exists() {
        bail!("{}", tr(ctx.locale, "cli.audit.none"));
    }
    match cmd {
        AuditCommand::Tail { n } => {
            let log = nexus_audit::AuditLog::open(&dir, None)?;
            for r in log.recent(*n)? {
                let details = r
                    .detail
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "{:>6}  {}  {:<22} {} {}",
                    r.seq,
                    format_time(r.ts_ms),
                    r.kind,
                    r.actor.as_deref().map(short).unwrap_or_default(),
                    details
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        AuditCommand::Verify => {
            // The signed head is checked against this device's own key.
            let identity = nexus_crypto::load_identity(ctx.store.as_ref())?;
            let verifier = identity.as_ref().map(|i| i.public().clone());
            let v = nexus_audit::verify(&dir, verifier.as_ref())?;
            match &v.problem {
                None => {
                    println!(
                        "{}",
                        tr_args(
                            ctx.locale,
                            "cli.audit.ok",
                            &[
                                ("records", &v.records.to_string()),
                                (
                                    "head",
                                    &tr(
                                        ctx.locale,
                                        if v.head_verified {
                                            "cli.audit.head_ok"
                                        } else {
                                            "cli.audit.head_unchecked"
                                        }
                                    )
                                )
                            ]
                        )
                    );
                    Ok(ExitCode::SUCCESS)
                }
                Some((seq, why)) => {
                    println!(
                        "{}",
                        tr_args(
                            ctx.locale,
                            "cli.audit.broken",
                            &[("seq", &seq.to_string()), ("why", why)]
                        )
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
    }
}

fn short(s: &str) -> String {
    s.chars().take(12).collect()
}

/// `YYYY-MM-DD HH:MM:SS` in UTC, without a date library.
fn format_time(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn times_format_as_utc() {
        assert_eq!(format_time(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_time(1_700_000_000_000), "2023-11-14 22:13:20Z");
        assert_eq!(format_time(951_782_400_000), "2000-02-29 00:00:00Z");
    }
}
