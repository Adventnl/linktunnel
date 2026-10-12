//! The target side of a remote-desktop session: capture, encode, send video and cursor, and
//! apply the controller's input.
//!
//! ```text
//!                   ┌── capture+encode thread ──┐   mpsc    ┌── sender task ──┐
//!  screen ─ XDamage ─┤ grab → I420 → H.264      ├──────────▶│ 1 QUIC uni stream│──▶ controller
//!                   └──────────▲───────────────┘            │   per frame     │
//!                     keyframe / bitrate / stop flags         └─────────────────┘
//!  cursor poll task ──▶ datagrams (position) + control msgs (new shapes)
//!  input stream ─▶ decode ─▶ coalesce ─▶ injector thread ─▶ OS
//! ```

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use nexus_capture::{CaptureBackend, Capturer, CursorSource, MonitorInfo};
use nexus_codec::{Encoder, EncoderConfig, Frame};
use nexus_core::clock::now_us;
use nexus_core::telemetry::LatencyRecorder;
use nexus_input::{Guarded, Injector, MonitorRect};
use nexus_net::{FramedStream, Session, StreamKind};
use nexus_proto::v1::control_message::Body;
use nexus_proto::v1::{
    AudioStarted, ControlMessage, CursorShape, Error as ProtoError, ErrorCode, Goodbye,
    GoodbyeReason, MonitorInfo as ProtoMonitor, MonitorList, Pong, PowerActionKind, PowerResult,
    StartVideo, VideoCodec, VideoStarted,
};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::input::TimedInput;
use nexus_proto::wire::video::{FrameFlags, FrameKind, VideoFrameHeader};
use nexus_trust::grants;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::MediaError;
use crate::input_wire::PacketReader;
use crate::ladder::{Content, ContentClassifier, QualityLadder};

/// Frames that may be on the wire (sent but not yet acknowledged) before the encoder pauses.
pub const DEFAULT_MAX_INFLIGHT: u32 = 12;
/// Cursor sampling period.
const CURSOR_POLL: Duration = Duration::from_millis(8);
/// How long to wait for the screen to change before looping to check flags.
const CAPTURE_TICK: Duration = Duration::from_millis(50);

/// Creates the injector for a session when the controller first sends input.
pub type InjectorFactory =
    Arc<dyn Fn() -> Result<Box<dyn Injector>, nexus_input::InputError> + Send + Sync>;

/// Opens the capture source for an audio stream with the given shape.
pub type AudioSourceFactory = Arc<
    dyn Fn(
            &nexus_audio::StreamConfig,
        ) -> Result<Box<dyn nexus_audio::AudioSource>, nexus_audio::AudioError>
        + Send
        + Sync,
>;

/// Encoder creation policy, also used to keep regression measurements on a fixed backend.
pub type EncoderFactory =
    Arc<dyn Fn(EncoderConfig) -> Result<Box<dyn Encoder>, nexus_codec::CodecError> + Send + Sync>;

/// Everything the target needs to serve a session.
#[derive(Clone)]
pub struct TargetConfig {
    /// Optional fixed encoder policy. None chooses available hardware before software.
    pub encoder: Option<EncoderFactory>,
    /// What the controller is allowed to do (`screen`, `input`, …) — from the pairing record.
    pub grants: BTreeSet<String>,
    /// Where pictures come from.
    pub capture: Arc<dyn CaptureBackend>,
    /// How to inject input; `None` makes the session view-only whatever the grants say.
    pub injector: Option<InjectorFactory>,
    /// Highest frame rate to produce.
    pub max_fps: u32,
    /// Frames in flight before the encoder pauses.
    pub max_inflight: u32,
    /// Limit and delay the video egress (a self-imposed bandwidth cap; also how the tests model a
    /// slow uplink, since a userspace proxy cannot hold QUIC on one path).
    pub egress: Option<EgressShaper>,
    /// Serves the file panel to controllers holding the `files` grant; `None` disables it.
    pub files: Option<Arc<nexus_files::FileServer>>,
    /// Clipboard sync for controllers holding the `clipboard` grant; `None` disables it.
    pub clipboard: Option<nexus_clipboard::ClipboardSetup>,
    /// Consent prompt for attended access; `None` means access is pre-authorised.
    pub consent: Option<ConsentGate>,
    /// How to lock / sleep / restart this machine for controllers holding the `power` grant;
    /// `None` refuses power requests.
    pub power: Option<Arc<dyn nexus_power::PowerBackend>>,
    /// Forwards TCP connections for controllers holding the `terminal` grant; `None` refuses.
    pub portfwd: Option<Arc<nexus_portfwd::ForwardHost>>,
    /// Where to note what the controller does on this machine (screen, input, files, ...).
    pub audit: Option<AuditContext>,
    /// Hosts terminals for controllers holding the `terminal` grant; `None` refuses them.
    pub terminal: Option<Arc<nexus_terminal::TerminalHost>>,
    /// Send Wake-on-LAN packets from this machine's network on behalf of controllers holding the
    /// `wake` grant (a device that is always on can wake the ones that sleep).
    pub wake_relay: bool,
    /// Where system audio comes from for controllers holding the `audio` grant; `None` refuses.
    pub audio: Option<AudioSourceFactory>,
    /// Physical screen privacy; unsupported backends refuse requests.
    pub privacy: Option<Arc<dyn nexus_privacy::PrivacyBackend>>,
}

/// Who is doing things on this machine, and where to write it down.
#[derive(Clone)]
pub struct AuditContext {
    /// The log.
    pub sink: Arc<dyn nexus_audit::AuditSink>,
    /// The controlling device (its ID).
    pub actor: String,
}

impl std::fmt::Debug for AuditContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditContext")
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

impl AuditContext {
    fn note(&self, kind: &str, detail: &[(&str, &str)]) {
        self.sink.record(kind, Some(&self.actor), detail);
    }
}

fn note(audit: &Option<AuditContext>, kind: &str, detail: &[(&str, &str)]) {
    if let Some(a) = audit {
        a.note(kind, detail);
    }
}

/// A serial bottleneck applied to outgoing video frames.
#[derive(Debug, Clone, Copy)]
pub struct EgressShaper {
    /// Bottleneck rate in bits per second.
    pub rate_bps: u64,
    /// Extra one-way delay added after serialisation.
    pub delay: Duration,
}

impl std::fmt::Debug for TargetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetConfig")
            .field("grants", &self.grants)
            .field("capture", &self.capture.name())
            .field("view_only", &self.injector.is_none())
            .finish_non_exhaustive()
    }
}

/// Counters a session exposes while it runs (and returns when it ends).
#[derive(Debug, Default)]
pub struct TargetStats {
    /// Frames encoded.
    pub frames: std::sync::atomic::AtomicU64,
    /// Compressed bytes produced.
    pub bytes: std::sync::atomic::AtomicU64,
    /// Capture → encoded latency.
    pub encode: LatencyRecorder,
    /// Input events injected.
    pub input_events: std::sync::atomic::AtomicU64,
    /// Frame rate the quality ladder currently runs the encoder at.
    pub fps_now: AtomicU32,
    /// Resolution scale (percent of native) currently in use.
    pub scale_now: AtomicU32,
    /// Bitrate the encoder is currently targeting.
    pub bitrate_now: AtomicU32,
}

/// The local person's answer to "may this device use this machine?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Go ahead.
    Allowed,
    /// Someone said no (or nobody was there to say yes).
    Denied,
    /// Nobody answered in time.
    TimedOut,
}

/// Asked once per session, the first time the controller does something that needs consent
/// (viewing the screen, sending input, opening a file/clipboard stream). Diagnostics such as
/// `nexus ping` never trigger it. `None` in the config means "always allowed".
pub type ConsentGate = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Consent> + Send>> + Send + Sync,
>;

/// A gate plus its memoised answer for one session.
#[derive(Clone)]
struct SessionConsent {
    gate: Option<ConsentGate>,
    answer: Arc<tokio::sync::OnceCell<Consent>>,
}

impl SessionConsent {
    fn new(gate: Option<ConsentGate>) -> Self {
        Self {
            gate,
            answer: Arc::default(),
        }
    }

    async fn check(&self) -> Consent {
        let Some(gate) = &self.gate else {
            return Consent::Allowed;
        };
        *self.answer.get_or_init(|| gate()).await
    }
}

/// Why a target session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The controller said goodbye or closed the control stream.
    ControllerLeft,
    /// The connection dropped.
    ConnectionLost(String),
    /// Told to stop by the local side.
    Stopped,
    /// The person at this machine refused (or did not answer).
    ConsentRefused(Consent),
}

/// A running video pipeline for one monitor.
struct Pipeline {
    flags: Arc<PipelineFlags>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct PipelineFlags {
    stop: AtomicBool,
    keyframe: AtomicBool,
    /// Bitrate the controller asked for.
    bitrate: AtomicU32,
    /// Frame-rate ceiling (from the controller's request and this device's limit).
    fps: AtomicU32,
    /// 0 = let the quality ladder choose; otherwise a pinned value.
    pin_fps: AtomicU32,
    /// 0 = automatic; otherwise a pinned resolution scale in percent.
    pin_scale: AtomicU32,
    inflight: AtomicU32,
}

/// `SetQuality` value that returns a pinned fps/scale to automatic.
pub const QUALITY_AUTO: u32 = u32::MAX;

impl Pipeline {
    /// Signals the encode thread to finish and joins it without blocking the async runtime.
    fn stop(&mut self) {
        self.flags.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(move || {
                    let _ = t.join();
                });
            } else {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

struct OutFrame {
    header: VideoFrameHeader,
    payload: bytes::Bytes,
    /// The pipeline's counter, released once the frame has been delivered.
    flags: Arc<PipelineFlags>,
}

/// Serves one remote-desktop session until it ends.
///
/// `control` is the stream already positioned after the `Hello` exchange.
pub async fn serve(
    session: Session,
    mut control: FramedStream,
    config: TargetConfig,
    stats: Arc<TargetStats>,
) -> EndReason {
    let can_view = config.grants.contains(grants::SCREEN);
    let can_input = config.grants.contains(grants::INPUT) && config.injector.is_some();
    let can_files = config.grants.contains(grants::FILES) && config.files.is_some();
    let can_clipboard = config.grants.contains(grants::CLIPBOARD) && config.clipboard.is_some();
    let can_terminal = config.grants.contains(grants::TERMINAL) && config.terminal.is_some();
    let can_portfwd = config.grants.contains(grants::TERMINAL) && config.portfwd.is_some();
    let consent = SessionConsent::new(config.consent.clone());
    info!(
        can_view,
        can_input, can_files, can_clipboard, "remote desktop session started"
    );

    let monitors = match config.capture.monitors() {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "cannot enumerate monitors");
            let _ = control
                .send(&goodbye(GoodbyeReason::Policy, &format!("no display: {e}")))
                .await;
            return EndReason::Stopped;
        }
    };

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ControlMessage>();
    let (frame_tx, frame_rx) = mpsc::channel::<OutFrame>(4);

    // Initial monitor list (also resent when the layout changes).
    if can_view {
        let _ = out_tx.send(monitor_list(&monitors));
    }

    let mut pipelines: HashMap<u8, Pipeline> = HashMap::new();
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    tasks.push(tokio::spawn(send_frames(
        session.clone(),
        frame_rx,
        config.egress,
    )));

    // Streams the controller opens: input, and diagnostics (`nexus ping`).
    tasks.push(tokio::spawn(accept_streams(
        session.clone(),
        config.clone(),
        monitors.clone(),
        Arc::clone(&stats),
        StreamAccess {
            input: can_input,
            files: can_files,
            clipboard: can_clipboard,
            terminal: can_terminal,
            portfwd: can_portfwd,
        },
        consent.clone(),
    )));
    // Diagnostics datagram echo, until video starts (after that datagrams carry the cursor).
    let video_active = Arc::new(AtomicBool::new(false));
    tasks.push(tokio::spawn(echo_datagrams(
        session.clone(),
        Arc::clone(&video_active),
    )));

    let mut cursor_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut audio: Option<nexus_audio::StreamHandle> = None;
    let mut closed = Box::pin(session.closed());
    let mut privacy: Option<Box<dyn nexus_privacy::PrivacyLease>> = None;
    let mut privacy_blocks_input = false;
    let mut privacy_tick = tokio::time::interval(Duration::from_secs(1));

    let reason = loop {
        tokio::select! {
            msg = control.recv::<ControlMessage>() => {
                let msg = match msg {
                    Ok(Some(m)) => m,
                    Ok(None) => break EndReason::ControllerLeft,
                    Err(e) => break EndReason::ConnectionLost(e.to_string()),
                };
                match msg.body {
                    Some(Body::Privacy(req)) => {
                        if !config.grants.contains(grants::PRIVACY) {
                            let _ = out_tx.send(error_reply(msg.id, ErrorCode::PermissionDenied, "privacy is not granted"));
                        } else if !req.enabled {
                            privacy = None;
                            privacy_blocks_input = false;
                            note(&config.audit, "privacy.disabled", &[]);
                            let _ = out_tx.send(ControlMessage { in_reply_to: msg.id, body: Some(Body::PrivacyResult(nexus_proto::v1::PrivacyResult { enabled: false, local_input_blocked: false })), ..Default::default() });
                        } else if consent.check().await != Consent::Allowed {
                            break EndReason::ConsentRefused(consent.check().await);
                        } else if let Some(backend) = config.privacy.clone() {
                            if privacy.is_some() && privacy_blocks_input != req.block_local_input {
                                let _ = out_tx.send(error_reply(msg.id, ErrorCode::InvalidRequest, "disable privacy before changing local-input policy"));
                                continue;
                            }
                            if privacy.is_none() {
                                match tokio::task::spawn_blocking(move || backend.enable(req.block_local_input)).await {
                                    Ok(Ok(lease)) => { privacy = Some(lease); privacy_blocks_input = req.block_local_input; },
                                    other => {
                                        let _ = other;
                                        debug!("privacy refused");
                                        let _ = out_tx.send(error_reply(msg.id, ErrorCode::Unsupported, "physical screen privacy could not be established"));
                                        continue;
                                    }
                                }
                            }
                            note(&config.audit, "privacy.enabled", &[]);
                            let _ = out_tx.send(ControlMessage { in_reply_to: msg.id, body: Some(Body::PrivacyResult(nexus_proto::v1::PrivacyResult { enabled: true, local_input_blocked: privacy_blocks_input })), ..Default::default() });
                        } else {
                            let _ = out_tx.send(error_reply(msg.id, ErrorCode::Unsupported, "privacy is unavailable on this target"));
                        }
                    }
                    Some(Body::StartVideo(req)) if !can_view => {
                        let _ = out_tx.send(error_reply(msg.id, ErrorCode::PermissionDenied, "screen viewing is not granted"));
                        debug!(?req.monitor, "StartVideo refused: no screen grant");
                    }
                    Some(Body::StartVideo(_)) if consent.check().await != Consent::Allowed => {
                        break EndReason::ConsentRefused(consent.check().await);
                    }
                    Some(Body::StartVideo(req)) => {
                        let id = msg.id;
                        match start_video(&config, &monitors, &req, &frame_tx, &stats, &out_tx) {
                            Ok((pipeline, started)) => {
                                note(&config.audit, "screen.started", &[("monitor", &req.monitor.to_string())]);
                                if let Some(mut old) = pipelines.insert(u8::try_from(req.monitor).unwrap_or(0), pipeline) {
                                    old.stop();
                                }
                                let _ = out_tx.send(ControlMessage { in_reply_to: id, body: Some(Body::VideoStarted(started)), ..Default::default() });
                                video_active.store(true, Ordering::SeqCst);
                                if cursor_task.is_none() {
                                    cursor_task = start_cursor(&session, &config, &monitors, req.monitor, out_tx.clone());
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "could not start video");
                                let _ = out_tx.send(error_reply(id, ErrorCode::Unsupported, &e.to_string()));
                            }
                        }
                    }
                    Some(Body::StopVideo(s)) => {
                        if let Some(mut p) = pipelines.remove(&u8::try_from(s.monitor).unwrap_or(0)) {
                            p.stop();
                        }
                    }
                    Some(Body::RequestKeyframe(r)) => {
                        if let Some(p) = pipelines.get(&u8::try_from(r.monitor).unwrap_or(0)) {
                            p.flags.keyframe.store(true, Ordering::SeqCst);
                        }
                    }
                    Some(Body::SetQuality(q)) => {
                        if let Some(p) = pipelines.get(&u8::try_from(q.monitor).unwrap_or(0)) {
                            if q.bitrate_bps != 0 {
                                p.flags.bitrate.store(q.bitrate_bps, Ordering::SeqCst);
                            }
                            match q.fps {
                                0 => {}
                                QUALITY_AUTO => p.flags.pin_fps.store(0, Ordering::SeqCst),
                                f => p.flags.pin_fps.store(f.min(config.max_fps), Ordering::SeqCst),
                            }
                            match q.scale_percent {
                                0 => {}
                                QUALITY_AUTO => p.flags.pin_scale.store(0, Ordering::SeqCst),
                                sc => p.flags.pin_scale.store(sc.clamp(25, 100), Ordering::SeqCst),
                            }
                        }
                    }
                    Some(Body::StartAudio(req)) if !config.grants.contains(grants::AUDIO) => {
                        let _ = out_tx.send(error_reply(msg.id, ErrorCode::PermissionDenied, "audio is not granted"));
                        debug!(?req, "StartAudio refused: no audio grant");
                    }
                    Some(Body::StartAudio(_)) if consent.check().await != Consent::Allowed => {
                        break EndReason::ConsentRefused(consent.check().await);
                    }
                    Some(Body::StartAudio(req)) => {
                        match start_audio(&config, &session, &req) {
                            Ok((handle, started)) => {
                                note(&config.audit, "audio.started", &[("source", &started.source)]);
                                audio = Some(handle);
                                let _ = out_tx.send(ControlMessage { in_reply_to: msg.id, body: Some(Body::AudioStarted(started)), ..Default::default() });
                            }
                            Err(reply) => { let _ = out_tx.send(error_reply(msg.id, reply.0, &reply.1)); }
                        }
                    }
                    Some(Body::StopAudio(_)) => {
                        if let Some(mut h) = audio.take() { h.stop(); }
                    }
                    Some(Body::SetAudio(set)) => {
                        if let Some(h) = &audio {
                            match set.mute { 1 => h.set_muted(true), 2 => h.set_muted(false), _ => {} }
                            if set.bitrate_bps != 0 { h.set_bitrate(set.bitrate_bps); }
                            if set.loss_percent <= 100 { h.set_expected_loss(u8::try_from(set.loss_percent).unwrap_or(100)); }
                        }
                    }
                    Some(Body::Power(req)) => {
                        let reply = handle_power(&config, &consent, msg.id, req.action).await;
                        match reply {
                            PowerOutcome::Reply(m) => { let _ = out_tx.send(m); }
                            PowerOutcome::Refused(why) => break EndReason::ConsentRefused(why),
                        }
                    }
                    Some(Body::Wake(req)) => {
                        let _ = out_tx.send(handle_wake(&config, msg.id, &req.macs));
                    }
                    Some(Body::Ping(p)) => {
                        let recv_us = now_us();
                        let _ = out_tx.send(ControlMessage {
                            in_reply_to: msg.id,
                            body: Some(Body::Pong(Pong { echo_sent_us: p.sent_us, recv_us, reply_us: now_us() })),
                            ..Default::default()
                        });
                    }
                    Some(Body::ReceiverReport(_)) => {} // consumed by congestion control (phase 4)
                    Some(Body::Goodbye(_)) => break EndReason::ControllerLeft,
                    Some(Body::Hello(_)) | Some(Body::Pong(_)) | None => {}
                    Some(other) => {
                        debug!(?other, "ignoring control message the target does not handle");
                        if msg.id != 0 {
                            let _ = out_tx.send(error_reply(msg.id, ErrorCode::Unsupported, "not supported by this target"));
                        }
                    }
                }
            }
            _ = privacy_tick.tick() => {
                if privacy.as_ref().is_some_and(|p| p.renew().is_err()) {
                    privacy = None;
                    privacy_blocks_input = false;
                    note(&config.audit, "privacy.restored_locally", &[]);
                    let _ = out_tx.send(ControlMessage { body: Some(Body::PrivacyResult(nexus_proto::v1::PrivacyResult { enabled: false, local_input_blocked: false })), ..Default::default() });
                }
            }
            Some(outgoing) = out_rx.recv() => {
                if let Err(e) = control.send(&outgoing).await {
                    break EndReason::ConnectionLost(e.to_string());
                }
            }
            reason = &mut closed => break EndReason::ConnectionLost(reason),
        }
    };

    drop(privacy);
    for (_, mut p) in pipelines.drain() {
        p.stop();
    }
    drop(frame_tx);
    if let Some(t) = cursor_task {
        t.abort();
    }
    let bye = match &reason {
        EndReason::ConsentRefused(Consent::TimedOut) => goodbye(
            GoodbyeReason::ConsentTimeout,
            "nobody approved the request in time",
        ),
        EndReason::ConsentRefused(_) => {
            goodbye(GoodbyeReason::ConsentDenied, "the request was not approved")
        }
        _ => goodbye(GoodbyeReason::Unspecified, "session ended"),
    };
    let _ = control.send(&bye).await;
    let _ = control.finish();
    if !matches!(reason, EndReason::ConnectionLost(_)) {
        // Dropping the session closes the connection at once and would discard the goodbye still
        // in flight: give the peer a moment to read it and close first.
        let _ = tokio::time::timeout(Duration::from_millis(500), &mut closed).await;
    }
    for t in tasks {
        t.abort();
    }
    info!(
        ?reason,
        frames = stats.frames.load(Ordering::Relaxed),
        "remote desktop session ended"
    );
    reason
}

#[allow(clippy::large_enum_variant)] // one value per power request, never stored
enum PowerOutcome {
    Reply(ControlMessage),
    Refused(Consent),
}

fn power_reply(in_reply_to: u64, ok: bool, detail: &str) -> ControlMessage {
    ControlMessage {
        in_reply_to,
        body: Some(Body::PowerResult(PowerResult {
            ok,
            detail: detail.to_owned(),
        })),
        ..Default::default()
    }
}

fn power_action(kind: i32) -> Option<nexus_power::PowerAction> {
    use nexus_power::PowerAction as A;
    Some(match PowerActionKind::try_from(kind).ok()? {
        PowerActionKind::Lock => A::Lock,
        PowerActionKind::Sleep => A::Sleep,
        PowerActionKind::Hibernate => A::Hibernate,
        PowerActionKind::Restart => A::Restart,
        PowerActionKind::Shutdown => A::Shutdown,
        PowerActionKind::LogOff => A::LogOff,
        PowerActionKind::Unspecified => return None,
    })
}

/// Checks the grant and the person at the machine, then performs the action *after* the reply
/// has had a moment to leave (the action usually ends the connection).
async fn handle_power(
    config: &TargetConfig,
    consent: &SessionConsent,
    id: u64,
    kind: i32,
) -> PowerOutcome {
    if !config.grants.contains(grants::POWER) {
        return PowerOutcome::Reply(error_reply(
            id,
            ErrorCode::PermissionDenied,
            "power control is not granted",
        ));
    }
    let Some(backend) = config.power.clone() else {
        return PowerOutcome::Reply(error_reply(
            id,
            ErrorCode::Unsupported,
            "this device does not accept power requests",
        ));
    };
    let Some(action) = power_action(kind) else {
        return PowerOutcome::Reply(error_reply(
            id,
            ErrorCode::InvalidRequest,
            "unknown power action",
        ));
    };
    if !backend.supported().contains(&action) {
        return PowerOutcome::Reply(power_reply(id, false, "not supported on this device"));
    }
    let answer = consent.check().await;
    if answer != Consent::Allowed {
        return PowerOutcome::Refused(answer);
    }
    info!(?action, "power request accepted");
    note(
        &config.audit,
        "power.requested",
        &[("action", &format!("{action:?}"))],
    );
    if action.drops_connection() {
        // Report success first: the machine may be gone a moment later.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Err(e) = tokio::task::spawn_blocking(move || backend.perform(action)).await {
                warn!(error = %e, "power action task failed");
            }
        });
        return PowerOutcome::Reply(power_reply(id, true, "started"));
    }
    match tokio::task::spawn_blocking(move || backend.perform(action)).await {
        Ok(Ok(())) => PowerOutcome::Reply(power_reply(id, true, "done")),
        Ok(Err(e)) => PowerOutcome::Reply(power_reply(id, false, &e.to_string())),
        Err(e) => PowerOutcome::Reply(power_reply(id, false, &e.to_string())),
    }
}

/// Most MACs one wake request may name (a request is not a way to flood a network).
const MAX_WAKE_MACS: usize = 8;

fn handle_wake(config: &TargetConfig, id: u64, macs: &[String]) -> ControlMessage {
    if !config.grants.contains(grants::WAKE) {
        return error_reply(id, ErrorCode::PermissionDenied, "wake relay is not granted");
    }
    if !config.wake_relay {
        return error_reply(
            id,
            ErrorCode::Unsupported,
            "this device does not relay wake requests",
        );
    }
    if macs.is_empty() || macs.len() > MAX_WAKE_MACS {
        return error_reply(
            id,
            ErrorCode::InvalidRequest,
            "name between 1 and 8 MAC addresses",
        );
    }
    let mut parsed = Vec::new();
    for m in macs {
        match nexus_power::parse_mac(m) {
            Ok(mac) => parsed.push(mac),
            Err(e) => return error_reply(id, ErrorCode::InvalidRequest, &e.to_string()),
        }
    }
    for mac in parsed {
        if let Err(e) = nexus_power::send_magic_packet(mac, std::net::Ipv4Addr::BROADCAST, 9) {
            return power_reply(id, false, &e.to_string());
        }
    }
    info!(count = macs.len(), "relayed Wake-on-LAN packets");
    note(
        &config.audit,
        "wake.relayed",
        &[("count", &macs.len().to_string())],
    );
    power_reply(id, true, "sent")
}

fn goodbye(reason: GoodbyeReason, detail: &str) -> ControlMessage {
    ControlMessage {
        body: Some(Body::Goodbye(Goodbye {
            reason: reason as i32,
            detail: detail.to_owned(),
        })),
        ..Default::default()
    }
}

fn error_reply(in_reply_to: u64, code: ErrorCode, message: &str) -> ControlMessage {
    ControlMessage {
        in_reply_to,
        body: Some(Body::Error(ProtoError {
            code: code as i32,
            message: message.to_owned(),
        })),
        ..Default::default()
    }
}

/// Converts capture monitors to the wire form.
#[must_use]
pub fn monitor_list(monitors: &[MonitorInfo]) -> ControlMessage {
    ControlMessage {
        body: Some(Body::MonitorList(MonitorList {
            monitors: monitors
                .iter()
                .map(|m| ProtoMonitor {
                    index: u32::from(m.index),
                    name: m.name.clone(),
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    scale: m.scale,
                    refresh_hz: m.refresh_hz,
                    primary: m.primary,
                })
                .collect(),
        })),
        ..Default::default()
    }
}

fn start_video(
    config: &TargetConfig,
    monitors: &[MonitorInfo],
    req: &StartVideo,
    frame_tx: &mpsc::Sender<OutFrame>,
    stats: &Arc<TargetStats>,
    notify: &mpsc::UnboundedSender<ControlMessage>,
) -> Result<(Pipeline, VideoStarted), MediaError> {
    let index =
        u8::try_from(req.monitor).map_err(|_| MediaError::Protocol("monitor index".into()))?;
    let info = monitors
        .iter()
        .find(|m| m.index == index)
        .ok_or_else(|| MediaError::Protocol(format!("no monitor {index}")))?
        .clone();
    let codec = req
        .codecs
        .iter()
        .filter_map(|c| VideoCodec::try_from(*c).ok())
        .find(|c| nexus_codec::supported_encode().contains(c))
        .ok_or_else(|| MediaError::Protocol("no common video codec".into()))?;
    let fps = if req.max_fps == 0 {
        config.max_fps
    } else {
        req.max_fps.min(config.max_fps)
    }
    .max(1);
    let bitrate = if req.bitrate_bps == 0 {
        default_bitrate(info.width, info.height, fps)
    } else {
        req.bitrate_bps
    };

    // H.264 needs even dimensions; capture hands us the real size, and we crop one pixel if odd.
    let (w, h) = (info.width & !1, info.height & !1);
    let enc_cfg = EncoderConfig {
        codec,
        width: w,
        height: h,
        fps: fps as f32,
        bitrate_bps: bitrate,
        screen_content: true,
        keyframe_interval: None,
    };
    let encoder_factory = config
        .encoder
        .clone()
        .unwrap_or_else(|| Arc::new(nexus_codec::create_encoder));
    let encoder = encoder_factory(enc_cfg)?;
    let encoder_name = encoder.name();
    let capturer = config.capture.open(index)?;

    let flags = Arc::new(PipelineFlags::default());
    flags.bitrate.store(bitrate, Ordering::SeqCst);
    flags.fps.store(fps, Ordering::SeqCst);
    flags.keyframe.store(true, Ordering::SeqCst);
    let thread = {
        let (flags, frame_tx, stats, notify) = (
            Arc::clone(&flags),
            frame_tx.clone(),
            Arc::clone(stats),
            notify.clone(),
        );
        let max_inflight = config.max_inflight;
        std::thread::Builder::new()
            .name(format!("encode-{index}"))
            .spawn(move || {
                encode_loop(
                    capturer,
                    encoder,
                    encoder_factory,
                    index,
                    codec,
                    flags,
                    frame_tx,
                    max_inflight,
                    stats,
                    notify,
                )
            })
            .map_err(|e| MediaError::Protocol(format!("spawning encode thread: {e}")))?
    };
    info!(
        monitor = index,
        w,
        h,
        fps,
        bitrate,
        encoder = encoder_name,
        "video started"
    );
    Ok((
        Pipeline {
            flags,
            thread: Some(thread),
        },
        VideoStarted {
            monitor: req.monitor,
            codec: codec as i32,
            width: w,
            height: h,
            fps,
            bitrate_bps: bitrate,
            encoder: encoder_name.to_owned(),
        },
    ))
}

/// A starting bitrate that keeps text crisp without flooding a slow link: ~0.1 bit/pixel/frame,
/// clamped to 1–50 Mbit/s. Congestion control adjusts from here.
#[must_use]
pub fn default_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let bits = f64::from(width) * f64::from(height) * f64::from(fps) * 0.1;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bits = bits as u64;
    u32::try_from(bits.clamp(1_000_000, 50_000_000)).unwrap_or(50_000_000)
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn encode_loop(
    mut capturer: Box<dyn Capturer>,
    mut encoder: Box<dyn Encoder>,
    encoder_factory: EncoderFactory,
    monitor: u8,
    codec: VideoCodec,
    flags: Arc<PipelineFlags>,
    frame_tx: mpsc::Sender<OutFrame>,
    max_inflight: u32,
    stats: Arc<TargetStats>,
    notify: mpsc::UnboundedSender<ControlMessage>,
) {
    let mut seq: u32 = 0;
    let mut last_seq: u32 = 0;
    let mut last_encode = Instant::now() - Duration::from_secs(1);
    let mut pending: Option<Frame> = None;
    // Native (even) size of the screen; the ladder scales relative to this.
    let native = (
        capturer.monitor().width & !1,
        capturer.monitor().height & !1,
    );
    let mut ladder = QualityLadder::new(native, flags.fps.load(Ordering::Relaxed));
    let mut classifier = ContentClassifier::default();
    let mut content = Content::Text;
    let mut cur_scale = 100u32;
    let mut cur_fps = flags.fps.load(Ordering::Relaxed).max(1);
    let mut current_bitrate = encoder.config().bitrate_bps;
    stats.fps_now.store(cur_fps, Ordering::Relaxed);
    stats.scale_now.store(100, Ordering::Relaxed);
    stats.bitrate_now.store(current_bitrate, Ordering::Relaxed);
    let mut last_plan = Instant::now();
    let mut planned_for = current_bitrate;

    while !flags.stop.load(Ordering::Relaxed) {
        // Pick up the newest picture (waiting a bit if there is nothing to send yet).
        let wait = if pending.is_some() {
            Duration::ZERO
        } else {
            CAPTURE_TICK
        };
        match capturer.next_frame(wait) {
            Ok(Some(f)) => {
                classifier.frame(dirty_fraction(&f));
                pending = Some(merge(pending.take(), f));
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, "capture failed; ending video");
                return;
            }
        }
        if pending.is_none() {
            continue;
        }

        // Re-plan quality when the bitrate moved, and periodically (content may have changed).
        let target_bitrate = flags.bitrate.load(Ordering::Relaxed).max(50_000);
        if target_bitrate != planned_for || last_plan.elapsed() > Duration::from_millis(500) {
            planned_for = target_bitrate;
            last_plan = Instant::now();
            content = classifier.class(content);
            let ceiling = flags.fps.load(Ordering::Relaxed).max(1);
            ladder = rebuild_ladder(ladder, native, ceiling);
            let rung = {
                ladder.plan(target_bitrate, content);
                ladder.current()
            };
            let pin_fps = flags.pin_fps.load(Ordering::Relaxed);
            let pin_scale = flags.pin_scale.load(Ordering::Relaxed);
            let want_fps = if pin_fps != 0 { pin_fps } else { rung.fps }
                .min(ceiling)
                .max(1);
            let want_scale = if pin_scale != 0 {
                pin_scale
            } else {
                rung.scale_percent
            };
            let (ew, eh) = ladder.size_at(want_scale);
            let resized = want_scale != cur_scale;
            if resized || want_fps != cur_fps {
                let cfg = EncoderConfig {
                    codec,
                    width: ew,
                    height: eh,
                    fps: want_fps as f32,
                    bitrate_bps: target_bitrate,
                    screen_content: true,
                    keyframe_interval: None,
                };
                match encoder_factory(cfg) {
                    Ok(e) => {
                        encoder = e;
                        current_bitrate = target_bitrate;
                        cur_scale = want_scale;
                        cur_fps = want_fps;
                        stats.fps_now.store(want_fps, Ordering::Relaxed);
                        stats.scale_now.store(want_scale, Ordering::Relaxed);
                        flags.keyframe.store(true, Ordering::SeqCst);
                        info!(
                            scale = want_scale,
                            fps = want_fps,
                            w = ew,
                            h = eh,
                            bitrate = target_bitrate,
                            ?content,
                            "quality changed"
                        );
                        let _ = notify.send(ControlMessage {
                            body: Some(Body::VideoStarted(VideoStarted {
                                monitor: u32::from(monitor),
                                codec: codec as i32,
                                width: ew,
                                height: eh,
                                fps: want_fps,
                                bitrate_bps: target_bitrate,
                                encoder: encoder.name().to_owned(),
                            })),
                            ..Default::default()
                        });
                    }
                    Err(e) => warn!(error = %e, "could not apply a quality change"),
                }
            } else if target_bitrate != current_bitrate {
                if let Err(e) = encoder.set_bitrate(target_bitrate) {
                    warn!(error = %e, "bitrate change failed");
                }
                current_bitrate = target_bitrate;
                stats.bitrate_now.store(target_bitrate, Ordering::Relaxed);
            }
        }

        // Respect the frame-rate cap, but never drop the change itself: keep it pending.
        let interval = Duration::from_secs_f64(1.0 / f64::from(cur_fps));
        let due = last_encode + interval;
        let now = Instant::now();
        if now < due {
            std::thread::sleep((due - now).min(Duration::from_millis(5)));
            continue;
        }
        // Back-pressure: do not queue more than the network can drain.
        if flags.inflight.load(Ordering::Relaxed) >= max_inflight {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        let requested = flags.keyframe.swap(false, Ordering::SeqCst);
        let Some(frame) = pending.take() else {
            continue;
        };
        let (enc_w, enc_h) = (encoder.config().width, encoder.config().height);
        let frame = crop_even(frame, native.0, native.1);
        let frame = if (enc_w, enc_h) == (frame.width, frame.height) {
            frame
        } else {
            match nexus_codec::scale::downscale_bgra(&frame, enc_w, enc_h) {
                Ok(f) => f,
                Err(e) => {
                    warn!(error = %e, "downscale failed");
                    continue;
                }
            }
        };
        match encoder.encode(&frame, requested) {
            Ok(Some(enc)) => {
                last_encode = Instant::now();
                seq = seq.wrapping_add(1).max(1);
                let kind = if enc.keyframe {
                    FrameKind::Key
                } else {
                    FrameKind::Delta
                };
                let ref_seq = if enc.keyframe { seq } else { last_seq };
                last_seq = seq;
                let mut fl = FrameFlags::SCREEN_CONTENT;
                if requested {
                    fl = fl.with(FrameFlags::REQUESTED);
                }
                let header = VideoFrameHeader {
                    codec,
                    kind,
                    monitor,
                    flags: fl,
                    width: u16::try_from(enc.width).unwrap_or(u16::MAX),
                    height: u16::try_from(enc.height).unwrap_or(u16::MAX),
                    frame_seq: seq,
                    ref_seq,
                    payload_len: u32::try_from(enc.data.len()).unwrap_or(u32::MAX),
                    capture_us: enc.captured_us,
                    send_us: now_us(),
                    dirty: if enc.keyframe {
                        Vec::new()
                    } else {
                        frame.dirty.clone()
                    },
                };
                stats.frames.fetch_add(1, Ordering::Relaxed);
                stats
                    .bytes
                    .fetch_add(enc.data.len() as u64, Ordering::Relaxed);
                stats
                    .encode
                    .record_us(header.send_us.saturating_sub(enc.captured_us));
                flags.inflight.fetch_add(1, Ordering::SeqCst);
                if frame_tx
                    .blocking_send(OutFrame {
                        header,
                        payload: enc.data,
                        flags: Arc::clone(&flags),
                    })
                    .is_err()
                {
                    return; // session is gone
                }
            }
            Ok(None) => {
                // The encoder skipped the frame to hold its bitrate; the picture is not lost, it
                // just waits for the next change. Ask for nothing special.
            }
            Err(e) => {
                warn!(error = %e, "encode failed; requesting a fresh keyframe");
                flags.keyframe.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Fraction of the screen covered by a frame's damage (1.0 when unknown).
fn dirty_fraction(frame: &Frame) -> f64 {
    if frame.dirty.is_empty() {
        return 1.0;
    }
    let total = f64::from(frame.width) * f64::from(frame.height);
    let area: f64 = frame
        .dirty
        .iter()
        .map(|r| f64::from(r.w) * f64::from(r.h))
        .sum();
    (area / total.max(1.0)).min(1.0)
}

/// Keeps the ladder's frame-rate ceiling in step with the (possibly changed) requested ceiling.
fn rebuild_ladder(ladder: QualityLadder, native: (u32, u32), ceiling: u32) -> QualityLadder {
    if ladder.max_fps() == QualityLadder::new(native, ceiling).max_fps() {
        ladder
    } else {
        QualityLadder::new(native, ceiling)
    }
}

/// Combines two pictures: the newer pixels with the union of both damage lists.
fn merge(old: Option<Frame>, mut new: Frame) -> Frame {
    if let Some(old) = old {
        if old.dirty.is_empty() || new.dirty.is_empty() {
            new.dirty.clear(); // "everything" absorbs the rest
        } else {
            let mut all = old.dirty;
            all.extend(new.dirty);
            new.dirty = nexus_capture::limit_rects(all, nexus_proto::wire::video::MAX_DIRTY_RECTS);
        }
    }
    new
}

/// H.264 needs even dimensions: trim an odd trailing row/column by adjusting the logical size
/// (the stride lets us skip the extra pixels without copying).
fn crop_even(mut frame: Frame, w: u32, h: u32) -> Frame {
    frame.width = w.min(frame.width);
    frame.height = h.min(frame.height);
    frame
}

/// Opens one QUIC unidirectional stream per frame and writes header + payload.
async fn send_frames(
    session: Session,
    mut rx: mpsc::Receiver<OutFrame>,
    shaper: Option<EgressShaper>,
) {
    let mut link_free = tokio::time::Instant::now();
    while let Some(frame) = rx.recv().await {
        let OutFrame {
            mut header,
            payload,
            flags,
        } = frame;
        // Stamped before any shaping: queueing at a bottleneck is exactly what the receiver's
        // delay-gradient estimator is meant to see.
        header.send_us = now_us();
        // The virtual bottleneck queue: when this frame finishes crossing the shaped link.
        let released_at = shaper.map(|sh| {
            let bits = (header.wire_len() + payload.len()) as f64 * 8.0;
            let start = link_free.max(tokio::time::Instant::now());
            #[allow(clippy::cast_precision_loss)]
            let serialisation = Duration::from_secs_f64(bits / sh.rate_bps.max(1) as f64);
            link_free = start + serialisation;
            link_free + sh.delay
        });
        let mut head = BytesMut::with_capacity(header.wire_len());
        if header.encode(&mut head).is_err() {
            flags.inflight.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let mut stream = match session.open_uni(StreamKind::Video).await {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "cannot open a video stream; stopping sender");
                flags.inflight.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        };
        tokio::spawn(async move {
            if let Some(at) = released_at {
                tokio::time::sleep_until(at).await;
            }
            if stream.write_all(&head).await.is_ok() && stream.write_all(&payload).await.is_ok() {
                let _ = stream.finish();
                let _ = stream.stopped().await;
            }
            flags.inflight.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn start_cursor(
    session: &Session,
    config: &TargetConfig,
    monitors: &[MonitorInfo],
    monitor: u32,
    out: mpsc::UnboundedSender<ControlMessage>,
) -> Option<tokio::task::JoinHandle<()>> {
    let source = match config.capture.cursor_source() {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(e) => {
            debug!(error = %e, "no cursor source; the cursor stays in the picture");
            return None;
        }
    };
    let mon = monitors
        .iter()
        .find(|m| u32::from(m.index) == monitor)?
        .clone();
    let session = session.clone();
    Some(tokio::spawn(async move {
        cursor_loop(session, source, mon, out).await
    }))
}

async fn cursor_loop(
    session: Session,
    source: Box<dyn CursorSource>,
    monitor: MonitorInfo,
    out: mpsc::UnboundedSender<ControlMessage>,
) {
    // The source may block on a round trip to the display server, so it runs on its own thread
    // and publishes samples through a watch channel.
    let (tx, mut rx) = watch::channel(None::<nexus_capture::CursorSample>);
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = Arc::clone(&stop);
        let mut source = source;
        std::thread::Builder::new()
            .name("cursor-poll".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match source.poll() {
                        Ok(s) => {
                            if tx.send(Some(s)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, "cursor poll failed; stopping");
                            return;
                        }
                    }
                    std::thread::sleep(CURSOR_POLL);
                }
            })
    };
    if thread.is_err() {
        return;
    }
    let mut seq: u32 = 0;
    let mut last: Option<(i32, i32, bool, u32)> = None;
    while rx.changed().await.is_ok() {
        let Some(sample) = rx.borrow_and_update().clone() else {
            continue;
        };
        // Monitor origin is in the OS's coordinate units (points on macOS); the wire format is
        // device pixels, hence the scale.
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = (
            ((sample.x - monitor.x) as f32 * monitor.scale) as i32,
            ((sample.y - monitor.y) as f32 * monitor.scale) as i32,
        );
        let inside = x >= 0
            && y >= 0
            && i64::from(x) < i64::from(monitor.width)
            && i64::from(y) < i64::from(monitor.height);
        let key = (x, y, inside && sample.visible, sample.shape_id);
        if last == Some(key) {
            continue;
        }
        let shape_changed = last.is_none_or(|l| l.3 != sample.shape_id);
        last = Some(key);
        if let Some(shape) = &sample.new_shape {
            // Shapes are rare and large: reliable control stream, before the update that uses them.
            let _ = out.send(ControlMessage {
                body: Some(Body::CursorShape(CursorShape {
                    shape_id: shape.shape_id,
                    width: shape.width,
                    height: shape.height,
                    hot_x: shape.hot_x,
                    hot_y: shape.hot_y,
                    bgra: shape.bgra.clone(),
                })),
                ..Default::default()
            });
        }
        seq = seq.wrapping_add(1);
        let update = CursorUpdate {
            visible: inside && sample.visible,
            shape_changed,
            monitor: monitor.index,
            shape_id: sample.shape_id,
            x,
            y,
            seq,
            ts_us: now_us(),
        };
        let mut buf = BytesMut::with_capacity(nexus_proto::wire::cursor::LEN);
        update.encode(&mut buf);
        if session.send_datagram(buf.freeze()).is_err() {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
}

/// Which stream kinds the controller's grants allow.
#[derive(Debug, Clone, Copy)]
struct StreamAccess {
    input: bool,
    files: bool,
    clipboard: bool,
    terminal: bool,
    portfwd: bool,
}

/// Accepts streams the controller opens until the session ends: input (each stream gets its own
/// injector so a reconnecting controller never inherits another connection's held keys) and
/// diagnostics. Anything else is closed unread.
async fn accept_streams(
    session: Session,
    config: TargetConfig,
    monitors: Vec<MonitorInfo>,
    stats: Arc<TargetStats>,
    access: StreamAccess,
    consent: SessionConsent,
) {
    let StreamAccess {
        input: can_input,
        files: can_files,
        clipboard: can_clipboard,
        terminal: can_terminal,
        portfwd: can_portfwd,
    } = access;
    loop {
        let Ok((kind, stream)) = session.accept_stream().await else {
            return;
        };
        match kind {
            StreamKind::Diagnostics => {
                tokio::spawn(nexus_net::diag::serve_stream(stream));
            }
            StreamKind::Input if can_input => {
                let Some(factory) = config.injector.clone() else {
                    continue;
                };
                let rects: Vec<MonitorRect> = monitors
                    .iter()
                    .map(|m| MonitorRect {
                        index: m.index,
                        x: m.x,
                        y: m.y,
                        width: m.width,
                        height: m.height,
                        scale: m.scale,
                    })
                    .collect();
                let (consent, stats, audit) =
                    (consent.clone(), Arc::clone(&stats), config.audit.clone());
                tokio::spawn(async move {
                    if consent.check().await != Consent::Allowed {
                        return;
                    }
                    // Each stream gets its own injector so a reconnecting controller never
                    // inherits another connection's held keys.
                    note(&audit, "input.attached", &[]);
                    match factory() {
                        Ok(injector) => run_input_stream(stream, injector, rects, stats).await,
                        Err(e) => {
                            warn!(error = %e, "input injection unavailable; session is view-only");
                        }
                    }
                });
            }
            StreamKind::Files if can_files => {
                let Some(server) = config.files.clone() else {
                    continue;
                };
                let (consent, audit) = (consent.clone(), config.audit.clone());
                tokio::spawn(async move {
                    if consent.check().await != Consent::Allowed {
                        return;
                    }
                    let mut io = nexus_files::framed(stream);
                    let observe = |op: &str, path: &str, ok: bool| {
                        note(
                            &audit,
                            &format!("files.{op}"),
                            &[("path", path), ("ok", if ok { "yes" } else { "no" })],
                        );
                    };
                    if let Err(e) = server.serve_observed(&mut io, &observe).await {
                        debug!(error = %e, "file operation ended with an error");
                    }
                });
            }
            StreamKind::Terminal if can_terminal => {
                let Some(host) = config.terminal.clone() else {
                    continue;
                };
                let (consent, audit) = (consent.clone(), config.audit.clone());
                tokio::spawn(async move {
                    if consent.check().await != Consent::Allowed {
                        return;
                    }
                    note(&audit, "terminal.opened", &[]);
                    let mut io = crate::terminal_io::QuicTerm::new(stream);
                    if let Err(e) = nexus_terminal::serve(&host, &mut io).await {
                        debug!(error = %e, "terminal ended");
                    }
                });
            }
            StreamKind::PortForward if can_portfwd => {
                let Some(host) = config.portfwd.clone() else {
                    continue;
                };
                let (consent, audit) = (consent.clone(), config.audit.clone());
                tokio::spawn(async move {
                    if consent.check().await != Consent::Allowed {
                        return;
                    }
                    let observe = |host: &str, port: u16, status: nexus_portfwd::Status| {
                        note(
                            &audit,
                            "forward.opened",
                            &[
                                ("destination", &format!("{host}:{port}")),
                                ("result", &format!("{status:?}")),
                            ],
                        );
                    };
                    host.serve_observed(tokio::io::join(stream.recv, stream.send), &observe)
                        .await;
                });
            }
            StreamKind::Clipboard if can_clipboard => {
                let Some(setup) = config.clipboard.clone() else {
                    continue;
                };
                let backend = match (setup.factory)() {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(error = %e, "clipboard unavailable on this device");
                        continue;
                    }
                };
                let (consent, audit) = (consent.clone(), config.audit.clone());
                tokio::spawn(async move {
                    if consent.check().await != Consent::Allowed {
                        return;
                    }
                    note(&audit, "clipboard.attached", &[]);
                    let io = FramedStream::new(stream, nexus_proto::framing::MAX_BULK_FRAME);
                    if let Err(e) = nexus_clipboard::run(backend, io, setup.policy).await {
                        debug!(error = %e, "clipboard sync ended");
                    }
                });
            }
            other => debug!(?other, "stream refused (not granted or not supported here)"),
        }
    }
}

/// Starts the audio pipeline for a `StartAudio` request.
fn start_audio(
    config: &TargetConfig,
    session: &Session,
    req: &nexus_proto::v1::StartAudio,
) -> Result<(nexus_audio::StreamHandle, AudioStarted), (ErrorCode, String)> {
    use nexus_audio::{FrameSize, StreamConfig};
    let Some(factory) = &config.audio else {
        return Err((
            ErrorCode::Unsupported,
            "this device cannot capture audio".into(),
        ));
    };
    let defaults = StreamConfig::default();
    let cfg = StreamConfig {
        channels: if req.channels == 0 {
            defaults.channels
        } else {
            u8::try_from(req.channels).unwrap_or(0)
        },
        frame: if req.frame_ms == 0 {
            defaults.frame
        } else {
            FrameSize::from_ms(req.frame_ms).ok_or((
                ErrorCode::InvalidRequest,
                format!("unsupported frame length {} ms", req.frame_ms),
            ))?
        },
        bitrate_bps: if req.bitrate_bps == 0 {
            defaults.bitrate_bps
        } else {
            req.bitrate_bps
        },
    };
    cfg.validate()
        .map_err(|e| (ErrorCode::InvalidRequest, e.to_string()))?;
    let source = factory(&cfg).map_err(|e| (ErrorCode::Unsupported, e.to_string()))?;
    let name = source.name().to_owned();
    let tx = session.clone();
    let handle = nexus_audio::spawn_stream(
        source,
        cfg,
        Box::new(move |datagram| {
            // A full datagram queue just drops this packet: audio is real-time, never retried.
            let _ = tx.send_datagram(datagram);
            true
        }),
    )
    .map_err(|e| (ErrorCode::Internal, e.to_string()))?;
    // 2.5 ms is reported as 2, the value `FrameSize::from_ms` accepts for it.
    let frame_ms = u32::try_from(cfg.frame.micros() / 1000).unwrap_or(10);
    Ok((
        handle,
        AudioStarted {
            channels: u32::from(cfg.channels),
            frame_ms,
            bitrate_bps: cfg.bitrate_bps,
            source: name,
        },
    ))
}

/// Echoes datagrams back while no video session is running (the diagnostics latency test).
async fn echo_datagrams(session: Session, video_active: Arc<AtomicBool>) {
    while let Ok(data) = session.recv_datagram().await {
        if video_active.load(Ordering::Relaxed) {
            continue;
        }
        if session.send_datagram(data).is_err() {
            return;
        }
    }
}

async fn run_input_stream(
    mut stream: nexus_net::Stream,
    injector: Box<dyn Injector>,
    monitors: Vec<MonitorRect>,
    stats: Arc<TargetStats>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<TimedInput>>();
    let injector_thread = std::thread::Builder::new().name("inject".into()).spawn({
        let stats = Arc::clone(&stats);
        move || {
            let mut guarded = Guarded::new(injector);
            guarded.set_monitors(&monitors);
            let mut warned_unsupported = false;
            while let Ok(first) = rx.recv() {
                // If events piled up while we were busy, merge the backlog so a slow frame does
                // not replay hundreds of stale pointer positions.
                let mut batch = first;
                while let Ok(more) = rx.try_recv() {
                    batch.extend(more);
                }
                for timed in nexus_proto::wire::input::coalesce(batch) {
                    match guarded.inject(&timed.event) {
                        Ok(()) => {
                            stats.input_events.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(nexus_input::InputError::Unsupported(what)) => {
                            if !warned_unsupported {
                                warned_unsupported = true;
                                info!(%what, "input event not supported on this platform (further occurrences suppressed)");
                            }
                        }
                        Err(e) => debug!(error = %e, "input injection failed"),
                    }
                }
            }
            // The channel closed: the stream ended. `Guarded` releases every held key on drop.
            drop(guarded);
        }
    });
    if injector_thread.is_err() {
        return;
    }
    let mut reader = PacketReader::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match stream.recv.read(&mut chunk).await {
            Ok(Some(n)) => reader.extend(&chunk[..n]),
            Ok(None) | Err(_) => break,
        }
        loop {
            match reader.next_packet() {
                Ok(Some(packet)) => {
                    if tx.send(packet.events).is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "malformed input stream; closing it");
                    return;
                }
            }
        }
    }
    // Dropping `tx` lets the injector thread release held keys and exit.
}
