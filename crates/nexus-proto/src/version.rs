//! Protocol version and capability negotiation.
//!
//! Both peers exchange a [`Hello`] as the first message on the control stream. That exchange
//! happens *inside* the authenticated, encrypted transport, so an on-path attacker cannot
//! strip capabilities or force an older version. On top of that, each side states its own
//! downgrade floor (`min_version`) and refuses to settle below it.

use std::collections::BTreeSet;

use crate::v1::{Capabilities, Hello, ProtocolVersion, VideoCodec};

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// The oldest protocol version this build still accepts.
pub const MIN_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// Why two peers cannot talk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiationError {
    /// A required `Hello` field was absent.
    #[error("hello is missing the `{0}` field")]
    MissingField(&'static str),
    /// The peers speak different major versions.
    #[error("incompatible protocol major versions: {controller} vs {target}")]
    IncompatibleMajor {
        /// The controller's major version.
        controller: u32,
        /// The target's major version.
        target: u32,
    },
    /// The best common version is below one side's floor.
    #[error("negotiated version {agreed_major}.{agreed_minor} is below a peer's minimum")]
    BelowMinimum {
        /// Major of the highest common version.
        agreed_major: u32,
        /// Minor of the highest common version.
        agreed_minor: u32,
    },
}

/// The outcome of a successful negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// Version both sides will speak.
    pub version: ProtocolVersion,
    /// Feature strings both sides support.
    pub features: BTreeSet<String>,
    /// Chosen video codec, or `None` when there is no common codec (video unavailable; the
    /// session degrades to terminal/audio-only).
    pub video_codec: Option<VideoCodec>,
    /// Largest stream width both can handle (0 = unlimited).
    pub max_width: u32,
    /// Largest stream height both can handle (0 = unlimited).
    pub max_height: u32,
    /// Highest frame rate both can handle (0 = unlimited).
    pub max_fps: u32,
}

impl Negotiated {
    /// True when `feature` was negotiated.
    #[must_use]
    pub fn has(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }
}

fn key(v: &ProtocolVersion) -> (u32, u32) {
    (v.major, v.minor)
}

/// Both sides' limits combine as "the smaller non-zero value" (zero means unlimited).
const fn min_nonzero(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (x, y) => {
            if x < y {
                x
            } else {
                y
            }
        }
    }
}

/// Negotiates a session between a `controller` (consumes video) and a `target` (produces
/// video). `codec_preference` is the controller's local policy, best first; the first codec
/// that the target can encode *and* the controller can decode wins.
pub fn negotiate(
    controller: &Hello,
    target: &Hello,
    codec_preference: &[VideoCodec],
) -> Result<Negotiated, NegotiationError> {
    let c_ver = controller
        .version
        .as_ref()
        .ok_or(NegotiationError::MissingField("version"))?;
    let t_ver = target
        .version
        .as_ref()
        .ok_or(NegotiationError::MissingField("version"))?;
    let c_min = controller
        .min_version
        .as_ref()
        .ok_or(NegotiationError::MissingField("min_version"))?;
    let t_min = target
        .min_version
        .as_ref()
        .ok_or(NegotiationError::MissingField("min_version"))?;

    if c_ver.major != t_ver.major {
        return Err(NegotiationError::IncompatibleMajor {
            controller: c_ver.major,
            target: t_ver.major,
        });
    }

    let agreed = if key(c_ver) <= key(t_ver) {
        c_ver
    } else {
        t_ver
    };
    if key(agreed) < key(c_min) || key(agreed) < key(t_min) {
        return Err(NegotiationError::BelowMinimum {
            agreed_major: agreed.major,
            agreed_minor: agreed.minor,
        });
    }

    let empty = Capabilities::default();
    let c_caps = controller.capabilities.as_ref().unwrap_or(&empty);
    let t_caps = target.capabilities.as_ref().unwrap_or(&empty);

    let c_features: BTreeSet<&String> = c_caps.features.iter().collect();
    let features = t_caps
        .features
        .iter()
        .filter(|f| c_features.contains(f))
        .cloned()
        .collect::<BTreeSet<_>>();

    let target_can_encode: BTreeSet<i32> = t_caps.video_encode.iter().copied().collect();
    let controller_can_decode: BTreeSet<i32> = c_caps.video_decode.iter().copied().collect();
    let video_codec = codec_preference.iter().copied().find(|c| {
        let v = *c as i32;
        target_can_encode.contains(&v) && controller_can_decode.contains(&v)
    });

    Ok(Negotiated {
        version: *agreed,
        features,
        video_codec,
        max_width: min_nonzero(c_caps.max_width, t_caps.max_width),
        max_height: min_nonzero(c_caps.max_height, t_caps.max_height),
        max_fps: min_nonzero(c_caps.max_fps, t_caps.max_fps),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;

    fn hello(major: u32, minor: u32, min_minor: u32, caps: Capabilities) -> Hello {
        Hello {
            version: Some(ProtocolVersion { major, minor }),
            min_version: Some(ProtocolVersion {
                major,
                minor: min_minor,
            }),
            capabilities: Some(caps),
            ..Hello::default()
        }
    }

    fn caps(enc: &[VideoCodec], dec: &[VideoCodec], feats: &[&str]) -> Capabilities {
        Capabilities {
            video_encode: enc.iter().map(|c| *c as i32).collect(),
            video_decode: dec.iter().map(|c| *c as i32).collect(),
            features: feats.iter().map(ToString::to_string).collect(),
            ..Capabilities::default()
        }
    }

    #[test]
    fn picks_lowest_common_version_and_intersects_features() {
        let controller = hello(
            1,
            4,
            0,
            caps(
                &[],
                &[VideoCodec::H264],
                &[features::CLIPBOARD_TEXT, features::AUDIO_OPUS],
            ),
        );
        let target = hello(
            1,
            2,
            0,
            caps(
                &[VideoCodec::H264],
                &[],
                &[features::CLIPBOARD_TEXT, features::TERMINAL_PTY],
            ),
        );
        let n = negotiate(&controller, &target, &[VideoCodec::H264]).unwrap();
        assert_eq!((n.version.major, n.version.minor), (1, 2));
        assert!(n.has(features::CLIPBOARD_TEXT));
        assert!(!n.has(features::AUDIO_OPUS), "only one side supports audio");
        assert!(
            !n.has(features::TERMINAL_PTY),
            "only one side supports terminal"
        );
    }

    #[test]
    fn codec_follows_controller_preference_among_common_codecs() {
        let controller = hello(
            1,
            0,
            0,
            caps(
                &[],
                &[VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1],
                &[],
            ),
        );
        let target = hello(
            1,
            0,
            0,
            caps(&[VideoCodec::H264, VideoCodec::Hevc], &[], &[]),
        );
        let n = negotiate(
            &controller,
            &target,
            &[VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264],
        )
        .unwrap();
        assert_eq!(
            n.video_codec,
            Some(VideoCodec::Hevc),
            "AV1 is not encodable by the target"
        );
    }

    #[test]
    fn no_common_codec_means_no_video_not_an_error() {
        let controller = hello(1, 0, 0, caps(&[], &[VideoCodec::Av1], &[]));
        let target = hello(1, 0, 0, caps(&[VideoCodec::H264], &[], &[]));
        let n = negotiate(&controller, &target, &[VideoCodec::Av1, VideoCodec::H264]).unwrap();
        assert_eq!(n.video_codec, None);
    }

    #[test]
    fn major_mismatch_is_rejected() {
        let a = hello(1, 0, 0, Capabilities::default());
        let b = hello(2, 0, 0, Capabilities::default());
        assert_eq!(
            negotiate(&a, &b, &[]).unwrap_err(),
            NegotiationError::IncompatibleMajor {
                controller: 1,
                target: 2
            }
        );
    }

    #[test]
    fn downgrade_below_a_peers_floor_is_refused() {
        // Controller insists on >= 1.3; target only speaks 1.1. An attacker (or a stale
        // build) must not be able to talk the controller down.
        let controller = hello(1, 5, 3, Capabilities::default());
        let target = hello(1, 1, 0, Capabilities::default());
        assert_eq!(
            negotiate(&controller, &target, &[]).unwrap_err(),
            NegotiationError::BelowMinimum {
                agreed_major: 1,
                agreed_minor: 1
            }
        );
    }

    #[test]
    fn missing_fields_are_errors() {
        let ok = hello(1, 0, 0, Capabilities::default());
        let bad = Hello::default();
        assert_eq!(
            negotiate(&ok, &bad, &[]).unwrap_err(),
            NegotiationError::MissingField("version")
        );
    }

    #[test]
    fn limits_use_smallest_nonzero() {
        let mut c = caps(&[], &[], &[]);
        c.max_width = 3840;
        c.max_fps = 0;
        let mut t = caps(&[], &[], &[]);
        t.max_width = 2560;
        t.max_fps = 60;
        let n = negotiate(&hello(1, 0, 0, c), &hello(1, 0, 0, t), &[]).unwrap();
        assert_eq!(n.max_width, 2560);
        assert_eq!(n.max_fps, 60);
        assert_eq!(n.max_height, 0);
    }

    #[test]
    fn this_build_accepts_itself() {
        let me = Hello {
            version: Some(PROTOCOL_VERSION),
            min_version: Some(MIN_PROTOCOL_VERSION),
            ..Hello::default()
        };
        assert!(negotiate(&me, &me, &[]).is_ok());
    }
}
