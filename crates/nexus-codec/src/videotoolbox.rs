//! Apple VideoToolbox hardware H.264 encoder (Apple Silicon media engine / Intel QuickSync).
//!
//! **Verification status:** this module is type-checked for `aarch64-apple-darwin` and
//! `x86_64-apple-darwin`; it is exercised by the tests below only when they run on a Mac (see
//! docs/STATUS.md). Decoding on macOS uses the OpenH264 decoder, which is fast enough for the
//! frame sizes we stream; a VideoToolbox decoder can be added behind the same trait.
//!
//! Design: the session runs in synchronous mode. [`Encoder::encode`] submits one frame, calls
//! `CompleteFrames`, and drains the single compressed sample the output callback produced, so
//! one input frame yields exactly one output frame and there is no queueing delay. The output is
//! converted from AVCC (length-prefixed NAL units) to Annex-B, with SPS/PPS prepended to every
//! keyframe.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use nexus_proto::v1::VideoCodec;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel,
};

use crate::CodecError;
use crate::types::{EncodedFrame, Encoder, EncoderConfig, Frame};

const TIMESCALE: i32 = 1_000_000;

/// What the compression callback hands back to the encode call.
#[derive(Default)]
struct Sink {
    slot: Mutex<Option<Result<Vec<u8>, String>>>,
}

/// Hardware H.264 encoder.
pub struct VideoToolboxEncoder {
    config: EncoderConfig,
    session: CFRetained<VTCompressionSession>,
    sink: Arc<Sink>,
    frames: u64,
}

impl std::fmt::Debug for VideoToolboxEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoToolboxEncoder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

// SAFETY: the VTCompressionSession is only used from the thread that currently owns the encoder
// (all methods take `&mut self`), and VideoToolbox sessions are not thread-affine.
unsafe impl Send for VideoToolboxEncoder {}

fn status(what: &str, code: i32) -> Result<(), CodecError> {
    if code == 0 {
        Ok(())
    } else {
        Err(CodecError::Encode(format!(
            "{what} failed with OSStatus {code}"
        )))
    }
}

fn set(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<(), CodecError> {
    // SAFETY: `session` is a live compression session and `key`/`value` are valid CF objects.
    let code = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    status("VTSessionSetProperty", code)
}

/// Compression callback. Runs on a VideoToolbox thread while `complete_frames` blocks the caller.
///
/// # Safety
/// `refcon` must be the pointer to a live [`Sink`] passed to `VTCompressionSession::create`.
unsafe extern "C-unwind" fn on_sample(
    refcon: *mut c_void,
    _source_refcon: *mut c_void,
    code: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: guaranteed by the caller contract above; the sink outlives the session.
    let sink = unsafe { &*(refcon as *const Sink) };
    let result = if code != 0 {
        Err(format!("encoder reported OSStatus {code}"))
    } else if let Some(sample) = NonNull::new(sample) {
        // SAFETY: VideoToolbox passes a valid sample buffer for the duration of the callback.
        unsafe { annexb_from_sample(sample.as_ref()) }
    } else {
        // Frame dropped by the encoder.
        Ok(Vec::new())
    };
    if let Ok(mut slot) = sink.slot.lock() {
        *slot = Some(result);
    }
}

/// Converts one H.264 sample from AVCC to Annex-B, prepending SPS/PPS when it contains an IDR.
///
/// # Safety
/// `sample` must be a valid, ready `CMSampleBuffer` holding H.264 data.
unsafe fn annexb_from_sample(sample: &CMSampleBuffer) -> Result<Vec<u8>, String> {
    // SAFETY: `sample` is valid per the contract.
    let block = unsafe { sample.data_buffer() }.ok_or("sample has no data buffer")?;
    let mut total = 0usize;
    let mut contiguous = 0usize;
    let mut data: *mut std::ffi::c_char = ptr::null_mut();
    // SAFETY: out-pointers point at locals.
    let code = unsafe { block.data_pointer(0, &mut contiguous, &mut total, &mut data) };
    if code != 0 || data.is_null() || contiguous != total {
        return Err(format!(
            "CMBlockBufferGetDataPointer: {code} (contiguous {contiguous} of {total})"
        ));
    }
    // SAFETY: `data` points at `total` readable bytes owned by `block`, which stays alive here.
    let avcc = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), total) };

    let mut header_len = 4i32;
    let mut parameter_sets: Vec<Vec<u8>> = Vec::new();
    // SAFETY: format description is optional and only read while `desc` is retained.
    if let Some(desc) = unsafe { sample.format_description() } {
        let mut count = 0usize;
        // SAFETY: out-pointers point at locals; null pointers are documented as allowed.
        let code = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &desc,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut count,
                &mut header_len,
            )
        };
        if code == 0 {
            for index in 0..count {
                let mut p: *const u8 = ptr::null();
                let mut n = 0usize;
                // SAFETY: as above; the returned pointer is valid while `desc` is retained.
                let code = unsafe {
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        &desc,
                        index,
                        &mut p,
                        &mut n,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                };
                if code == 0 && !p.is_null() {
                    // SAFETY: `p` points at `n` bytes inside `desc`.
                    parameter_sets.push(unsafe { std::slice::from_raw_parts(p, n) }.to_vec());
                }
            }
        }
    }
    let header_len = usize::try_from(header_len)
        .ok()
        .filter(|n| (1..=4).contains(n))
        .ok_or("bad NAL header length")?;

    let mut out = Vec::with_capacity(total + 64);
    let mut idr = false;
    let mut at = 0usize;
    let mut nals: Vec<&[u8]> = Vec::new();
    while at + header_len <= avcc.len() {
        let len = avcc[at..at + header_len]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        at += header_len;
        let end = at
            .checked_add(len)
            .filter(|e| *e <= avcc.len())
            .ok_or("NAL length overruns the sample")?;
        if len > 0 {
            idr |= avcc[at] & 0x1f == 5;
            nals.push(&avcc[at..end]);
        }
        at = end;
    }
    if idr {
        for ps in &parameter_sets {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(ps);
        }
    }
    for nal in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    Ok(out)
}

impl VideoToolboxEncoder {
    /// Creates a hardware encoder for `config`.
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        config.validate()?;
        if config.codec != VideoCodec::H264 {
            return Err(CodecError::Unavailable(format!(
                "VideoToolbox backend only does H.264, not {:?}",
                config.codec
            )));
        }
        let sink = Arc::new(Sink::default());
        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        let width = i32::try_from(config.width).map_err(|_| CodecError::Invalid("width".into()))?;
        let height =
            i32::try_from(config.height).map_err(|_| CodecError::Invalid("height".into()))?;
        // SAFETY: `raw` is a valid out-pointer; the refcon is the Arc's allocation, which this
        // struct keeps alive until after the session is invalidated (see `Drop`).
        let code = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                kCMVideoCodecType_H264,
                None,
                None,
                None,
                Some(on_sample),
                Arc::as_ptr(&sink).cast_mut().cast::<c_void>(),
                NonNull::from(&mut raw),
            )
        };
        if code != 0 {
            return Err(CodecError::Unavailable(format!(
                "VTCompressionSessionCreate failed with OSStatus {code}"
            )));
        }
        let raw = NonNull::new(raw)
            .ok_or_else(|| CodecError::Unavailable("VideoToolbox returned no session".into()))?;
        // SAFETY: `create` returned a +1 retained session that we now own.
        let session = unsafe { CFRetained::from_raw(raw) };
        let encoder = Self {
            config,
            session,
            sink,
            frames: 0,
        };
        encoder.configure()?;
        // SAFETY: session is live.
        status("PrepareToEncodeFrames", unsafe {
            encoder.session.prepare_to_encode_frames()
        })?;
        Ok(encoder)
    }

    fn configure(&self) -> Result<(), CodecError> {
        // SAFETY: the constants are valid CFStrings for the lifetime of the process.
        unsafe {
            set(
                &self.session,
                kVTCompressionPropertyKey_RealTime,
                CFBoolean::new(true),
            )?;
            set(
                &self.session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            )?;
            set(
                &self.session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel,
            )?;
        }
        let bitrate = i32::try_from(self.config.bitrate_bps).unwrap_or(i32::MAX);
        let interval = self
            .config
            .keyframe_interval
            .map_or(0, |n| i32::try_from(n).unwrap_or(i32::MAX));
        // SAFETY: as above.
        unsafe {
            set(
                &self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i32(bitrate),
            )?;
            set(
                &self.session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_f64(f64::from(self.config.fps)),
            )?;
            set(
                &self.session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i32(interval),
            )?;
        }
        Ok(())
    }

    fn pixel_buffer(&self, frame: &Frame) -> Result<CFRetained<CVPixelBuffer>, CodecError> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: valid out-pointer; no attributes or allocator overrides.
        let rc = unsafe {
            CVPixelBufferCreate(
                None,
                frame.width as usize,
                frame.height as usize,
                kCVPixelFormatType_32BGRA,
                None,
                NonNull::from(&mut raw),
            )
        };
        let raw = NonNull::new(raw)
            .filter(|_| rc == kCVReturnSuccess)
            .ok_or_else(|| CodecError::Encode(format!("CVPixelBufferCreate: {rc}")))?;
        // SAFETY: +1 retained buffer returned by Create.
        let buffer = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: the buffer is live; the base address is only touched while locked.
        unsafe {
            let flags = CVPixelBufferLockFlags(0);
            if CVPixelBufferLockBaseAddress(&buffer, flags) != kCVReturnSuccess {
                return Err(CodecError::Encode(
                    "CVPixelBufferLockBaseAddress failed".into(),
                ));
            }
            let dst = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let dst_stride = CVPixelBufferGetBytesPerRow(&buffer);
            let row = frame.width as usize * 4;
            if dst.is_null() || dst_stride < row {
                CVPixelBufferUnlockBaseAddress(&buffer, flags);
                return Err(CodecError::Encode(
                    "pixel buffer has an unexpected layout".into(),
                ));
            }
            for y in 0..frame.height as usize {
                let src = &frame.data[y * frame.stride..y * frame.stride + row];
                ptr::copy_nonoverlapping(src.as_ptr(), dst.add(y * dst_stride), row);
            }
            CVPixelBufferUnlockBaseAddress(&buffer, flags);
        }
        Ok(buffer)
    }
}

impl Encoder for VideoToolboxEncoder {
    fn name(&self) -> &'static str {
        "videotoolbox"
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
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err(CodecError::Invalid(
                "frame size does not match the encoder".into(),
            ));
        }
        let started = Instant::now();
        let buffer = self.pixel_buffer(frame)?;
        #[allow(clippy::cast_possible_truncation)]
        let frame_us = (f64::from(TIMESCALE) / f64::from(self.config.fps)) as i64;
        let index = i64::try_from(self.frames).unwrap_or(i64::MAX);
        // SAFETY: CMTimeMake is a pure function of its arguments.
        let (pts, duration) = unsafe {
            (
                CMTime::new(index.saturating_mul(frame_us), TIMESCALE),
                CMTime::new(frame_us, TIMESCALE),
            )
        };
        let props: Option<CFRetained<CFDictionary<CFType, CFType>>> = force_keyframe.then(|| {
            // SAFETY: the key is a valid CFString constant.
            let key: &CFType = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            let value: &CFType = CFBoolean::new(true);
            CFDictionary::from_slices(&[key], &[value])
        });
        if let Ok(mut slot) = self.sink.slot.lock() {
            *slot = None;
        }
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: session and buffer are live; `flags` is a valid out-pointer.
        let code = unsafe {
            self.session.encode_frame(
                &buffer,
                pts,
                duration,
                props.as_deref().map(CFDictionary::as_opaque),
                ptr::null_mut(),
                &mut flags,
            )
        };
        status("VTCompressionSessionEncodeFrame", code)?;
        // SAFETY: session is live. Blocks until the callback for this frame has run.
        status("VTCompressionSessionCompleteFrames", unsafe {
            self.session.complete_frames(pts)
        })?;
        self.frames += 1;
        let produced = self
            .sink
            .slot
            .lock()
            .map_err(|_| CodecError::Encode("encoder output lock poisoned".into()))?
            .take()
            .ok_or_else(|| CodecError::Encode("encoder produced no output callback".into()))?
            .map_err(CodecError::Encode)?;
        if produced.is_empty() {
            return Ok(None);
        }
        let keyframe = contains_idr(&produced);
        Ok(Some(EncodedFrame {
            codec: VideoCodec::H264,
            keyframe,
            data: Bytes::from(produced),
            width: frame.width,
            height: frame.height,
            captured_us: frame.captured_us,
            encode_time: started.elapsed(),
        }))
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), CodecError> {
        let mut next = self.config.clone();
        next.bitrate_bps = bitrate_bps;
        next.validate()?;
        let bitrate = i32::try_from(bitrate_bps).unwrap_or(i32::MAX);
        // SAFETY: valid constant key; live session. VideoToolbox applies this without a restart.
        unsafe {
            set(
                &self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i32(bitrate),
            )?
        };
        self.config = next;
        Ok(())
    }
}

impl Drop for VideoToolboxEncoder {
    fn drop(&mut self) {
        // SAFETY: invalidating first guarantees no callback runs after `sink` is freed.
        unsafe { self.session.invalidate() };
    }
}

fn contains_idr(annexb: &[u8]) -> bool {
    annexb
        .windows(4)
        .enumerate()
        .any(|(i, w)| w == [0, 0, 0, 1] && annexb.get(i + 4).is_some_and(|b| b & 0x1f == 5))
}

#[cfg(all(test, feature = "software"))]
mod tests {
    use super::*;
    use crate::h264::H264Decoder;
    use crate::{Decoder, FrameFormat, i420_to_bgra, psnr_bgra};

    fn frame(w: u32, h: u32, shift: u32) -> Frame {
        let mut data = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                if (x / 6 + y / 10) % 3 == 0 && y % 10 < 7
                    || (x >= shift && x < shift + 24 && (20..44).contains(&y))
                {
                    let i = ((y * w + x) * 4) as usize;
                    data[i..i + 3].copy_from_slice(&[10, 10, 10]);
                }
            }
        }
        Frame {
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: FrameFormat::Bgra8,
            data: Bytes::from(data),
            captured_us: 7,
            dirty: vec![],
        }
    }

    #[test]
    fn hardware_stream_decodes_with_the_software_decoder() {
        let (w, h) = (640, 360);
        let mut enc = match VideoToolboxEncoder::new(EncoderConfig::screen(w, h, 60.0, 6_000_000)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("VideoToolbox unavailable here ({e}); skipping");
                return;
            }
        };
        let mut dec = H264Decoder::new().unwrap();
        let mut decoded = 0;
        for i in 0..10 {
            let f = frame(w, h, i * 8);
            if let Some(out) = enc.encode(&f, false).unwrap() {
                assert_eq!(out.keyframe, i == 0);
                if let Some(pic) = dec.decode(&out.data).unwrap() {
                    decoded += 1;
                    let back = i420_to_bgra(w, h, &pic.planes).unwrap();
                    assert!(psnr_bgra(&f.data, &back) > 26.0);
                }
            }
        }
        assert!(decoded >= 8, "decoded {decoded} of 10");
    }
}
