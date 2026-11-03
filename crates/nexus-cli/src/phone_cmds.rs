//! The same Android boundary used by the desktop, available without opening the app.
use anyhow::{Result, bail};
use clap::Subcommand;
use std::process::ExitCode;

#[derive(Debug, Subcommand)]
pub enum PhoneCommand {
    /// Tool versions and availability (JSON).
    Status,
    /// List Android transports, including unauthorised ones (JSON).
    Devices,
    /// Pair over Android wireless debugging. Reads the pairing code privately from the terminal.
    Pair { address: String },
    /// Connect an already authorised wireless transport.
    Connect { address: String },
    /// Disconnect a wireless transport.
    Disconnect { address: String },
    /// Control one authorised phone. Keeps running until Ctrl+C or the phone window closes.
    View {
        serial: String,
        #[arg(long)]
        screen_off: bool,
        #[arg(long)]
        audio: bool,
        #[arg(long, default_value_t = 1600, value_parser = clap::value_parser!(u16).range(720..=2560))]
        max_size: u16,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(15..=120))]
        fps: u16,
    },
}

pub async fn run(locale: nexus_core::i18n::Locale, command: PhoneCommand) -> Result<ExitCode> {
    let phone = nexus_phone::Phone::default();
    match command {
        PhoneCommand::Status => {
            println!("{}", serde_json::to_string_pretty(&phone.status().await)?)
        }
        PhoneCommand::Devices => {
            println!("{}", serde_json::to_string_pretty(&phone.devices().await?)?)
        }
        PhoneCommand::Pair { address } => {
            let prompt = nexus_core::i18n::tr(locale, "cli.phone.pair_code");
            let code =
                tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt)).await??;
            phone.pair(&address, &code).await?;
        }
        PhoneCommand::Connect { address } => phone.connect(&address).await?,
        PhoneCommand::Disconnect { address } => phone.disconnect(&address).await?,
        PhoneCommand::View {
            serial,
            screen_off,
            audio,
            max_size,
            fps,
        } => {
            phone
                .start(nexus_phone::Options {
                    serial,
                    screen_off,
                    media_audio: audio,
                    max_size,
                    max_fps: fps,
                })
                .await?;
            let result: Result<()> = async {
                loop {
                    tokio::select! {
                        signal = tokio::signal::ctrl_c() => { signal?; break; }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                            let session = phone.session().await;
                            if let Some(error) = session.error { bail!("{error}"); }
                            if session.serial.is_none() { break; }
                        }
                    }
                }
                Ok(())
            }
            .await;
            phone.stop().await?;
            result?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
