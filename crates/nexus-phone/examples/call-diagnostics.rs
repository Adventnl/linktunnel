//! Read-only runtime probe: never registers a phone, dials, or captures audio.
#![allow(clippy::print_stdout, clippy::print_stderr)]
#[tokio::main]
async fn main() {
    let calls = nexus_phone::calls::Calls::default();
    match calls.status().await {
        Ok(status) => {
            println!(
                "Windows API: {}, Bluetooth adapter: {}, package identity: {}, per-call controls: {}, Bluetooth phones: {}, communications mic: {}, output: {}",
                status.available,
                status.bluetooth,
                status.packaged,
                status.call_control,
                status.devices.len(),
                status.input.is_some(),
                status.output.is_some()
            );
        }
        Err(error) => eprintln!("Call diagnostics: {error}"),
    }
}
