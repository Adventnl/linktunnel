//! Video codecs behind two small traits, [`Encoder`] and [`Decoder`].
//!
//! Backends:
//! * [`h264`]: OpenH264, software, everywhere. Tuned for screen content and low latency. This is
//!   the fallback and the reference the other backends are tested against.
//! * `videotoolbox` (macOS only): the Apple hardware encoder. Decoding on macOS uses OpenH264.
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
#[cfg(feature = "software")]
pub mod h264;
mod types;
#[cfg(target_os = "macos")]
pub mod videotoolbox;

pub use convert::{bgra_to_i420, i420_to_bgra, psnr_bgra};
pub use error::CodecError;
#[cfg(feature = "software")]
pub use h264::{H264Decoder, H264Encoder};
pub use types::{
    DecodedFrame, Decoder, EncodedFrame, Encoder, EncoderConfig, Frame, FrameFormat, I420Planes,
    Rect,
};
#[cfg(target_os = "macos")]
pub use videotoolbox::VideoToolboxEncoder;

use nexus_proto::v1::VideoCodec;

/// Codecs this build can encode, best first.
#[must_use]
pub fn supported_encode() -> Vec<VideoCodec> {
    #[cfg(any(feature = "software", target_os = "macos"))]
    {
        vec![VideoCodec::H264]
    }
    #[cfg(not(any(feature = "software", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Codecs this build can decode, best first.
#[must_use]
pub fn supported_decode() -> Vec<VideoCodec> {
    #[cfg(feature = "software")]
    {
        vec![VideoCodec::H264]
    }
    #[cfg(not(feature = "software"))]
    {
        Vec::new()
    }
}

/// Creates the best available encoder for `config`: hardware first where this build has a
/// backend, then software. The returned encoder's [`Encoder::name`] says which one was chosen.
#[allow(clippy::needless_pass_by_value)]
pub fn create_encoder(config: EncoderConfig) -> Result<Box<dyn Encoder>, CodecError> {
    #[cfg(target_os = "macos")]
    {
        match VideoToolboxEncoder::new(config.clone()) {
            Ok(e) => return Ok(Box::new(e)),
            Err(e) => {
                tracing::warn!(error = %e, "VideoToolbox encoder unavailable; falling back to software")
            }
        }
    }
    #[cfg(feature = "software")]
    {
        return Ok(Box::new(H264Encoder::new(config)?));
    }
    #[allow(unreachable_code)]
    Err(CodecError::Unavailable(format!(
        "no encoder for {:?} in this build",
        config.codec
    )))
}

/// Creates a decoder for `codec`.
pub fn create_decoder(codec: VideoCodec) -> Result<Box<dyn Decoder>, CodecError> {
    #[cfg(feature = "software")]
    if codec == VideoCodec::H264 {
        return Ok(Box::new(H264Decoder::new()?));
    }
    Err(CodecError::Unavailable(format!(
        "no decoder for {codec:?} in this build"
    )))
}
