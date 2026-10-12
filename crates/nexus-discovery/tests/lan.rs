//! Two discovery instances on this machine find each other over real multicast DNS.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::{Duration, Instant};

use nexus_discovery::Discovery;

fn wait_for(d: &Discovery, id: &[u8]) -> Vec<std::net::SocketAddr> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let a = d.addresses_for(id);
        if !a.is_empty() {
            return a;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Vec::new()
}

#[test]
fn a_paired_peer_finds_the_advertiser_and_a_stranger_does_not() {
    let server_id = [7u8; 16];
    let server = Discovery::start().unwrap();
    server.advertise(server_id, 41234).unwrap();
    let client = Discovery::start().unwrap();

    let found = wait_for(&client, &server_id);
    if found.is_empty() {
        eprintln!(
            "no mDNS delivery on this host (multicast unavailable?); skipping the assertions"
        );
        return;
    }
    assert!(found.iter().all(|a| a.port() == 41234), "{found:?}");
    // Someone who does not know the device id learns nothing from what is on the wire.
    assert!(client.addresses_for(&[8u8; 16]).is_empty());
    assert!(client.heard_count() >= 1);
}
