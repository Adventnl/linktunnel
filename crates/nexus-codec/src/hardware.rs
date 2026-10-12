//! FFmpeg hardware H.264 backends: NVENC, AMF, Quick Sync and VAAPI.
//! System BGRA capture still requires CPU colour conversion and a GPU upload.
use crate::{CodecError, EncodedFrame, Encoder, EncoderConfig, Frame, bgra_to_i420};
use bytes::Bytes;
use ffmpeg_next as ffmpeg;
use nexus_proto::v1::VideoCodec;
use std::time::Instant;

/// Available vendor backends, in automatic selection order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync.
    QuickSync,
    /// Linux VAAPI.
    Vaapi,
}
impl Backend {
    /// FFmpeg encoder identifier.
    pub fn name(self) -> &'static str {
        match self {
            Self::Nvenc => "h264_nvenc",
            Self::Amf => "h264_amf",
            Self::QuickSync => "h264_qsv",
            Self::Vaapi => "h264_vaapi",
        }
    }
}

/// A persistent low-delay hardware encoder, selected by a real encode probe.
pub struct HardwareEncoder {
    config: EncoderConfig,
    backend: Backend,
    raw: ffmpeg::encoder::Video,
    frames: u64,
    force_next: bool,
}
impl HardwareEncoder {
    /// Tries each hardware backend. A compiled codec without a working device is not enough.
    pub fn best(config: &EncoderConfig) -> Result<Self, CodecError> {
        let mut errors = Vec::new();
        for backend in [
            Backend::Nvenc,
            Backend::Amf,
            Backend::QuickSync,
            Backend::Vaapi,
        ] {
            match Self::new(config.clone(), backend) {
                Ok(e) => return Ok(e),
                Err(e) => errors.push(format!("{}: {e}", backend.name())),
            }
        }
        Err(CodecError::Unavailable(errors.join("; ")))
    }
    /// Opens a specific backend and probes that it produces an immediate decodable IDR.
    pub fn new(config: EncoderConfig, backend: Backend) -> Result<Self, CodecError> {
        config.validate()?;
        if config.codec != VideoCodec::H264 {
            return Err(CodecError::Unavailable(
                "hardware backend supports H.264 only".into(),
            ));
        }
        ffmpeg::init().map_err(unavailable)?;
        let codec = ffmpeg::encoder::find_by_name(backend.name()).ok_or_else(|| {
            CodecError::Unavailable(format!("{} is absent in this FFmpeg build", backend.name()))
        })?;
        let mut raw = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(unavailable)?;
        raw.set_width(config.width);
        raw.set_height(config.height);
        raw.set_format(if backend == Backend::Vaapi {
            ffmpeg::format::Pixel::VAAPI
        } else {
            ffmpeg::format::Pixel::NV12
        });
        #[allow(clippy::cast_possible_truncation)]
        let fps = (config.fps * 1000.0).round() as i32;
        raw.set_time_base((1000, fps));
        raw.set_frame_rate(Some((fps, 1000)));
        raw.set_bit_rate(config.bitrate_bps as usize);
        raw.set_max_b_frames(0);
        raw.set_gop(config.keyframe_interval.unwrap_or(600));
        let mut opts = ffmpeg::Dictionary::new();
        match backend {
            Backend::Nvenc => {
                for (k, v) in [
                    ("preset", "p1"),
                    ("tune", "ull"),
                    ("rc", "cbr"),
                    ("rc-lookahead", "0"),
                    ("zerolatency", "1"),
                    ("delay", "0"),
                    ("forced-idr", "1"),
                ] {
                    opts.set(k, v);
                }
            }
            Backend::Amf => {
                for (k, v) in [
                    ("usage", "ultralowlatency"),
                    ("quality", "speed"),
                    ("rc", "cbr"),
                    ("header_insertion_mode", "idr"),
                    ("preanalysis", "false"),
                ] {
                    opts.set(k, v);
                }
            }
            Backend::QuickSync => {
                for (k, v) in [
                    ("preset", "veryfast"),
                    ("async_depth", "1"),
                    ("look_ahead", "0"),
                    ("forced_idr", "1"),
                    ("repeat_pps", "1"),
                ] {
                    opts.set(k, v);
                }
            }
            Backend::Vaapi => {
                opts.set("async_depth", "1");
                opts.set("rc_mode", "CBR");
                vaapi::configure(&mut raw, config.width, config.height)?;
            }
        }
        let raw = raw.open_as_with(codec, opts).map_err(unavailable)?;
        let mut encoder = Self {
            config,
            backend,
            raw,
            frames: 0,
            force_next: false,
        };
        let stride = encoder.config.width as usize * 4;
        let probe = Frame {
            width: encoder.config.width,
            height: encoder.config.height,
            stride,
            format: crate::FrameFormat::Bgra8,
            data: Bytes::from(vec![0; stride * encoder.config.height as usize]),
            captured_us: 0,
            dirty: vec![],
        };
        let encoded = encoder.encode(&probe, true)?.ok_or_else(|| {
            CodecError::Unavailable(
                "backend buffers frames instead of producing immediate output".into(),
            )
        })?;
        if !encoded.keyframe || !has_parameter_sets(&encoded.data) {
            return Err(CodecError::Unavailable(
                "backend did not emit an Annex-B IDR with SPS/PPS".into(),
            ));
        }
        encoder.force_next = true;
        Ok(encoder)
    }
}
fn unavailable(e: ffmpeg::Error) -> CodecError {
    CodecError::Unavailable(e.to_string())
}
fn encode_error(e: ffmpeg::Error) -> CodecError {
    CodecError::Encode(e.to_string())
}
fn has_parameter_sets(data: &[u8]) -> bool {
    let nal = |kind| {
        data.windows(4)
            .any(|w| w[..3] == [0, 0, 1] && w[3] & 31 == kind)
    };
    nal(7) && nal(8)
}
impl Encoder for HardwareEncoder {
    fn name(&self) -> &'static str {
        self.backend.name()
    }
    fn config(&self) -> &EncoderConfig {
        &self.config
    }
    fn encode(
        &mut self,
        frame: &Frame,
        force_keyframe: bool,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        frame.validate()?;
        if (frame.width, frame.height) != (self.config.width, self.config.height) {
            return Err(CodecError::Invalid(
                "frame dimensions differ from hardware encoder".into(),
            ));
        }
        let started = Instant::now();
        let planes = bgra_to_i420(frame)?;
        let mut input =
            ffmpeg::frame::Video::new(ffmpeg::format::Pixel::NV12, frame.width, frame.height);
        let stride = input.stride(0);
        for row in 0..frame.height as usize {
            input.data_mut(0)[row * stride..row * stride + frame.width as usize].copy_from_slice(
                &planes.y[row * planes.y_stride..row * planes.y_stride + frame.width as usize],
            );
        }
        let stride = input.stride(1);
        for row in 0..frame.height as usize / 2 {
            for x in 0..frame.width as usize / 2 {
                input.data_mut(1)[row * stride + x * 2] = planes.u[row * planes.uv_stride + x];
                input.data_mut(1)[row * stride + x * 2 + 1] = planes.v[row * planes.uv_stride + x];
            }
        }
        input.set_pts(Some(i64::try_from(self.frames).map_err(|_| {
            CodecError::Encode("frame timestamp exhausted".into())
        })?));
        input.set_kind(if force_keyframe || self.force_next || self.frames == 0 {
            ffmpeg::picture::Type::I
        } else {
            ffmpeg::picture::Type::None
        });
        let input = if self.backend == Backend::Vaapi {
            vaapi::upload(&mut self.raw, &input)?
        } else {
            input
        };
        self.raw.send_frame(&input).map_err(encode_error)?;
        self.frames += 1;
        self.force_next = false;
        let mut packet = ffmpeg::Packet::empty();
        self.raw.receive_packet(&mut packet).map_err(encode_error)?;
        let data = packet
            .data()
            .ok_or_else(|| CodecError::Encode("empty hardware packet".into()))?;
        // Each call must represent this exact capture timestamp; no asynchronous frame backlog.
        if packet.pts() != input.pts() {
            return Err(CodecError::Encode(
                "hardware output timestamp does not match current frame".into(),
            ));
        }
        let keyframe = packet.is_key();
        if keyframe && !has_parameter_sets(data) {
            return Err(CodecError::Encode(
                "IDR missing repeated parameter sets".into(),
            ));
        }
        Ok(Some(EncodedFrame {
            codec: VideoCodec::H264,
            keyframe,
            data: Bytes::copy_from_slice(data),
            width: frame.width,
            height: frame.height,
            captured_us: frame.captured_us,
            encode_time: started.elapsed(),
        }))
    }
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), CodecError> {
        if bitrate_bps == self.config.bitrate_bps {
            return Ok(());
        }
        let mut next = self.config.clone();
        next.bitrate_bps = bitrate_bps;
        // Vendor runtime reconfiguration differs. Reopen transactionally and emit a new IDR.
        let replacement = Self::new(next, self.backend)?;
        *self = replacement;
        Ok(())
    }
}

#[allow(unsafe_code)]
mod vaapi {
    use crate::CodecError;
    use ffmpeg::ffi::*;
    use ffmpeg_next as ffmpeg;
    pub(super) fn configure(
        raw: &mut ffmpeg::encoder::video::Video,
        width: u32,
        height: u32,
    ) -> Result<(), CodecError> {
        let width = i32::try_from(width).map_err(|e| CodecError::Invalid(e.to_string()))?;
        let height = i32::try_from(height).map_err(|e| CodecError::Invalid(e.to_string()))?;
        // SAFETY: FFmpeg owns all referenced buffers. Every allocation is checked; refs are
        // transferred to AVCodecContext only after successful initialization, which frees them.
        unsafe {
            let mut device = std::ptr::null_mut();
            let status = av_hwdevice_ctx_create(
                &mut device,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if status < 0 {
                return Err(CodecError::Unavailable(
                    ffmpeg::Error::from(status).to_string(),
                ));
            }
            let mut frames = av_hwframe_ctx_alloc(device);
            av_buffer_unref(&mut device);
            if frames.is_null() {
                return Err(CodecError::Unavailable(
                    "VAAPI frame allocation failed".into(),
                ));
            }
            let ctx = (*frames).data.cast::<AVHWFramesContext>();
            (*ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*ctx).width = width;
            (*ctx).height = height;
            (*ctx).initial_pool_size = 4;
            let status = av_hwframe_ctx_init(frames);
            if status < 0 {
                av_buffer_unref(&mut frames);
                return Err(CodecError::Unavailable(
                    ffmpeg::Error::from(status).to_string(),
                ));
            }
            (*raw.as_mut_ptr()).hw_frames_ctx = frames;
        }
        Ok(())
    }
    pub(super) fn upload(
        raw: &mut ffmpeg::encoder::Video,
        input: &ffmpeg::frame::Video,
    ) -> Result<ffmpeg::frame::Video, CodecError> {
        let mut gpu = ffmpeg::frame::Video::empty();
        // SAFETY: the context has an initialized hw_frames_ctx, the input is NV12 of the same
        // geometry and AVFrame owns the returned references (freed by the safe wrapper's Drop).
        unsafe {
            let status =
                av_hwframe_get_buffer((*raw.as_mut_ptr()).hw_frames_ctx, gpu.as_mut_ptr(), 0);
            if status < 0 {
                return Err(CodecError::Encode(ffmpeg::Error::from(status).to_string()));
            }
            let status = av_hwframe_transfer_data(gpu.as_mut_ptr(), input.as_ptr(), 0);
            if status < 0 {
                return Err(CodecError::Encode(ffmpeg::Error::from(status).to_string()));
            }
            let status = av_frame_copy_props(gpu.as_mut_ptr(), input.as_ptr());
            if status < 0 {
                return Err(CodecError::Encode(ffmpeg::Error::from(status).to_string()));
            }
        }
        Ok(gpu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_config_before_device_probing() {
        assert!(HardwareEncoder::best(&EncoderConfig::screen(1, 10, 60.0, 1_000_000)).is_err());
    }
    #[test]
    fn parameter_sets_need_both_nal_units() {
        assert!(has_parameter_sets(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68]));
        assert!(!has_parameter_sets(&[0, 0, 1, 0x67]));
    }
    #[test]
    #[ignore = "requires a matching GPU and FFmpeg hardware codecs; run on the device lab"]
    fn hardware_stream_decodes_and_changes_rate() {
        let cfg = EncoderConfig::screen(128, 72, 60.0, 1_000_000);
        let mut encoder = HardwareEncoder::best(&cfg).unwrap();
        let frame = Frame {
            width: 128,
            height: 72,
            stride: 512,
            format: crate::FrameFormat::Bgra8,
            data: Bytes::from(vec![80; 512 * 72]),
            captured_us: 99,
            dirty: vec![],
        };
        let mut decoder = crate::H264Decoder::new().unwrap();
        for i in 0..20 {
            if i == 10 {
                encoder.set_bitrate(2_000_000).unwrap();
            }
            let encoded = encoder.encode(&frame, i == 0 || i == 15).unwrap().unwrap();
            assert_eq!(encoded.captured_us, 99);
            if i == 0 || i == 10 || i == 15 {
                assert!(encoded.keyframe);
            }
            assert!(
                crate::Decoder::decode(&mut decoder, &encoded.data)
                    .unwrap()
                    .is_some()
            );
        }
    }
}
