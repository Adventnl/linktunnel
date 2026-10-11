//! `nexus id …`: create, show and delete the device identity.

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nexus_core::Paths;
use nexus_core::i18n::{Locale, tr, tr_args};
use nexus_crypto::store::{SecretStore, StoreKind};
use nexus_crypto::{CryptoError, DeviceIdentity};
use serde::Serialize;

use crate::AppContext;

#[derive(Debug, Subcommand)]
pub enum IdCommand {
    /// Show this device's ID, fingerprint and public key.
    Show {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Create the device identity (fails if one exists).
    Create {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Permanently delete the device identity. All pairings become invalid.
    Delete {
        /// Confirm the destructive action.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Serialize)]
struct IdentityView {
    device_id: String,
    fingerprint: String,
    ed25519_public_key: String,
    x25519_public_key: String,
    secret_store: String,
}

fn view(identity: &DeviceIdentity, store_desc: String) -> IdentityView {
    IdentityView {
        device_id: identity.device_id().to_string(),
        fingerprint: identity.public().fingerprint().to_string(),
        ed25519_public_key: hex(&identity.public().ed25519_bytes()),
        x25519_public_key: hex(identity.public().x25519()),
        secret_store: store_desc,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Human-readable description of where the secret lives.
pub fn describe_store(locale: Locale, store: &dyn SecretStore, _paths: &Paths) -> String {
    match store.kind() {
        StoreKind::OsKeychain => tr_args(locale, "cli.store.os", &[("name", &store.describe())]),
        StoreKind::File => tr_args(locale, "cli.store.file", &[("path", &store.describe())]),
        StoreKind::Memory => store.describe(),
    }
}

fn print_identity(ctx: &AppContext, identity: &DeviceIdentity, json: bool) -> Result<()> {
    let v = view(
        identity,
        describe_store(ctx.locale, ctx.store.as_ref(), &ctx.paths),
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let l = ctx.locale;
        println!("{:<16} {}", tr(l, "cli.identity.device_id"), v.device_id);
        println!(
            "{:<16} {}",
            tr(l, "cli.identity.fingerprint"),
            v.fingerprint
        );
        println!(
            "{:<16} {}",
            tr(l, "cli.identity.public_key"),
            v.ed25519_public_key
        );
        println!(
            "{:<16} {}",
            tr(l, "cli.identity.secret_store"),
            v.secret_store
        );
    }
    Ok(())
}

pub fn run(ctx: &AppContext, cmd: &IdCommand) -> Result<ExitCode> {
    let l = ctx.locale;
    match *cmd {
        IdCommand::Show { json } => {
            let identity = nexus_crypto::load_identity(ctx.store.as_ref())
                .context("loading the device identity")?;
            let Some(identity) = identity else {
                eprintln!("{}", tr(l, "cli.identity.missing"));
                return Ok(ExitCode::FAILURE);
            };
            print_identity(ctx, &identity, json)?;
        }
        IdCommand::Create { json } => match nexus_crypto::create_identity(ctx.store.as_ref()) {
            Ok(identity) => {
                if !json {
                    println!("{}\n", tr(l, "cli.identity.created"));
                }
                print_identity(ctx, &identity, json)?;
            }
            Err(CryptoError::AlreadyExists) => {
                eprintln!("{}", tr(l, "cli.identity.exists"));
                return Ok(ExitCode::FAILURE);
            }
            Err(e) => return Err(e).context("creating the device identity"),
        },
        IdCommand::Delete { yes } => {
            if !yes {
                bail!("{}", tr(l, "cli.identity.confirm_delete"));
            }
            nexus_crypto::delete_identity(ctx.store.as_ref())
                .context("deleting the device identity")?;
            println!(
                "{}",
                tr_args(
                    l,
                    "cli.identity.deleted",
                    &[("store", &ctx.store.describe())]
                )
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
