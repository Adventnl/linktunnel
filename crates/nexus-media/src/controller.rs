//! The controller side of a remote-desktop session (network half).
//!
//! [`ControllerSession::start`] takes an established session, asks the target for video, and
//! returns a stream of [`MediaEvent`]s: frames in decode order, cursor shapes, monitor lists.
//! Cursor *positions* arrive on a separate latest-wins channel because they must never queue
//! behind video. Input goes the other way through [`ControllerSession::send_input`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use nexus_core::clock::now_us;
use nexus_core::telemetry::{LatencyRecorder, LatencySummary};
use nexus_net::{FramedStream, Session, StreamKind};
use nexus_proto::serial::newer_u32;
use nexus_proto::v1::control_message::Body;
use nexus_proto::v1::{
    AudioStarted, ControlMessage, CursorShape, Goodbye, GoodbyeReason, MonitorInfo, Ping,
    PowerActionKind, PowerRequest, ReceiverReport, RequestKeyframe, SetAudio, SetQuality,
    StartAudio, StartVideo, StopAudio, StopVideo, VideoCodec, VideoStarted, WakeRequest,
};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::input::{InputEvent, InputPacket, MAX_EVENTS, TimedInput};
use nexus_proto::wire::video::{FIXED_LEN, FrameKind, VideoFrameHeader};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

use crate::MediaError;
use crate::input_wire;
use crate::rate::{FrameArrival, RateConfig, RateController};
use crate::reorder::{Action, Reorder, Sequenced};

/// How often the controller reports what it received.
const REPORT_INTERVAL: Duration = Duration::from_millis(250);
/// How often the control stream measures round-trip time and clock offset.
const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Longest a single frame may take to arrive in full.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames buffered between the reorder task and the consumer before the consumer counts as slow.
const EVENT_QUEUE: usize = 16;

/// What to ask the target for.
#[derive(Debug, Clone)]
pub struct ControllerOptions {
    /// Monitor to view.
    pub monitor: u32,
    /// Frame-rate cap (0 = target's default).
    pub max_fps: u32,
    /// Starting bitrate (0 = target's default).
    pub bitrate_bps: u32,
    /// Codecs this controller can decode, best first.
    pub codecs: Vec<VideoCodec>,
    /// How long a missing frame is waited for before it counts as lost.
    pub reorder_wait: Duration,
    /// Request video at all (false = control-only session, e.g. terminal).
    pub want_video: bool,
    /// Let the receiver steer the sender's bitrate from observed delay and loss.
    pub adaptive: bool,
    /// Lowest bitrate adaptation may choose.
    pub min_bitrate_bps: u32,
    /// Highest bitrate adaptation may choose.
    pub max_bitrate_bps: u32,
    /// Keep this side's clipboard in sync with the target's (needs the `clipboard` grant).
    pub clipboard: Option<nexus_clipboard::ClipboardSetup>,
    /// Key substitution applied to input before it is sent (⌘ ↔ Ctrl between a Mac and a PC).
    pub key_remap: nexus_input::remap::KeyRemap,
}

impl Default for ControllerOptions {
    fn default() -> Self {
        Self {
            monitor: 0,
            max_fps: 0,
            bitrate_bps: 0,
            codecs: nexus_codec::supported_decode(),
            reorder_wait: Duration::from_millis(60),
            want_video: true,
            adaptive: true,
            min_bitrate_bps: 300_000,
            max_bitrate_bps: 50_000_000,
            clipboard: None,
            key_remap: nexus_input::remap::KeyRemap::identity(),
        }
    }
}

/// A compressed frame as received, ready for a decoder.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// The frame header (sequence, kind, dirty rectangles, sender timestamps).
    pub header: VideoFrameHeader,
    /// Codec bitstream.
    pub payload: Bytes,
    /// Local monotonic time when the last byte arrived.
    pub recv_us: u64,
}

impl Sequenced for VideoFrame {
    fn seq(&self) -> u32 {
        self.header.frame_seq
    }

    fn is_key(&self) -> bool {
        self.header.kind == FrameKind::Key
    }
}

/// Something that happened on the session.
#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// The target's monitors.
    Monitors(Vec<MonitorInfo>),
    /// Video is flowing with these parameters.
    Started(VideoStarted),
    /// A frame, in decode order.
    Frame(VideoFrame),
    /// A new cursor bitmap.
    CursorShape(CursorShape),
    /// The target refused or failed a request.
    Refused {
        /// Machine-readable code.
        code: i32,
        /// Diagnostic text.
        message: String,
    },
    /// Target restored physical privacy locally or through its watchdog.
    Privacy(nexus_proto::v1::PrivacyResult),
    /// The session is over. Always the last event.
    Closed(String),
}

/// The channels a consumer reads.
#[derive(Debug)]
pub struct Streams {
    /// Reliable events, in order.
    pub events: mpsc::Receiver<MediaEvent>,
    /// Latest cursor position (older ones are replaced, never queued).
    pub cursor: watch::Receiver<Option<CursorUpdate>>,
}

/// Measurements for the HUD and the benchmarks.
#[derive(Debug, Default)]
pub struct ControllerStats {
    /// Frames received in full.
    pub frames_received: AtomicU64,
    /// Compressed bytes received.
    pub bytes_received: AtomicU64,
    /// Frames delivered to the consumer.
    pub frames_delivered: AtomicU64,
    /// Frames dropped (late, duplicate, desynchronised, consumer too slow).
    pub frames_dropped: AtomicU64,
    /// Gaps declared lost.
    pub losses: AtomicU64,
    /// Keyframe requests sent.
    pub keyframe_requests: AtomicU64,
    /// Round-trip time of the control channel, microseconds (latest).
    pub rtt_us: AtomicU64,
    /// Target clock minus controller clock, microseconds, as a two's-complement `i64`.
    pub clock_offset_us: AtomicU64,
    /// Time from capture on the target to last byte received here (offset-corrected).
    pub capture_to_receive: LatencyRecorder,
    /// Time from the target handing the frame to the network until it fully arrived.
    pub network: LatencyRecorder,
    /// Cursor updates received.
    pub cursor_updates: AtomicU32,
    /// The bitrate the receiver last asked the sender for (0 until adaptation starts).
    pub target_bitrate_bps: AtomicU32,
    /// Set when the session ended because the link died rather than on purpose (a viewer may
    /// reconnect); read after [`MediaEvent::Closed`].
    pub ended_by_loss: std::sync::atomic::AtomicBool,
}

impl ControllerStats {
    /// Offset of the target's clock relative to ours, microseconds.
    #[must_use]
    pub fn clock_offset(&self) -> i64 {
        i64::from_ne_bytes(self.clock_offset_us.load(Ordering::Relaxed).to_ne_bytes())
    }

    /// Capture → received latency percentiles.
    #[must_use]
    pub fn capture_to_receive_summary(&self) -> LatencySummary {
        self.capture_to_receive.summary()
    }
}

enum Command {
    Control(ControlMessage),
    /// A request whose reply (matched by `in_reply_to`) goes back through `reply`.
    Request {
        msg: ControlMessage,
        reply: oneshot::Sender<ControlMessage>,
    },
}

/// Tells the reorder task why it should forget what it has.
#[derive(Debug, Clone, Copy)]
enum Resync {
    /// The decoder choked: skip to the next keyframe.
    Desync,
    /// A different monitor is being streamed: sequence numbers start over.
    NewMonitor,
}

/// A live controller session.
#[derive(Debug, Clone)]
pub struct ControllerSession {
    session: Session,
    commands: mpsc::UnboundedSender<Command>,
    desync: mpsc::UnboundedSender<Resync>,
    input: mpsc::UnboundedSender<(u64, InputEvent)>,
    stats: Arc<ControllerStats>,
    /// The monitor being viewed; changes with [`switch_monitor`](Self::switch_monitor).
    monitor: Arc<AtomicU32>,
    /// What video was asked for, repeated when the monitor changes.
    video: Option<StartVideo>,
    key_remap: Arc<nexus_input::remap::KeyRemap>,
    next_request: Arc<AtomicU64>,
    /// Where received audio datagrams go; set by [`start_audio`](Self::start_audio).
    audio: AudioSlot,
}

/// The player that audio datagrams are pushed into (empty until audio is started).
type AudioSlot = Arc<Mutex<Option<Arc<Mutex<nexus_audio::Player>>>>>;

impl ControllerSession {
    /// Starts the session machinery on an established session and (if `want_video`) asks the
    /// target to start streaming. Look for [`MediaEvent::Started`] or [`MediaEvent::Refused`].
    pub async fn start(
        session: Session,
        control: FramedStream,
        options: ControllerOptions,
    ) -> Result<(Self, Streams), MediaError> {
        let stats = Arc::new(ControllerStats::default());
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (cursor_tx, cursor_rx) = watch::channel(None);
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (frames_tx, frames_rx) = mpsc::unbounded_channel::<VideoFrame>();
        let (desync_tx, desync_rx) = mpsc::unbounded_channel::<Resync>();
        let monitor = Arc::new(AtomicU32::new(options.monitor));
        let (arrivals_tx, arrivals_rx) = mpsc::unbounded_channel::<FrameArrival>();

        let video = options.want_video.then(|| StartVideo {
            monitor: options.monitor,
            codecs: options.codecs.iter().map(|c| *c as i32).collect(),
            max_fps: options.max_fps,
            bitrate_bps: options.bitrate_bps,
            max_width: 0,
            max_height: 0,
        });
        if let Some(start) = video.clone() {
            let _ = commands_tx.send(Command::Control(ControlMessage {
                id: 1,
                body: Some(Body::StartVideo(start)),
                ..Default::default()
            }));
        }

        let rate_cfg = options
            .adaptive
            .then_some((options.min_bitrate_bps, options.max_bitrate_bps));
        tokio::spawn(control_task(
            control,
            commands_rx,
            arrivals_rx,
            events_tx.clone(),
            Arc::clone(&stats),
            Arc::clone(&monitor),
            session.clone(),
            rate_cfg,
        ));
        tokio::spawn(video_receiver(
            session.clone(),
            frames_tx,
            arrivals_tx,
            Arc::clone(&stats),
        ));
        tokio::spawn(reorder_task(
            frames_rx,
            desync_rx,
            events_tx.clone(),
            commands_tx.clone(),
            Arc::clone(&stats),
            Arc::clone(&monitor),
            options.reorder_wait,
        ));
        let audio: AudioSlot = Arc::default();
        tokio::spawn(cursor_receiver(
            session.clone(),
            cursor_tx,
            Arc::clone(&stats),
            Arc::clone(&audio),
        ));
        tokio::spawn(input_task(session.clone(), input_rx));
        if let Some(setup) = options.clipboard.clone() {
            tokio::spawn(clipboard_task(session.clone(), setup));
        }

        Ok((
            Self {
                session,
                commands: commands_tx,
                desync: desync_tx,
                input: input_tx,
                stats,
                monitor,
                video,
                key_remap: Arc::new(options.key_remap),
                next_request: Arc::new(AtomicU64::new(1000)),
                audio,
            },
            Streams {
                events: events_rx,
                cursor: cursor_rx,
            },
        ))
    }

    /// Statistics shared with the background tasks.
    #[must_use]
    pub fn stats(&self) -> &Arc<ControllerStats> {
        &self.stats
    }

    /// The underlying transport session (path information, close).
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The target's file panel (needs the `files` grant and a shared folder on the target).
    #[must_use]
    pub fn files(&self) -> nexus_files::FilesClient<Session> {
        nexus_files::FilesClient::new(self.session.clone())
    }

    /// Opens a terminal on the target (needs the `terminal` grant). Commands go in through the
    /// returned sender, output and the exit status come out of the receiver.
    pub async fn terminal(
        &self,
        open: nexus_proto::v1::TermOpen,
    ) -> Result<
        (
            mpsc::UnboundedSender<nexus_terminal::TermCmd>,
            mpsc::Receiver<nexus_terminal::TermEvent>,
        ),
        MediaError,
    > {
        let stream = self.session.open_stream(StreamKind::Terminal).await?;
        let io = crate::terminal_io::QuicTerm::new(stream);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::channel(256);
        tokio::spawn(async move {
            if let Err(e) = nexus_terminal::run_client(io, open, cmd_rx, ev_tx.clone()).await {
                let _ = ev_tx
                    .send(nexus_terminal::TermEvent::Failed(e.to_string()))
                    .await;
            }
        });
        Ok((cmd_tx, ev_rx))
    }

    /// Opens one stream for a forwarded TCP connection (see `nexus_portfwd::open_forward`).
    pub async fn forward_stream(
        &self,
    ) -> Result<tokio::io::Join<nexus_net::RecvStream, nexus_net::SendStream>, MediaError> {
        let stream = self.session.open_stream(StreamKind::PortForward).await?;
        Ok(tokio::io::join(stream.recv, stream.send))
    }

    /// Queues input events, in order. Cheap and non-blocking; the input task batches them.
    pub fn send_input(&self, events: impl IntoIterator<Item = InputEvent>) {
        let now = now_us();
        for mut e in events {
            self.key_remap.apply(&mut e);
            let _ = self.input.send((now, e));
        }
    }

    /// Asks the target for a recovery frame (the decoder choked) and drops frames until it arrives.
    pub fn request_keyframe(&self) {
        let _ = self.desync.send(Resync::Desync);
    }

    /// The monitor currently being viewed.
    #[must_use]
    pub fn monitor(&self) -> u32 {
        self.monitor.load(Ordering::Relaxed)
    }

    /// Switches the stream to another monitor without ending the session. The target answers
    /// with a [`MediaEvent::Started`] describing the new picture; frames of the old monitor that
    /// are still in flight are discarded.
    pub fn switch_monitor(&self, monitor: u32) {
        let Some(template) = self.video.clone() else {
            return;
        };
        let old = self.monitor.swap(monitor, Ordering::Relaxed);
        if old == monitor {
            return;
        }
        let _ = self.desync.send(Resync::NewMonitor);
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::StopVideo(StopVideo { monitor: old })),
            ..Default::default()
        }));
        let _ = self.commands.send(Command::Control(ControlMessage {
            id: 2,
            body: Some(Body::StartVideo(StartVideo {
                monitor,
                ..template
            })),
            ..Default::default()
        }));
    }

    /// Changes quality settings (0 = unchanged).
    pub fn set_quality(&self, bitrate_bps: u32, fps: u32, scale_percent: u32) {
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::SetQuality(SetQuality {
                monitor: self.monitor.load(Ordering::Relaxed),
                bitrate_bps,
                fps,
                scale_percent,
            })),
            ..Default::default()
        }));
    }

    /// Enables session-scoped privacy on the target; it is restored automatically on disconnect.
    pub async fn privacy(
        &self,
        enabled: bool,
        block_local_input: bool,
    ) -> Result<nexus_proto::v1::PrivacyResult, MediaError> {
        let reply = self
            .request(
                Body::Privacy(nexus_proto::v1::PrivacyRequest {
                    enabled,
                    block_local_input,
                }),
                Duration::from_secs(10),
            )
            .await?;
        match reply.body {
            Some(Body::PrivacyResult(result)) => Ok(result),
            Some(Body::Error(e)) => Err(MediaError::Denied(e.message)),
            _ => Err(MediaError::Protocol(
                "target did not confirm privacy".into(),
            )),
        }
    }

    /// Sends a control request and waits for the matching reply.
    pub async fn request(&self, body: Body, wait: Duration) -> Result<ControlMessage, MediaError> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Request {
                msg: ControlMessage {
                    id,
                    body: Some(body),
                    ..Default::default()
                },
                reply: tx,
            })
            .map_err(|_| MediaError::Closed("the session has ended".into()))?;
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(MediaError::Closed(
                "the session ended before a reply".into(),
            )),
            Err(_) => Err(MediaError::Protocol(
                "the device did not answer in time".into(),
            )),
        }
    }

    /// Asks the target to lock, sleep, restart, ... (needs the `power` grant; the person at the
    /// target may be asked). `Ok(())` means the target accepted and started it.
    pub async fn power(&self, action: PowerActionKind) -> Result<(), MediaError> {
        // Allow for the consent prompt's timeout.
        let reply = self
            .request(
                Body::Power(PowerRequest {
                    action: action as i32,
                }),
                Duration::from_secs(120),
            )
            .await?;
        match reply.body {
            Some(Body::PowerResult(r)) if r.ok => Ok(()),
            Some(Body::PowerResult(r)) => Err(MediaError::Denied(r.detail)),
            Some(Body::Error(e)) => Err(MediaError::Denied(e.message)),
            _ => Err(MediaError::Protocol(
                "unexpected reply to a power request".into(),
            )),
        }
    }

    /// Asks this (always-on) target to send Wake-on-LAN packets for `macs` on its network.
    pub async fn wake_relay(&self, macs: Vec<String>) -> Result<(), MediaError> {
        let reply = self
            .request(Body::Wake(WakeRequest { macs }), Duration::from_secs(15))
            .await?;
        match reply.body {
            Some(Body::PowerResult(r)) if r.ok => Ok(()),
            Some(Body::PowerResult(r)) => Err(MediaError::Denied(r.detail)),
            Some(Body::Error(e)) => Err(MediaError::Denied(e.message)),
            _ => Err(MediaError::Protocol(
                "unexpected reply to a wake request".into(),
            )),
        }
    }

    /// Asks the target to stream its audio (needs the `audio` grant) and returns the player the
    /// received packets are fed into. Attach it to an output device (for example
    /// `nexus_audio::device::DevicePlayback`) or pull samples from it with `fill`.
    pub async fn start_audio(
        &self,
        request: StartAudio,
    ) -> Result<(Arc<Mutex<nexus_audio::Player>>, AudioStarted), MediaError> {
        let reply = self
            .request(Body::StartAudio(request), Duration::from_secs(120))
            .await?;
        let started = match reply.body {
            Some(Body::AudioStarted(s)) => s,
            Some(Body::Error(e)) => return Err(MediaError::Denied(e.message)),
            _ => {
                return Err(MediaError::Protocol(
                    "unexpected reply to an audio request".into(),
                ));
            }
        };
        let cfg = nexus_audio::StreamConfig {
            channels: u8::try_from(started.channels).unwrap_or(0),
            frame: nexus_audio::FrameSize::from_ms(started.frame_ms).ok_or_else(|| {
                MediaError::Protocol("the target chose an unknown audio frame size".into())
            })?,
            bitrate_bps: started.bitrate_bps,
        };
        let player = nexus_audio::Player::new(cfg)
            .map_err(|e| MediaError::Protocol(format!("cannot play the audio stream: {e}")))?;
        let player = Arc::new(Mutex::new(player));
        if let Ok(mut slot) = self.audio.lock() {
            *slot = Some(Arc::clone(&player));
        }
        Ok((player, started))
    }

    /// Stops the audio stream (the session continues).
    pub fn stop_audio(&self) {
        if let Ok(mut slot) = self.audio.lock() {
            *slot = None;
        }
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::StopAudio(StopAudio {})),
            ..Default::default()
        }));
    }

    /// Mutes or unmutes at the source, or reports loss so the target adds redundancy.
    pub fn set_audio(&self, set: SetAudio) {
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::SetAudio(set)),
            ..Default::default()
        }));
    }

    /// Stops video without ending the session.
    pub fn stop_video(&self) {
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::StopVideo(StopVideo {
                monitor: self.monitor.load(Ordering::Relaxed),
            })),
            ..Default::default()
        }));
    }

    /// Says goodbye and closes the connection.
    pub async fn close(&self) {
        let _ = self.input.send((now_us(), InputEvent::ReleaseAll));
        let _ = self.commands.send(Command::Control(ControlMessage {
            body: Some(Body::Goodbye(Goodbye {
                reason: GoodbyeReason::ControllerDisconnect as i32,
                detail: String::new(),
            })),
            ..Default::default()
        }));
        // Give the goodbye and the release-all a moment to leave before the connection closes.
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.session.close_normal();
    }
}

#[allow(clippy::too_many_arguments)]
async fn control_task(
    mut control: FramedStream,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut arrivals: mpsc::UnboundedReceiver<FrameArrival>,
    events: mpsc::Sender<MediaEvent>,
    stats: Arc<ControllerStats>,
    monitor: Arc<AtomicU32>,
    session: Session,
    rate_limits: Option<(u32, u32)>,
) {
    // Created when the target announces its starting bitrate.
    let mut rate: Option<RateController> = None;
    let mut last_loss = (0u64, 0u64);
    let mut ping = tokio::time::interval(PING_INTERVAL);
    let mut report = tokio::time::interval(REPORT_INTERVAL);
    let mut samples: VecDeque<(u64, i64)> = VecDeque::with_capacity(10); // (rtt, offset)
    let mut last_report = ReportState::default();
    let mut closed = Box::pin(session.closed_detail());
    // Until proven deliberate (goodbye, revocation, our own close), an ending is a lost link.
    let mut lost = true;
    let mut waiting: HashMap<u64, oneshot::Sender<ControlMessage>> = HashMap::new();
    let reason = loop {
        tokio::select! {
            msg = control.recv::<ControlMessage>() => {
                let msg = match msg {
                    Ok(Some(m)) => m,
                    Ok(None) => break "target closed the control stream".to_owned(),
                    Err(e) => break e.to_string(),
                };
                if msg.in_reply_to != 0 && let Some(tx) = waiting.remove(&msg.in_reply_to) {
                    let _ = tx.send(msg);
                    continue;
                }
                match msg.body {
                    Some(Body::MonitorList(l)) => { let _ = events.send(MediaEvent::Monitors(l.monitors)).await; }
                    Some(Body::VideoStarted(v)) => {
                        if let (Some((min_bps, max_bps)), None) = (rate_limits, &rate) {
                            rate = Some(RateController::new(RateConfig {
                                min_bps,
                                max_bps,
                                start_bps: v.bitrate_bps.clamp(min_bps, max_bps),
                            }));
                        } let _ = events.send(MediaEvent::Started(v)).await; }
                    Some(Body::PrivacyResult(state)) => { let _ = events.send(MediaEvent::Privacy(state)).await; }
                    Some(Body::CursorShape(c)) => { let _ = events.send(MediaEvent::CursorShape(c)).await; }
                    Some(Body::Error(e)) => { let _ = events.send(MediaEvent::Refused { code: e.code, message: e.message }).await; }
                    Some(Body::Pong(p)) => {
                        let t3 = now_us();
                        let rtt = t3.saturating_sub(p.echo_sent_us).saturating_sub(p.reply_us.saturating_sub(p.recv_us));
                        #[allow(clippy::cast_possible_wrap)]
                        let offset = ((p.recv_us as i64 - p.echo_sent_us as i64) + (p.reply_us as i64 - t3 as i64)) / 2;
                        if samples.len() == 10 { samples.pop_front(); }
                        samples.push_back((rtt, offset));
                        // The lowest-RTT sample has the least queueing noise in its offset.
                        if let Some((best_rtt, best_off)) = samples.iter().min_by_key(|s| s.0) {
                            stats.rtt_us.store(rtt, Ordering::Relaxed);
                            stats.clock_offset_us.store(u64::from_ne_bytes(best_off.to_ne_bytes()), Ordering::Relaxed);
                            let _ = best_rtt;
                        }
                    }
                    Some(Body::Goodbye(g)) => {
                        lost = false;
                        break format!("target ended the session: {}", g.detail);
                    }
                    _ => {}
                }
            }
            Some(arrival) = arrivals.recv() => {
                if let Some(rc) = rate.as_mut() {
                    rc.on_frame(arrival);
                    stats.target_bitrate_bps.store(rc.target_bps(), Ordering::Relaxed);
                    if let Some(bps) = rc.poll() {
                        let msg = ControlMessage {
                            body: Some(Body::SetQuality(SetQuality { monitor: monitor.load(Ordering::Relaxed), bitrate_bps: bps, fps: 0, scale_percent: 0 })),
                            ..Default::default()
                        };
                        if control.send(&msg).await.is_err() { break "control stream failed".to_owned(); }
                    }
                }
            }
            Some(cmd) = commands.recv() => {
                let msg = match cmd {
                    Command::Control(msg) => msg,
                    Command::Request { msg, reply } => {
                        waiting.insert(msg.id, reply);
                        msg
                    }
                };
                if control.send(&msg).await.is_err() { break "control stream failed".to_owned(); }
            }
            _ = ping.tick() => {
                let msg = ControlMessage { body: Some(Body::Ping(Ping { sent_us: now_us() })), ..Default::default() };
                if control.send(&msg).await.is_err() { break "control stream failed".to_owned(); }
            }
            _ = report.tick() => {
                if let Some(rc) = rate.as_mut() {
                    let received = stats.frames_received.load(Ordering::Relaxed);
                    let lost = stats.losses.load(Ordering::Relaxed);
                    let (d_recv, d_lost) = (received - last_loss.0, lost.saturating_sub(last_loss.1));
                    last_loss = (received, lost);
                    #[allow(clippy::cast_precision_loss)]
                    let now_ms = nexus_core::clock::now_us() as f64 / 1000.0;
                    rc.on_loss(u32::try_from(d_lost).unwrap_or(u32::MAX), u32::try_from(d_recv + d_lost).unwrap_or(u32::MAX), now_ms);
                    if let Some(bps) = rc.poll() {
                        let msg = ControlMessage {
                            body: Some(Body::SetQuality(SetQuality { monitor: monitor.load(Ordering::Relaxed), bitrate_bps: bps, fps: 0, scale_percent: 0 })),
                            ..Default::default()
                        };
                        if control.send(&msg).await.is_err() { break "control stream failed".to_owned(); }
                    }
                }
                if let Some(r) = last_report.build(&stats, monitor.load(Ordering::Relaxed)) {
                    let msg = ControlMessage { body: Some(Body::ReceiverReport(r)), ..Default::default() };
                    if control.send(&msg).await.is_err() { break "control stream failed".to_owned(); }
                }
            }
            (reason, deliberate) = &mut closed => {
                lost = !deliberate;
                break reason;
            }
        }
    };
    if lost && session.is_closed() {
        let (_, deliberate) = session.closed_detail().await;
        lost = !deliberate;
    }
    stats.ended_by_loss.store(lost, Ordering::SeqCst);
    let _ = events.send(MediaEvent::Closed(reason)).await;
}

#[derive(Default)]
struct ReportState {
    received: u64,
    delivered: u64,
    dropped: u64,
    bytes: u64,
    at: Option<Instant>,
}

impl ReportState {
    fn build(&mut self, stats: &ControllerStats, monitor: u32) -> Option<ReceiverReport> {
        let received = stats.frames_received.load(Ordering::Relaxed);
        let delivered = stats.frames_delivered.load(Ordering::Relaxed);
        let dropped = stats.frames_dropped.load(Ordering::Relaxed);
        let bytes = stats.bytes_received.load(Ordering::Relaxed);
        let now = Instant::now();
        let elapsed = self.at.map(|t| now.duration_since(t));
        let report = (received != self.received).then(|| {
            let secs = elapsed.map_or(0.25, |d| d.as_secs_f64()).max(0.001);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let goodput = ((bytes - self.bytes) as f64 * 8.0 / secs) as u32;
            ReceiverReport {
                monitor,
                received: u32::try_from(received - self.received).unwrap_or(u32::MAX),
                decoded: u32::try_from(delivered - self.delivered).unwrap_or(u32::MAX),
                dropped: u32::try_from(dropped - self.dropped).unwrap_or(u32::MAX),
                highest_seq: 0,
                delay_gradient_us: 0,
                decode_us: 0,
                goodput_bps: goodput,
            }
        });
        self.received = received;
        self.delivered = delivered;
        self.dropped = dropped;
        self.bytes = bytes;
        self.at = Some(now);
        report
    }
}

/// Accepts the target's per-frame unidirectional streams.
async fn video_receiver(
    session: Session,
    frames: mpsc::UnboundedSender<VideoFrame>,
    arrivals: mpsc::UnboundedSender<FrameArrival>,
    stats: Arc<ControllerStats>,
) {
    loop {
        let (kind, recv) = match session.accept_uni().await {
            Ok(s) => s,
            Err(_) => return,
        };
        if kind != StreamKind::Video {
            debug!(?kind, "ignoring unexpected unidirectional stream");
            continue;
        }
        let (frames, arrivals, stats) = (frames.clone(), arrivals.clone(), Arc::clone(&stats));
        tokio::spawn(async move {
            match tokio::time::timeout(FRAME_TIMEOUT, read_frame(recv)).await {
                Ok(Ok(frame)) => {
                    stats.frames_received.fetch_add(1, Ordering::Relaxed);
                    stats
                        .bytes_received
                        .fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
                    let offset = stats.clock_offset();
                    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
                    {
                        let local_capture = frame.header.capture_us as i64 - offset;
                        let local_send = frame.header.send_us as i64 - offset;
                        stats
                            .capture_to_receive
                            .record_us((frame.recv_us as i64 - local_capture).max(0) as u64);
                        stats
                            .network
                            .record_us((frame.recv_us as i64 - local_send).max(0) as u64);
                    }
                    let _ = arrivals.send(FrameArrival {
                        send_us: frame.header.send_us,
                        recv_us: frame.recv_us,
                        bytes: frame.payload.len(),
                        is_key: frame.header.kind == FrameKind::Key,
                    });
                    let _ = frames.send(frame);
                }
                Ok(Err(e)) => debug!(error = %e, "discarding a damaged video frame"),
                Err(_) => debug!("a video frame took too long to arrive; discarding"),
            }
        });
    }
}

async fn read_frame(mut recv: nexus_net::RecvStream) -> Result<VideoFrame, MediaError> {
    let mut fixed = [0u8; FIXED_LEN];
    recv.read_exact(&mut fixed)
        .await
        .map_err(|e| MediaError::Protocol(format!("frame header: {e}")))?;
    let extra = VideoFrameHeader::extra_len(&fixed)
        .map_err(|e| MediaError::Protocol(format!("frame header: {e}")))?;
    let mut head = Vec::with_capacity(FIXED_LEN + extra);
    head.extend_from_slice(&fixed);
    if extra > 0 {
        head.resize(FIXED_LEN + extra, 0);
        recv.read_exact(&mut head[FIXED_LEN..])
            .await
            .map_err(|e| MediaError::Protocol(format!("frame header: {e}")))?;
    }
    let header = VideoFrameHeader::decode(&head)
        .map_err(|e| MediaError::Protocol(format!("frame header: {e}")))?;
    let mut payload = vec![0u8; header.payload_len as usize];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| MediaError::Protocol(format!("frame payload: {e}")))?;
    Ok(VideoFrame {
        header,
        payload: Bytes::from(payload),
        recv_us: now_us(),
    })
}

async fn reorder_task(
    mut frames: mpsc::UnboundedReceiver<VideoFrame>,
    mut desync_rx: mpsc::UnboundedReceiver<Resync>,
    events: mpsc::Sender<MediaEvent>,
    commands: mpsc::UnboundedSender<Command>,
    stats: Arc<ControllerStats>,
    monitor: Arc<AtomicU32>,
    max_wait: Duration,
) {
    let mut reorder: Reorder<VideoFrame> = Reorder::new(max_wait);
    loop {
        let sleep_for = reorder
            .next_deadline()
            .map_or(Duration::from_secs(3600), |d| {
                d.saturating_duration_since(Instant::now())
            });
        let mut need_desync = false;
        let actions = tokio::select! {
            frame = frames.recv() => match frame {
                Some(f) if u32::from(f.header.monitor) != monitor.load(Ordering::Relaxed) => {
                    // A straggler from the monitor we just left.
                    Vec::new()
                }
                Some(f) => reorder.push(f, Instant::now()),
                None => return,
            },
            Some(why) = desync_rx.recv() => {
                match why {
                    // The decoder choked: drop everything until a keyframe and ask for one.
                    Resync::Desync => need_desync = true,
                    Resync::NewMonitor => reorder = Reorder::new(max_wait),
                }
                Vec::new()
            }
            () = tokio::time::sleep(sleep_for) => reorder.poll(Instant::now()),
        };
        for action in actions {
            match action {
                Action::Deliver(frame) => {
                    if events.try_send(MediaEvent::Frame(frame)).is_err() {
                        // The consumer cannot keep up. Frames are interdependent, so skipping one
                        // means skipping until the next keyframe.
                        stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                        need_desync = true;
                    } else {
                        stats.frames_delivered.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Action::RequestKeyframe { last_good } => {
                    stats.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                    let _ = commands.send(Command::Control(ControlMessage {
                        body: Some(Body::RequestKeyframe(RequestKeyframe {
                            monitor: monitor.load(Ordering::Relaxed),
                            last_good_seq: last_good,
                        })),
                        ..Default::default()
                    }));
                }
            }
        }
        if need_desync {
            for action in reorder.desync(Instant::now()) {
                if let Action::RequestKeyframe { last_good } = action {
                    stats.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                    let _ = commands.send(Command::Control(ControlMessage {
                        body: Some(Body::RequestKeyframe(RequestKeyframe {
                            monitor: monitor.load(Ordering::Relaxed),
                            last_good_seq: last_good,
                        })),
                        ..Default::default()
                    }));
                }
            }
        }
        let s = reorder.stats();
        stats.losses.store(s.losses, Ordering::Relaxed);
        stats.frames_dropped.fetch_max(s.dropped, Ordering::Relaxed);
    }
}

async fn cursor_receiver(
    session: Session,
    tx: watch::Sender<Option<CursorUpdate>>,
    stats: Arc<ControllerStats>,
    audio: AudioSlot,
) {
    loop {
        let datagram = match session.recv_datagram().await {
            Ok(d) => d,
            Err(_) => return,
        };
        if nexus_proto::wire::audio::is_audio(&datagram) {
            let player = audio.lock().ok().and_then(|slot| slot.clone());
            if let Some(p) = player
                && let Ok(mut p) = p.lock()
            {
                p.push(&datagram, now_us());
            }
            continue;
        }
        let Ok(update) = CursorUpdate::decode(&datagram) else {
            continue;
        };
        stats.cursor_updates.fetch_add(1, Ordering::Relaxed);
        // Datagrams can arrive out of order; only ever move forward.
        tx.send_if_modified(|cur| match cur {
            Some(old) if !newer_u32(update.seq, old.seq) => false,
            _ => {
                *cur = Some(update);
                true
            }
        });
    }
}

async fn input_task(session: Session, mut rx: mpsc::UnboundedReceiver<(u64, InputEvent)>) {
    let mut stream: Option<nexus_net::Stream> = None;
    let mut seq: u32 = 0;
    while let Some(first) = rx.recv().await {
        // Batch everything that is already waiting into one packet (up to the packet limit).
        let mut batch = vec![first];
        while batch.len() < MAX_EVENTS {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        if stream.is_none() {
            match session.open_stream(StreamKind::Input).await {
                Ok(s) => stream = Some(s),
                Err(e) => {
                    debug!(error = %e, "cannot open the input stream");
                    return;
                }
            }
        }
        let base = batch[0].0;
        let events: Vec<TimedInput> = batch
            .into_iter()
            .map(|(t, event)| TimedInput {
                dt_us: u32::try_from(t.saturating_sub(base)).unwrap_or(u32::MAX),
                event,
            })
            .collect();
        let count = u32::try_from(events.len()).unwrap_or(0);
        let packet = InputPacket {
            seq,
            base_us: base,
            events,
            skipped: 0,
        };
        seq = seq.wrapping_add(count);
        match input_wire::encode(&packet) {
            Ok(bytes) => {
                if let Some(s) = stream.as_mut()
                    && s.send.write_all(&bytes).await.is_err()
                {
                    return;
                }
            }
            Err(e) => warn!(error = %e, "dropping an input packet that could not be encoded"),
        }
    }
}

/// Opens the clipboard stream and syncs until the session ends. A target without the grant
/// (or without a clipboard) just closes the stream; that is not an error worth surfacing.
async fn clipboard_task(session: Session, setup: nexus_clipboard::ClipboardSetup) {
    let backend = match (setup.factory)() {
        Ok(b) => b,
        Err(e) => {
            debug!(error = %e, "no local clipboard; sync disabled");
            return;
        }
    };
    let stream = match session.open_stream(StreamKind::Clipboard).await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "could not open the clipboard stream");
            return;
        }
    };
    let io = FramedStream::new(stream, nexus_proto::framing::MAX_BULK_FRAME);
    if let Err(e) = nexus_clipboard::run(backend, io, setup.policy).await {
        debug!(error = %e, "clipboard sync ended");
    }
}
