//! Video codecs behind two small traits, [`Encoder`] and [`Decoder`].
//!
//! Backends:
//! * [`h264`]: OpenH264, software, everywhere. Tuned for screen content and low latency. This is
//!   the fallback and the reference the other backends are tested against.
//! * `videotoolbox` (macOS only): the Apple hardware encoder/decoder.
//!
//! Hardware backends for other platforms (NVENC, AMF, QuickSync, VAAPI, Media Foundation) plug
//! into the same traits; they are not implemented because they can only be validated on matching
//! hardware (see docs/STATUS.md).
//!
//! **Latency rules every backend follows:** no B-frames, no look-ahead, one output frame per input
//! frame, keyframes only on request (plus an optional long interval), and `SPS`/`PPS` repeated in
//! front of every keyframe so a decoder can join at any keyframe.

#![deny(unsafe_code)]

mod convert;
mod error;
pub mod h264;
mod types;
#[cfg(target_os = "macos")]
pub mod videotoolbox;

pub use convert::{bgra_to_i420, i420_to_bgra, psnr_bgra};
pub use error::CodecError;
pub use types::{
    DecodedFrame, Decoder, EncodedFrame, Encoder, EncoderConfig, Frame, FrameFormat, I420Planes,
    Rect,
};
