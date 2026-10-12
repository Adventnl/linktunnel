//! Every protobuf message that crosses a trust boundary: decode must never panic, and a decoded
//! message must survive an encode/decode round trip.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_proto::v1;
use prost::Message;

fn check<M: Message + Default + PartialEq + std::fmt::Debug>(data: &[u8]) {
    if let Ok(m) = M::decode(data) {
        let again = M::decode(m.encode_to_vec().as_slice()).expect("re-decode");
        assert_eq!(m, again);
    }
}

fuzz_target!(|data: &[u8]| {
    // Session (peer-controlled).
    check::<v1::ControlMessage>(data);
    check::<v1::Hello>(data);
    check::<v1::MonitorList>(data);
    check::<v1::CursorShape>(data);
    check::<v1::ReceiverReport>(data);
    check::<v1::StartAudio>(data);
    check::<v1::SetAudio>(data);
    // Server-controlled.
    check::<v1::SignalServerMessage>(data);
    // Client-controlled (what the signaling server parses).
    check::<v1::SignalClientMessage>(data);
    check::<v1::SignedPairingRecord>(data);
    check::<v1::SignedRevocation>(data);
    // Peer-controlled sub-protocols.
    check::<v1::FileMessage>(data);
    check::<v1::TerminalMessage>(data);
    check::<v1::ClipboardUpdate>(data);
    // Local IPC.
    check::<v1::IpcRequest>(data);
    check::<v1::IpcFrame>(data);
});
