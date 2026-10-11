//! Release tooling for the publisher: make a signing key, sign a manifest.
//!
//! ```text
//! nexus-release keygen --out release.key          # prints the public key to ship in the app
//! nexus-release sign --key release.key manifest.json > manifest.signed.json
//! ```

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs)]

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use nexus_crypto::{DeviceIdentity, IdentitySeed};
use nexus_update::{Manifest, manifest};

#[derive(Debug, Parser)]
#[command(name = "nexus-release", about = "Sign Nexus Remote release manifests")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new publisher key. Keep the file offline; ship only the printed public key.
    Keygen {
        /// Where to write the secret (refuses to overwrite).
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign a manifest JSON file; the signed manifest goes to stdout.
    Sign {
        /// The secret key file from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// The unsigned manifest.
        manifest: PathBuf,
    },
    /// Print the public key for a secret key file.
    PublicKey {
        /// The secret key file.
        key: PathBuf,
    },
}

fn load(path: &PathBuf) -> Result<DeviceIdentity> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes: Vec<u8> = (0..text.trim().len() / 2)
        .map(|i| u8::from_str_radix(&text.trim()[i * 2..i * 2 + 2], 16))
        .collect::<Result<_, _>>()
        .context("the key file is not hex")?;
    let seed = IdentitySeed::from_slice(&bytes).context("the key file has the wrong length")?;
    Ok(DeviceIdentity::from_seed(&seed))
}

fn public_hex(id: &DeviceIdentity) -> String {
    id.public()
        .ed25519_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Keygen { out } => {
            if out.exists() {
                bail!(
                    "{} already exists; refusing to overwrite a key",
                    out.display()
                );
            }
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("{e}"))?;
            let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
            {
                use std::io::Write as _;
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let mut f = opts
                    .open(&out)
                    .with_context(|| format!("creating {}", out.display()))?;
                writeln!(f, "{hex}")?;
            }
            let id = DeviceIdentity::from_seed(&IdentitySeed::from_bytes(seed));
            println!(
                "public key (ship this in the app and in update.public_keys):\n{}",
                public_hex(&id)
            );
        }
        Command::PublicKey { key } => println!("{}", public_hex(&load(&key)?)),
        Command::Sign {
            key,
            manifest: path,
        } => {
            let id = load(&key)?;
            let text = std::fs::read_to_string(&path)?;
            let m: Manifest = serde_json::from_str(&text).context("parsing the manifest")?;
            m.parsed_version()
                .context("the version is not valid semver")?;
            let signed = manifest::sign(&id, &m)?;
            println!("{}", serde_json::to_string_pretty(&signed)?);
        }
    }
    Ok(())
}
