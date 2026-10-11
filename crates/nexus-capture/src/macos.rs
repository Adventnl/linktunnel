//! macOS screen capture with ScreenCaptureKit.
//!
//! **Verification status:** type-checked for `aarch64-apple-darwin`; not run here (docs/STATUS.md).
//!
//! * Needs the **Screen Recording** permission. [`MacBackend::new`] preflights it and returns a
//!   clear error (with the System Settings location) instead of capturing black frames.
//! * SCK only delivers a frame when the content changed (idle frames carry no image), so a static
//!   desktop costs nothing, like the XDamage backend.
//! * The hardware cursor is *not* composited (`showsCursor = false`); it travels on the cursor
//!   channel and the controller draws it locally.
//! * Monitor `x`/`y` are in **points**, `width`/`height` in **pixels**, and `scale` is pixels per
//!   point (2.0 on Retina), matching what `nexus-input` expects.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use bytes::Bytes;
use dispatch2::{DispatchQueue, DispatchRetained};
use nexus_codec::{Frame, FrameFormat};
use nexus_core::clock::now_us;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGEvent, CGMainDisplayID};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};

use crate::CaptureError;
use crate::types::{CaptureBackend, Capturer, CursorSample, CursorSource, MonitorInfo};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

const PERMISSION_HELP: &str = "grant Nexus the Screen Recording permission in System Settings → Privacy & Security → Screen & System Audio Recording, then restart the agent";

/// Whether this process may capture the screen.
#[must_use]
pub fn screen_recording_granted() -> bool {
    // SAFETY: no arguments, no preconditions.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Triggers the system permission prompt (once per app); returns the current state.
#[must_use]
pub fn request_screen_recording() -> bool {
    // SAFETY: no arguments, no preconditions.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// ScreenCaptureKit backend.
#[derive(Debug)]
pub struct MacBackend;

impl MacBackend {
    /// Creates the backend, checking the Screen Recording permission.
    pub fn new() -> Result<Self, CaptureError> {
        if !screen_recording_granted() {
            let _ = request_screen_recording();
            return Err(CaptureError::Permission(PERMISSION_HELP.into()));
        }
        Ok(Self)
    }
}

/// Plain data about a display, extracted inside the SCK completion handler.
#[derive(Debug, Clone)]
struct DisplayData {
    id: u32,
    x: f64,
    y: f64,
    w_pts: f64,
    h_pts: f64,
    scale: f64,
}

/// Fetches the shareable displays synchronously.
fn fetch_displays() -> Result<Vec<DisplayData>, CaptureError> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if content.is_null() {
                // SAFETY: when non-null, `error` is a valid NSError for the duration of the callback.
                let msg = unsafe { error.as_ref() }.map_or_else(
                    || "unknown error".to_owned(),
                    |e| e.localizedDescription().to_string(),
                );
                Err(msg)
            } else {
                // SAFETY: non-null content object valid for the callback's duration.
                let content = unsafe { &*content };
                // SAFETY: plain property reads on live objects.
                let displays = unsafe { content.displays() };
                let mut out = Vec::new();
                for display in displays.iter() {
                    // SAFETY: property reads on a live SCDisplay; the filter is created and dropped here.
                    unsafe {
                        let frame = display.frame();
                        let empty = NSArray::new();
                        let filter = SCContentFilter::initWithDisplay_excludingWindows(
                            SCContentFilter::alloc(),
                            &display,
                            &empty,
                        );
                        out.push(DisplayData {
                            id: display.displayID(),
                            x: frame.origin.x,
                            y: frame.origin.y,
                            w_pts: frame.size.width,
                            h_pts: frame.size.height,
                            scale: f64::from(filter.pointPixelScale()),
                        });
                    }
                }
                Ok(out)
            };
            let _ = tx.send(result);
        },
    );
    // SAFETY: the block is valid and SCK copies it before returning.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(mut v)) => {
            let main = CGMainDisplayID();
            v.sort_by_key(|d| u8::from(d.id != main)); // primary first, stable otherwise
            Ok(v)
        }
        Ok(Err(msg))
            if msg.to_lowercase().contains("declined")
                || msg.to_lowercase().contains("permission") =>
        {
            Err(CaptureError::Permission(PERMISSION_HELP.into()))
        }
        Ok(Err(msg)) => Err(CaptureError::Failed(msg)),
        Err(_) => Err(CaptureError::Failed(
            "ScreenCaptureKit did not answer in time".into(),
        )),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_info(index: usize, d: &DisplayData, primary: bool) -> MonitorInfo {
    MonitorInfo {
        index: u8::try_from(index).unwrap_or(u8::MAX),
        name: format!("Display {}", d.id),
        x: d.x as i32,
        y: d.y as i32,
        width: (d.w_pts * d.scale).round() as u32,
        height: (d.h_pts * d.scale).round() as u32,
        scale: d.scale as f32,
        refresh_hz: 0.0,
        primary,
    }
}

impl CaptureBackend for MacBackend {
    fn name(&self) -> &'static str {
        "screencapturekit"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let displays = fetch_displays()?;
        Ok(displays
            .iter()
            .enumerate()
            .map(|(i, d)| to_info(i, d, i == 0))
            .collect())
    }

    fn open(&self, index: u8) -> Result<Box<dyn Capturer>, CaptureError> {
        let displays = fetch_displays()?;
        let data = displays
            .get(usize::from(index))
            .cloned()
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        let monitor = to_info(usize::from(index), &data, index == 0);
        Ok(Box::new(MacCapturer::start(monitor, data.id)?))
    }

    fn cursor_source(&self) -> Result<Option<Box<dyn CursorSource>>, CaptureError> {
        Ok(Some(Box::new(MacCursor)))
    }
}

/// The latest frame, handed from SCK's queue to the capture thread.
#[derive(Default)]
struct Mailbox {
    slot: Mutex<Option<Frame>>,
    ready: Condvar,
}

struct OutputIvars {
    mailbox: Arc<Mailbox>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; we implement no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "NexusStreamOutput"]
    #[ivars = OutputIvars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output_sample_buffer_of_type(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: `sample` is valid for the duration of the callback.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            }; // idle frame
            if let Some(frame) = copy_pixels(&image) {
                if let Ok(mut slot) = self.ivars().mailbox.slot.lock() {
                    *slot = Some(frame);
                }
                self.ivars().mailbox.ready.notify_one();
            }
        }
    }
);

impl Output {
    fn new(mailbox: Arc<Mailbox>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { mailbox });
        // SAFETY: standard `init` on a freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }
}

/// Copies a BGRA pixel buffer into a tightly packed [`Frame`].
fn copy_pixels(buffer: &objc2_core_video::CVPixelBuffer) -> Option<Frame> {
    let flags = CVPixelBufferLockFlags(1); // kCVPixelBufferLock_ReadOnly
    // SAFETY: the buffer is live; the base address is only read while locked.
    unsafe {
        if CVPixelBufferLockBaseAddress(buffer, flags) != kCVReturnSuccess {
            return None;
        }
        let (w, h) = (
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        );
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
        let frame = if base.is_null() || stride < w * 4 {
            None
        } else {
            let mut data = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                data.extend_from_slice(std::slice::from_raw_parts(base.add(row * stride), w * 4));
            }
            Some(Frame {
                width: u32::try_from(w).ok()?,
                height: u32::try_from(h).ok()?,
                stride: w * 4,
                format: FrameFormat::Bgra8,
                data: Bytes::from(data),
                captured_us: now_us(),
                dirty: Vec::new(),
            })
        };
        CVPixelBufferUnlockBaseAddress(buffer, flags);
        frame
    }
}

struct MacCapturer {
    monitor: MonitorInfo,
    mailbox: Arc<Mailbox>,
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    first: bool,
}

// SAFETY: the SCK objects are only touched from the owning capture thread (start/stop) and SCK's
// own serial queue (which only writes into the mutex-protected mailbox).
unsafe impl Send for MacCapturer {}

impl MacCapturer {
    fn start(monitor: MonitorInfo, display_id: u32) -> Result<Self, CaptureError> {
        // Re-fetch the SCDisplay object (we only kept plain data).
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(
            move |content: *mut SCShareableContent, _error: *mut NSError| {
                // SAFETY: `content` is valid for the callback's duration when non-null, and the
                // SCDisplay objects are retained by the `displays` array we iterate.
                let found: Option<Retained<SCDisplay>> = unsafe {
                    content
                        .as_ref()
                        .and_then(|c| c.displays().iter().find(|d| d.displayID() == display_id))
                };
                let _ = tx.send(found.map(SendDisplay));
            },
        );
        // SAFETY: block is valid; SCK copies it.
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
        let display = rx
            .recv_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .map(|d| d.0)
            .ok_or_else(|| CaptureError::Failed("display disappeared".into()))?;

        // SAFETY: all calls below are plain property setters / initialisers on objects we own.
        unsafe {
            let empty = NSArray::new();
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &empty,
            );
            let config = SCStreamConfiguration::new();
            config.setWidth(monitor.width as usize);
            config.setHeight(monitor.height as usize);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setShowsCursor(false);
            config.setQueueDepth(3);
            config.setMinimumFrameInterval(CMTime::new(1, 120));
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            );
            let mailbox = Arc::new(Mailbox::default());
            let output = Output::new(Arc::clone(&mailbox));
            let queue = DispatchQueue::new("app.nexus-remote.capture", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| CaptureError::Failed(e.localizedDescription().to_string()))?;
            let (done_tx, done_rx) = mpsc::channel();
            let done = RcBlock::new(move |error: *mut NSError| {
                let msg = error.as_ref().map(|e| e.localizedDescription().to_string());
                let _ = done_tx.send(msg);
            });
            stream.startCaptureWithCompletionHandler(Some(&done));
            match done_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(None) => {}
                Ok(Some(msg)) => {
                    return Err(CaptureError::Failed(format!("starting capture: {msg}")));
                }
                Err(_) => {
                    return Err(CaptureError::Failed(
                        "ScreenCaptureKit did not start in time".into(),
                    ));
                }
            }
            Ok(Self {
                monitor,
                mailbox,
                stream,
                _output: output,
                _queue: queue,
                first: true,
            })
        }
    }
}

/// `Retained<SCDisplay>` is not `Send`; it only crosses the channel once, to the thread that
/// created the stream, and is never used concurrently.
struct SendDisplay(Retained<SCDisplay>);
// SAFETY: see the type's documentation.
unsafe impl Send for SendDisplay {}

impl Capturer for MacCapturer {
    fn name(&self) -> &'static str {
        "screencapturekit"
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let deadline = Instant::now()
            + timeout.max(if self.first {
                Duration::from_secs(3)
            } else {
                Duration::ZERO
            });
        let mut slot = self
            .mailbox
            .slot
            .lock()
            .map_err(|_| CaptureError::Failed("mailbox poisoned".into()))?;
        loop {
            if let Some(frame) = slot.take() {
                self.first = false;
                return Ok(Some(frame));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let (guard, _) = self
                .mailbox
                .ready
                .wait_timeout(slot, deadline - now)
                .map_err(|_| CaptureError::Failed("mailbox poisoned".into()))?;
            slot = guard;
        }
    }
}

impl Drop for MacCapturer {
    fn drop(&mut self) {
        let done = RcBlock::new(|_error: *mut NSError| {});
        // SAFETY: the stream is live; the handler block is copied by SCK.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&done)) };
    }
}

/// Samples the pointer position with `CGEvent` (no extra permission needed). The cursor *shape*
/// is not available without AppKit and is left to the controller's default arrow.
struct MacCursor;

impl CursorSource for MacCursor {
    fn poll(&mut self) -> Result<CursorSample, CaptureError> {
        let event: Option<CFRetained<CGEvent>> = CGEvent::new(None);
        let CGPoint { x, y } = CGEvent::location(event.as_deref());
        #[allow(clippy::cast_possible_truncation)]
        Ok(CursorSample {
            x: x as i32,
            y: y as i32,
            visible: true,
            shape_id: 0,
            new_shape: None,
        })
    }
}
