use bytes::Bytes;
use nexus_proto::v1::VideoCodec;

use crate::CodecError;

pub use nexus_proto::wire::video::Rect;

/// Pixel layout of a captured frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFormat {
    /// 8-bit B, G, R, A in memory order (what every platform's screen capture can produce).
    Bgra8,
}

/// An uncompressed screen frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes between the start of consecutive rows (≥ `width * 4`).
    pub stride: usize,
    /// Pixel layout.
    pub format: FrameFormat,
    /// Pixel data, `stride * height` bytes.
    pub data: Bytes,
    /// Monotonic microsecond timestamp of when the frame was captured.
    pub captured_us: u64,
    /// Regions changed since the previous frame; empty means "unknown, assume everything".
    pub dirty: Vec<Rect>,
}

impl Frame {
    /// Checks that the buffer matches the declared geometry.
    pub fn validate(&self) -> Result<(), CodecError> {
        let min_stride = self.width as usize * 4;
        if self.width == 0 || self.height == 0 {
            return Err(CodecError::Invalid("zero-sized frame".into()));
        }
        if self.stride < min_stride {
            return Err(CodecError::Invalid(format!(
                "stride {} < {}",
                self.stride, min_stride
            )));
        }
        let needed = self.stride * (self.height as usize - 1) + min_stride;
        if self.data.len() < needed {
            return Err(CodecError::Invalid(format!(
                "buffer of {} bytes is too small for {}x{} (needs {needed})",
                self.data.len(),
                self.width,
                self.height
            )));
        }
        Ok(())
    }
}

/// Encoder settings.
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    /// Output codec.
    pub codec: VideoCodec,
    /// Width in pixels (even).
    pub width: u32,
    /// Height in pixels (even).
    pub height: u32,
    /// Target frame rate.
    pub fps: f32,
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Tune for text/UI content rather than natural video.
    pub screen_content: bool,
    /// Insert a keyframe at least this often, in frames (`None` = only on request).
    pub keyframe_interval: Option<u32>,
}

impl EncoderConfig {
    /// A sensible low-latency screen-content config.
    #[must_use]
    pub fn screen(width: u32, height: u32, fps: f32, bitrate_bps: u32) -> Self {
        Self {
            codec: VideoCodec::H264,
            width,
            height,
            fps,
            bitrate_bps,
            screen_content: true,
            keyframe_interval: None,
        }
    }

    /// Validates the constraints all backends share.
    pub fn validate(&self) -> Result<(), CodecError> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err(CodecError::Invalid(format!(
                "dimensions must be positive and even, got {}x{}",
                self.width, self.height
            )));
        }
        if self.width > 8192 || self.height > 8192 {
            return Err(CodecError::Invalid(
                "dimensions above 8192 are not supported".into(),
            ));
        }
        if !(1.0..=240.0).contains(&self.fps) {
            return Err(CodecError::Invalid(format!(
                "fps {} out of range 1..=240",
                self.fps
            )));
        }
        if self.bitrate_bps < 50_000 {
            return Err(CodecError::Invalid("bitrate below 50 kbit/s".into()));
        }
        Ok(())
    }
}

/// One compressed frame.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Bitstream format.
    pub codec: VideoCodec,
    /// True for a self-contained (IDR) frame, with parameter sets in front.
    pub keyframe: bool,
    /// Annex-B bitstream.
    pub data: Bytes,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The input frame's capture timestamp, carried through.
    pub captured_us: u64,
    /// Time the encoder spent on this frame.
    pub encode_time: std::time::Duration,
}

/// Turns [`Frame`]s into [`EncodedFrame`]s.
pub trait Encoder: Send {
    /// Backend name for logs and telemetry.
    fn name(&self) -> &'static str;

    /// Current configuration.
    fn config(&self) -> &EncoderConfig;

    /// Encodes one frame. `force_keyframe` makes this frame a keyframe. Returns `None` if the
    /// backend chose to skip the frame.
    fn encode(
        &mut self,
        frame: &Frame,
        force_keyframe: bool,
    ) -> Result<Option<EncodedFrame>, CodecError>;

    /// Changes the target bitrate without restarting the stream.
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), CodecError>;
}

/// Planar 4:2:0 picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I420Planes {
    /// Luma plane.
    pub y: Vec<u8>,
    /// Blue-difference chroma plane.
    pub u: Vec<u8>,
    /// Red-difference chroma plane.
    pub v: Vec<u8>,
    /// Luma row stride.
    pub y_stride: usize,
    /// Chroma row stride.
    pub uv_stride: usize,
}

/// A decoded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The planes (limited-range BT.709, as encoded).
    pub planes: I420Planes,
}

/// Turns bitstream into [`DecodedFrame`]s.
pub trait Decoder: Send {
    /// Backend name for logs and telemetry.
    fn name(&self) -> &'static str;

    /// Decodes one access unit. `Ok(None)` means "no picture yet" (e.g. waiting for a keyframe).
    /// An error means the stream is broken; request a keyframe.
    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, CodecError>;
}
