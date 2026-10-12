//! Browser smoke test using an ephemeral in-memory credential store (never touches real identity).
#![allow(clippy::print_stdout)]
use nexus_auth::{Verifier, browser::BrowserPrompt};
use std::sync::Arc;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut verifier = Verifier::new(Arc::new(nexus_crypto::store::MemoryStore::new()))?;
    let (id, options) = verifier.enroll(true, "Test device")?;
    let prompt = BrowserPrompt::start(options, true, "Test device", false).await?;
    println!("{}", prompt.url());
    verifier.finish_enrollment(&id, &prompt.wait().await?, 1_900_000_000)?;
    let challenge = verifier.begin([1; 32], [2; 32])?;
    let prompt = BrowserPrompt::start(
        challenge.public_key.ok_or("no passkey challenge")?,
        false,
        "Test device",
        false,
    )
    .await?;
    println!("{}", prompt.url());
    verifier.finish(
        &challenge.id,
        [1; 32],
        [2; 32],
        "",
        &prompt.wait().await?,
        1_900_000_000,
    )?;
    println!("passkey browser registration and authentication verified");
    Ok(())
}
