//! VP8 screen codec and colour conversion using the system libvpx.
//!
//! Encoded video stays on the native WebRTC media path. This module accepts
//! packed BGRA frames from a native capture backend, scales them to the
//! negotiated profile, converts to I420 and produces VP8 payloads. The reverse
//! path decodes complete VP8 frames so the call actor can make a small UI
//! preview.
//!
//! libvpx is found through pkg-config on Linux and vcpkg on Windows; bindings
//! are generated against the installed headers. Linux packages must provide libvpx
//! development files at build time and the runtime library at run time. libyuv
//! is optional: when its shared library is found, SIMD scaling and conversion
//! are used; otherwise the Rust fallback keeps the build portable.

#![allow(unsafe_code)] // single audited FFI boundary for libvpx
#![allow(unsafe_op_in_unsafe_fn)] // helpers keep their pointer contracts documented

use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::ptr;
use std::time::Instant;

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unused_imports,
    clippy::all
)]
mod vpx_ffi {
    include!(concat!(env!("OUT_DIR"), "/vpx_ffi.rs"));
}

use vpx_ffi as vpx;

#[cfg(concord_libyuv)]
mod libyuv_ffi {
    use std::os::raw::{c_int, c_uchar};

    pub const K_FILTER_BILINEAR: c_int = 2;

    unsafe extern "C" {
        pub fn ARGBScale(
            src_argb: *const c_uchar,
            src_stride_argb: c_int,
            src_width: c_int,
            src_height: c_int,
            dst_argb: *mut c_uchar,
            dst_stride_argb: c_int,
            dst_width: c_int,
            dst_height: c_int,
            filtering: c_int,
        ) -> c_int;

        pub fn ARGBToI420(
            src_argb: *const c_uchar,
            src_stride_argb: c_int,
            dst_y: *mut c_uchar,
            dst_stride_y: c_int,
            dst_u: *mut c_uchar,
            dst_stride_u: c_int,
            dst_v: *mut c_uchar,
            dst_stride_v: c_int,
            width: c_int,
            height: c_int,
        ) -> c_int;
    }
}

use crate::{CapturedFrame, MediaError, PixelFormat, VideoProfile};

/// A complete VP8 frame ready for RTP packetization.
#[derive(Debug, Clone)]
pub struct EncodedVideoFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub captured_at: Instant,
}

/// Planar 4:2:0 frame. All planes are tightly packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I420Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420Frame {
    pub fn new(
        width: u32,
        height: u32,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    ) -> Result<Self, MediaError> {
        let frame = Self {
            width,
            height,
            y,
            u,
            v,
        };
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), MediaError> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err(MediaError::InvalidSettings(
                "I420 dimensions must be non-zero and even",
            ));
        }
        let luma = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(MediaError::InvalidSettings("I420 dimensions overflow"))?;
        let chroma = luma / 4;
        if self.y.len() != luma || self.u.len() != chroma || self.v.len() != chroma {
            return Err(MediaError::InvalidSettings(
                "I420 planes have the wrong size",
            ));
        }
        Ok(())
    }

    /// Convert limited-range BT.601 I420 to packed BGRA for the screen preview.
    pub fn to_bgra(&self) -> Result<Vec<u8>, MediaError> {
        self.validate()?;
        let width = self.width as usize;
        let height = self.height as usize;
        let chroma_width = width / 2;
        let mut output = vec![0u8; width * height * 4];
        for row in 0..height {
            for column in 0..width {
                let y = self.y[row * width + column] as i32;
                let u = self.u[(row / 2) * chroma_width + column / 2] as i32 - 128;
                let v = self.v[(row / 2) * chroma_width + column / 2] as i32 - 128;
                let c = 298 * (y - 16);
                let red = clamp_u8((c + 409 * v + 128) >> 8);
                let green = clamp_u8((c - 100 * u - 208 * v + 128) >> 8);
                let blue = clamp_u8((c + 516 * u + 128) >> 8);
                let offset = (row * width + column) * 4;
                output[offset] = blue;
                output[offset + 1] = green;
                output[offset + 2] = red;
                output[offset + 3] = 255;
            }
        }
        Ok(output)
    }
}

#[cfg(concord_libyuv)]
fn libyuv_bgra_to_i420(data: &[u8], width: usize, height: usize) -> Option<I420Frame> {
    let width_i32 = i32::try_from(width).ok()?;
    let height_i32 = i32::try_from(height).ok()?;
    let source_stride = width_i32.checked_mul(4)?;
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let chroma_size = chroma_width.checked_mul(chroma_height)?;
    let mut y_plane = vec![0u8; width.checked_mul(height)?];
    let mut u_plane = vec![0u8; chroma_size];
    let mut v_plane = vec![0u8; chroma_size];
    // SAFETY: all buffers are sized from the same validated dimensions and
    // remain alive for the duration of the call. libyuv only writes into the
    // output planes.
    let result = unsafe {
        libyuv_ffi::ARGBToI420(
            data.as_ptr(),
            source_stride,
            y_plane.as_mut_ptr(),
            width_i32,
            u_plane.as_mut_ptr(),
            i32::try_from(chroma_width).ok()?,
            v_plane.as_mut_ptr(),
            i32::try_from(chroma_width).ok()?,
            width_i32,
            height_i32,
        )
    };
    if result != 0 {
        return None;
    }
    I420Frame::new(width as u32, height as u32, y_plane, u_plane, v_plane).ok()
}

#[cfg(concord_libyuv)]
fn libyuv_scale(
    data: &[u8],
    source_width: usize,
    source_height: usize,
    target_width: usize,
    target_height: usize,
) -> Option<Vec<u8>> {
    let source_width_i32 = i32::try_from(source_width).ok()?;
    let source_height_i32 = i32::try_from(source_height).ok()?;
    let target_width_i32 = i32::try_from(target_width).ok()?;
    let target_height_i32 = i32::try_from(target_height).ok()?;
    let source_stride = source_width_i32.checked_mul(4)?;
    let target_stride = target_width_i32.checked_mul(4)?;
    let target_len = target_width.checked_mul(target_height)?.checked_mul(4)?;
    let mut output = vec![0u8; target_len];
    // SAFETY: the source slice is at least source_width * source_height * 4
    // bytes and output is exactly target_width * target_height * 4 bytes.
    let result = unsafe {
        libyuv_ffi::ARGBScale(
            data.as_ptr(),
            source_stride,
            source_width_i32,
            source_height_i32,
            output.as_mut_ptr(),
            target_stride,
            target_width_i32,
            target_height_i32,
            libyuv_ffi::K_FILTER_BILINEAR,
        )
    };
    (result == 0).then_some(output)
}

/// Convert a packed BGRA image to limited-range BT.601 I420.
pub fn bgra_to_i420(data: &[u8], width: u32, height: u32) -> Result<I420Frame, MediaError> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(MediaError::InvalidSettings(
            "video dimensions must be non-zero and even",
        ));
    }
    let width = width as usize;
    let height = height as usize;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(MediaError::InvalidSettings("video dimensions overflow"))?;
    if data.len() < expected {
        return Err(MediaError::InvalidSettings(
            "BGRA buffer is smaller than the frame",
        ));
    }

    #[cfg(concord_libyuv)]
    if let Some(frame) = libyuv_bgra_to_i420(data, width, height) {
        return Ok(frame);
    }

    let luma_size = width * height;
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let mut y_plane = vec![0u8; luma_size];
    let mut u_plane = vec![0u8; chroma_width * chroma_height];
    let mut v_plane = vec![0u8; chroma_width * chroma_height];

    for row in 0..height {
        for column in 0..width {
            let offset = (row * width + column) * 4;
            let blue = data[offset] as i32;
            let green = data[offset + 1] as i32;
            let red = data[offset + 2] as i32;
            y_plane[row * width + column] =
                clamp_u8(((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16);
        }
    }

    for row in 0..chroma_height {
        for column in 0..chroma_width {
            let mut red = 0i32;
            let mut green = 0i32;
            let mut blue = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let offset = ((row * 2 + dy) * width + column * 2 + dx) * 4;
                    blue += data[offset] as i32;
                    green += data[offset + 1] as i32;
                    red += data[offset + 2] as i32;
                }
            }
            red /= 4;
            green /= 4;
            blue /= 4;
            let index = row * chroma_width + column;
            u_plane[index] = clamp_u8(((-38 * red - 74 * green + 112 * blue + 128) >> 8) + 128);
            v_plane[index] = clamp_u8(((112 * red - 94 * green - 18 * blue + 128) >> 8) + 128);
        }
    }

    Ok(I420Frame {
        width: width as u32,
        height: height as u32,
        y: y_plane,
        u: u_plane,
        v: v_plane,
    })
}

/// Bilinear BGRA scaling. The output is tightly packed and opaque.
pub fn scale_bgra(
    data: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, MediaError> {
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return Err(MediaError::InvalidSettings("cannot scale an empty frame"));
    }
    let source_width = source_width as usize;
    let source_height = source_height as usize;
    let target_width = target_width as usize;
    let target_height = target_height as usize;
    let expected = source_width
        .checked_mul(source_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(MediaError::InvalidSettings("source dimensions overflow"))?;
    if data.len() < expected {
        return Err(MediaError::InvalidSettings(
            "BGRA buffer is smaller than the source frame",
        ));
    }
    if source_width == target_width && source_height == target_height {
        return Ok(data[..expected].to_vec());
    }

    #[cfg(concord_libyuv)]
    if let Some(output) = libyuv_scale(
        data,
        source_width,
        source_height,
        target_width,
        target_height,
    ) {
        return Ok(output);
    }

    let mut output = vec![0u8; target_width * target_height * 4];
    let x_scale = if target_width > 1 {
        (source_width.saturating_sub(1) as f32) / (target_width.saturating_sub(1) as f32)
    } else {
        0.0
    };
    let y_scale = if target_height > 1 {
        (source_height.saturating_sub(1) as f32) / (target_height.saturating_sub(1) as f32)
    } else {
        0.0
    };

    for target_y in 0..target_height {
        let source_y = target_y as f32 * y_scale;
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let fy = source_y - y0 as f32;
        for target_x in 0..target_width {
            let source_x = target_x as f32 * x_scale;
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let fx = source_x - x0 as f32;
            let top_left = (y0 * source_width + x0) * 4;
            let top_right = (y0 * source_width + x1) * 4;
            let bottom_left = (y1 * source_width + x0) * 4;
            let bottom_right = (y1 * source_width + x1) * 4;
            let offset = (target_y * target_width + target_x) * 4;
            for channel in 0..3 {
                let top = data[top_left + channel] as f32 * (1.0 - fx)
                    + data[top_right + channel] as f32 * fx;
                let bottom = data[bottom_left + channel] as f32 * (1.0 - fx)
                    + data[bottom_right + channel] as f32 * fx;
                let value = top * (1.0 - fy) + bottom * fy;
                output[offset + channel] = clamp_u8((value + 0.5) as i32);
            }
            output[offset + 3] = 255;
        }
    }
    Ok(output)
}

/// A scaled BGRA preview paired with its I420 representation.
pub struct PreparedScreenFrame {
    pub i420: I420Frame,
    pub bgra: Vec<u8>,
}

pub fn prepare_screen_frame(
    frame: &CapturedFrame,
    width: u32,
    height: u32,
) -> Result<I420Frame, MediaError> {
    prepare_screen_frame_with_preview(frame, width, height).map(|prepared| prepared.i420)
}

/// Scale and convert a captured screen frame once, returning both the I420
/// frame for VP8 and the scaled BGRA buffer for the local preview.
pub fn prepare_screen_frame_with_preview(
    frame: &CapturedFrame,
    width: u32,
    height: u32,
) -> Result<PreparedScreenFrame, MediaError> {
    if frame.format != PixelFormat::Bgra8 {
        return Err(MediaError::InvalidSettings(
            "screen capture did not produce BGRA",
        ));
    }
    frame.validate()?;
    let bgra = if frame.width == width && frame.height == height {
        let row_bytes = width as usize * 4;
        if frame.stride as usize == row_bytes {
            frame.data[..row_bytes * height as usize].to_vec()
        } else {
            repack_bgra(
                &frame.data,
                frame.width,
                frame.height,
                frame.stride,
                width,
                height,
            )?
        }
    } else {
        let stride = frame.stride as usize;
        let row_bytes = frame.width as usize * 4;
        if stride == row_bytes {
            scale_bgra(&frame.data, frame.width, frame.height, width, height)?
        } else {
            let mut tight = vec![0u8; row_bytes * frame.height as usize];
            for row in 0..frame.height as usize {
                let source = row * stride;
                tight[row * row_bytes..(row + 1) * row_bytes]
                    .copy_from_slice(&frame.data[source..source + row_bytes]);
            }
            scale_bgra(&tight, frame.width, frame.height, width, height)?
        }
    };
    let i420 = bgra_to_i420(&bgra, width, height)?;
    Ok(PreparedScreenFrame { i420, bgra })
}

/// VP8 encoder with a realtime, screen-content-oriented configuration.
pub struct Vp8Encoder {
    context: vpx::vpx_codec_ctx_t,
    image: *mut vpx::vpx_image_t,
    width: u32,
    height: u32,
    pts: i64,
    keyframe_interval: u32,
    frames_since_keyframe: u32,
    initialized: bool,
}

// SAFETY: the encoder owns its libvpx context and image allocation and is only
// moved between threads, never shared concurrently. libvpx contexts are not
// thread-safe, so the owning thread must remain exclusive.
#[allow(unsafe_code)]
unsafe impl Send for Vp8Encoder {}

impl Vp8Encoder {
    pub fn new(profile: VideoProfile) -> Result<Self, MediaError> {
        profile.validate()?;
        let mut config = MaybeUninit::<vpx::vpx_codec_enc_cfg_t>::zeroed();
        // SAFETY: the interface pointer is static libvpx data and config is a
        // valid output buffer. The return code is checked below.
        let result = unsafe {
            vpx::vpx_codec_enc_config_default(vpx::vpx_codec_vp8_cx(), config.as_mut_ptr(), 0)
        };
        if result != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(MediaError::Codec(
                "could not load the VP8 defaults".to_owned(),
            ));
        }
        // SAFETY: the call above initialized every field on success.
        let mut config = unsafe { config.assume_init() };
        config.g_w = profile.width;
        config.g_h = profile.height;
        config.g_timebase.num = 1;
        config.g_timebase.den = profile.fps as i32;
        config.g_threads = 1;
        config.g_lag_in_frames = 0;
        config.g_error_resilient = vpx::VPX_ERROR_RESILIENT_DEFAULT;
        config.rc_end_usage = vpx::vpx_rc_mode::VPX_CBR;
        config.rc_target_bitrate = profile.max_bitrate_kbps;
        config.rc_min_quantizer = 4;
        config.rc_max_quantizer = 52;
        config.rc_buf_sz = 1_000;
        config.rc_buf_initial_sz = 500;
        config.rc_buf_optimal_sz = 600;
        config.rc_dropframe_thresh = 5;
        config.kf_mode = vpx::vpx_kf_mode::VPX_KF_AUTO;
        config.kf_min_dist = 0;
        config.kf_max_dist = (profile.fps * 2).max(1);

        let mut context = MaybeUninit::<vpx::vpx_codec_ctx_t>::zeroed();
        // SAFETY: the context is an output buffer, the config remains alive for
        // the duration of the call, and the ABI constant matches the bindings.
        let result = unsafe {
            vpx::vpx_codec_enc_init_ver(
                context.as_mut_ptr(),
                vpx::vpx_codec_vp8_cx(),
                &config,
                0,
                vpx::VPX_ENCODER_ABI_VERSION as i32,
            )
        };
        if result != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(MediaError::Codec(format!(
                "could not initialize the VP8 encoder: {result:?}"
            )));
        }
        // SAFETY: the call above initialized the context on success.
        let mut context = unsafe { context.assume_init() };

        // Real-time speed 8 is the libvpx sweet spot for screen sharing on one
        // CPU thread. Screen-content mode improves text at a small quality cost.
        unsafe {
            vpx::vpx_codec_control_(
                &mut context,
                vpx::vp8e_enc_control_id::VP8E_SET_CPUUSED as i32,
                8 as std::os::raw::c_int,
            );
            vpx::vpx_codec_control_(
                &mut context,
                vpx::vp8e_enc_control_id::VP8E_SET_SCREEN_CONTENT_MODE as i32,
                1 as std::os::raw::c_int,
            );
            vpx::vpx_codec_control_(
                &mut context,
                vpx::vp8e_enc_control_id::VP8E_SET_STATIC_THRESHOLD as i32,
                0 as std::os::raw::c_int,
            );
            vpx::vpx_codec_control_(
                &mut context,
                vpx::vp8e_enc_control_id::VP8E_SET_MAX_INTRA_BITRATE_PCT as i32,
                800 as std::os::raw::c_int,
            );
        }

        let image = unsafe {
            vpx::vpx_img_alloc(
                ptr::null_mut(),
                vpx::vpx_img_fmt::VPX_IMG_FMT_I420,
                profile.width,
                profile.height,
                32,
            )
        };
        if image.is_null() {
            unsafe {
                vpx::vpx_codec_destroy(&mut context);
            }
            return Err(MediaError::Codec(
                "could not allocate the VP8 input image".to_owned(),
            ));
        }

        Ok(Self {
            context,
            image,
            width: profile.width,
            height: profile.height,
            pts: 0,
            keyframe_interval: profile.fps.max(1),
            frames_since_keyframe: 0,
            initialized: true,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Encode one I420 frame. A keyframe is forced at startup and once per
    /// second so a viewer that joins late can synchronise without waiting for
    /// libvpx's two-second automatic interval.
    pub fn encode(
        &mut self,
        frame: &I420Frame,
        force_keyframe: bool,
    ) -> Result<Option<EncodedVideoFrame>, MediaError> {
        if frame.validate().is_err() {
            return Err(MediaError::InvalidSettings("invalid I420 frame"));
        }
        if frame.width != self.width || frame.height != self.height {
            return Err(MediaError::InvalidSettings(
                "encoded frame does not match the encoder profile",
            ));
        }
        self.copy_planes(frame)?;

        let automatic = self.frames_since_keyframe >= self.keyframe_interval;
        let request_keyframe = force_keyframe || self.pts == 0 || automatic;
        let flags = if request_keyframe {
            vpx::VPX_EFLAG_FORCE_KF as vpx::vpx_enc_frame_flags_t
        } else {
            0
        };
        // SAFETY: image is the allocation owned by this encoder and remains
        // valid until Drop; the context is initialized.
        let result = unsafe {
            vpx::vpx_codec_encode(
                &mut self.context,
                self.image,
                self.pts,
                1,
                flags,
                // C unsigned long is 32-bit on Windows and 64-bit on Linux.
                vpx::VPX_DL_REALTIME as std::os::raw::c_ulong,
            )
        };
        if result != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(MediaError::Codec(format!(
                "VP8 encode failed: {}",
                codec_error(&mut self.context)
            )));
        }
        self.pts = self.pts.wrapping_add(1);

        let mut iter: vpx::vpx_codec_iter_t = ptr::null();
        let mut encoded = None;
        loop {
            // SAFETY: iter is owned locally and the context is valid.
            let packet = unsafe { vpx::vpx_codec_get_cx_data(&mut self.context, &mut iter) };
            if packet.is_null() {
                break;
            }
            // SAFETY: libvpx returned a valid packet for this iteration.
            let packet = unsafe { &*packet };
            if packet.kind != vpx::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                continue;
            }
            // SAFETY: the kind tag above guarantees the frame arm is active.
            let frame = unsafe { packet.data.frame };
            let data = if frame.buf.is_null() || frame.sz == 0 {
                Vec::new()
            } else {
                // SAFETY: libvpx guarantees buf points to sz readable bytes for
                // the lifetime of this packet.
                unsafe { std::slice::from_raw_parts(frame.buf as *const u8, frame.sz) }.to_vec()
            };
            if data.is_empty() {
                continue;
            }
            let keyframe = frame.flags & vpx::VPX_FRAME_IS_KEY != 0;
            encoded = Some(EncodedVideoFrame {
                data,
                keyframe,
                width: self.width,
                height: self.height,
                captured_at: Instant::now(),
            });
        }

        if encoded.as_ref().is_some_and(|frame| frame.keyframe) {
            self.frames_since_keyframe = 0;
        } else {
            self.frames_since_keyframe = self.frames_since_keyframe.saturating_add(1);
        }
        Ok(encoded)
    }

    fn copy_planes(&mut self, frame: &I420Frame) -> Result<(), MediaError> {
        if self.image.is_null() {
            return Err(MediaError::Codec("VP8 image was released".to_owned()));
        }
        // SAFETY: the image pointer is owned by this encoder and initialized.
        let image = unsafe { &mut *self.image };
        copy_plane(image, 0, &frame.y, self.width, self.height)?;
        copy_plane(image, 1, &frame.u, self.width / 2, self.height / 2)?;
        copy_plane(image, 2, &frame.v, self.width / 2, self.height / 2)?;
        Ok(())
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        unsafe {
            if !self.image.is_null() {
                vpx::vpx_img_free(self.image);
                self.image = ptr::null_mut();
            }
            vpx::vpx_codec_destroy(&mut self.context);
        }
        self.initialized = false;
    }
}

/// VP8 decoder for screen previews. It expects complete frames, not RTP
/// packets; the RTC layer reassembles and strips the VP8 payload descriptor.
pub struct Vp8Decoder {
    context: vpx::vpx_codec_ctx_t,
    initialized: bool,
}

// SAFETY: see `Vp8Encoder`; libvpx contexts must remain exclusive to a thread.
#[allow(unsafe_code)]
unsafe impl Send for Vp8Decoder {}

impl Vp8Decoder {
    pub fn new() -> Result<Self, MediaError> {
        let mut context = MaybeUninit::<vpx::vpx_codec_ctx_t>::zeroed();
        let config = vpx::vpx_codec_dec_cfg_t {
            threads: 1,
            w: 0,
            h: 0,
        };
        // SAFETY: context is an output buffer, config lives for the call and
        // the ABI constant matches the generated bindings.
        let result = unsafe {
            vpx::vpx_codec_dec_init_ver(
                context.as_mut_ptr(),
                vpx::vpx_codec_vp8_dx(),
                &config,
                0,
                vpx::VPX_DECODER_ABI_VERSION as i32,
            )
        };
        if result != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            return Err(MediaError::Codec(format!(
                "could not initialize the VP8 decoder: {result:?}"
            )));
        }
        // SAFETY: the call above initialized the context on success.
        let context = unsafe { context.assume_init() };
        Ok(Self {
            context,
            initialized: true,
        })
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Option<I420Frame>, MediaError> {
        if data.is_empty() {
            return Ok(None);
        }
        // SAFETY: data is a readable slice and the context is initialized.
        let result = unsafe {
            vpx::vpx_codec_decode(
                &mut self.context,
                data.as_ptr(),
                data.len().try_into().map_err(|_| {
                    MediaError::InvalidSettings("VP8 frame is too large for libvpx")
                })?,
                ptr::null_mut(),
                0,
            )
        };
        if result != vpx::vpx_codec_err_t::VPX_CODEC_OK {
            if result == vpx::vpx_codec_err_t::VPX_CODEC_CORRUPT_FRAME {
                return Ok(None);
            }
            return Err(MediaError::Codec(format!(
                "VP8 decode failed: {}",
                codec_error(&mut self.context)
            )));
        }

        let mut iter: vpx::vpx_codec_iter_t = ptr::null();
        let mut decoded = None;
        loop {
            // SAFETY: iter is local and the context is valid.
            let image = unsafe { vpx::vpx_codec_get_frame(&mut self.context, &mut iter) };
            if image.is_null() {
                break;
            }
            // SAFETY: libvpx returned a valid image for this iteration.
            let frame = unsafe { copy_image_to_i420(&*image)? };
            decoded = Some(frame);
        }
        Ok(decoded)
    }
}

impl Drop for Vp8Decoder {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                vpx::vpx_codec_destroy(&mut self.context);
            }
            self.initialized = false;
        }
    }
}

#[allow(unsafe_code)]
fn copy_plane(
    image: &mut vpx::vpx_image_t,
    plane: usize,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<(), MediaError> {
    if image.planes[plane].is_null() || image.stride[plane] <= 0 {
        return Err(MediaError::Codec("VP8 image plane is missing".to_owned()));
    }
    let stride = image.stride[plane] as usize;
    if stride < width as usize {
        return Err(MediaError::Codec(
            "VP8 image stride is too small".to_owned(),
        ));
    }
    for row in 0..height as usize {
        let destination = unsafe { image.planes[plane].add(row * stride) };
        unsafe {
            ptr::copy_nonoverlapping(
                data[row * width as usize..(row + 1) * width as usize].as_ptr(),
                destination,
                width as usize,
            );
        }
    }
    Ok(())
}

#[allow(unsafe_code)]
unsafe fn copy_image_to_i420(image: &vpx::vpx_image_t) -> Result<I420Frame, MediaError> {
    // `w`/`h` include libvpx's allocation padding; `d_w`/`d_h` are the actual
    // displayed dimensions of the decoded frame.
    let width = image.d_w;
    let height = image.d_h;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(MediaError::Codec(
            "VP8 decoder returned an invalid frame size".to_owned(),
        ));
    }
    if image.planes[0].is_null() || image.planes[1].is_null() || image.planes[2].is_null() {
        return Err(MediaError::Codec(
            "VP8 decoder returned an empty image".to_owned(),
        ));
    }
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let mut y = vec![0u8; width as usize * height as usize];
    let mut u = vec![0u8; chroma_width as usize * chroma_height as usize];
    let mut v = vec![0u8; chroma_width as usize * chroma_height as usize];
    for row in 0..height as usize {
        let source = image.planes[0].add(row * image.stride[0] as usize);
        ptr::copy_nonoverlapping(
            source,
            y[row * width as usize..].as_mut_ptr(),
            width as usize,
        );
    }
    for row in 0..chroma_height as usize {
        let source = image.planes[1].add(row * image.stride[1] as usize);
        ptr::copy_nonoverlapping(
            source,
            u[row * chroma_width as usize..].as_mut_ptr(),
            chroma_width as usize,
        );
        let source = image.planes[2].add(row * image.stride[2] as usize);
        ptr::copy_nonoverlapping(
            source,
            v[row * chroma_width as usize..].as_mut_ptr(),
            chroma_width as usize,
        );
    }
    Ok(I420Frame {
        width,
        height,
        y,
        u,
        v,
    })
}

fn repack_bgra(
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, MediaError> {
    let row_bytes = width as usize * 4;
    if (stride as usize) < row_bytes {
        return Err(MediaError::InvalidSettings("BGRA stride is too small"));
    }
    let mut tight = vec![0u8; row_bytes * height as usize];
    for row in 0..height as usize {
        let source = row * stride as usize;
        let destination = row * row_bytes;
        let available = data.len().saturating_sub(source);
        let copy_bytes = row_bytes.min(available);
        tight[destination..destination + copy_bytes]
            .copy_from_slice(&data[source..source + copy_bytes]);
    }
    if width == target_width && height == target_height {
        Ok(tight)
    } else {
        scale_bgra(&tight, width, height, target_width, target_height)
    }
}

fn codec_error(context: &mut vpx::vpx_codec_ctx_t) -> String {
    unsafe {
        let detail = vpx::vpx_codec_error_detail(context);
        if !detail.is_null() {
            let detail = CStr::from_ptr(detail).to_string_lossy();
            if !detail.is_empty() {
                return detail.into_owned();
            }
        }
        let error = vpx::vpx_codec_error(context);
        if error.is_null() {
            "unknown libvpx error".to_owned()
        } else {
            CStr::from_ptr(error).to_string_lossy().into_owned()
        }
    }
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{PROFILE_720P30, PROFILE_720P60, PROFILE_1080P30, PROFILE_1080P60};

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        data
    }

    #[test]
    fn converts_and_roundtrips_solid_colour() {
        let data = [0u8, 0, 255, 255].repeat(16); // BGRA red
        let frame = bgra_to_i420(&data, 4, 4).expect("i420");
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 4);
        let bgra = frame.to_bgra().expect("bgra");
        assert!(bgra[2] > 200);
        assert!(bgra[1] < 100);
        assert!(bgra[0] < 100);
    }

    #[test]
    fn scales_to_the_requested_size() {
        let scaled = scale_bgra(&gradient(8, 6), 8, 6, 4, 3).expect("scale");
        assert_eq!(scaled.len(), 4 * 3 * 4);
        assert!(scaled.iter().any(|value| *value != 0));
        assert!(
            scaled
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] >= 250)
        );
    }

    #[test]
    fn vp8_encodes_and_decodes_a_keyframe() {
        let width = 64;
        let height = 48;
        let source = gradient(width, height);
        let i420 = bgra_to_i420(&source, width, height).expect("i420");
        let mut encoder = Vp8Encoder::new(VideoProfile {
            width,
            height,
            fps: 30,
            max_bitrate_kbps: 800,
        })
        .expect("encoder");
        let encoded = encoder
            .encode(&i420, true)
            .expect("encode")
            .expect("packet");
        assert!(encoded.keyframe);
        assert!(!encoded.data.is_empty());

        let mut decoder = Vp8Decoder::new().expect("decoder");
        let decoded = decoder
            .decode(&encoded.data)
            .expect("decode")
            .expect("frame");
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.y.len(), (width * height) as usize);
    }

    #[test]
    fn screen_frame_is_scaled_then_converted() {
        let frame = CapturedFrame {
            width: 8,
            height: 6,
            stride: 32,
            format: PixelFormat::Bgra8,
            data: gradient(8, 6),
            captured_at: Instant::now(),
        };
        let prepared = prepare_screen_frame_with_preview(&frame, 4, 4).expect("prepared");
        assert_eq!((prepared.i420.width, prepared.i420.height), (4, 4));
        assert_eq!(prepared.i420.y.len(), 16);
        assert_eq!(prepared.bgra.len(), 4 * 4 * 4);
        assert!(
            prepared
                .bgra
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] >= 250)
        );
    }

    #[test]
    fn profile_dimensions_are_preserved_by_the_encoder() {
        let encoder = Vp8Encoder::new(PROFILE_720P30).expect("encoder");
        assert_eq!(encoder.dimensions(), (1280, 720));
    }

    #[test]
    #[ignore = "manual timing probe for the screen pipeline"]
    fn screen_pipeline_timing_probe() {
        let frame = CapturedFrame {
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            format: PixelFormat::Bgra8,
            data: gradient(1920, 1080),
            captured_at: Instant::now(),
        };
        for profile in [
            PROFILE_720P30,
            PROFILE_720P60,
            PROFILE_1080P30,
            PROFILE_1080P60,
        ] {
            let mut encoder = Vp8Encoder::new(profile).expect("encoder");
            let mut decoder = Vp8Decoder::new().expect("decoder");
            let mut prepare_time = std::time::Duration::ZERO;
            let mut encode_time = std::time::Duration::ZERO;
            let mut decode_time = std::time::Duration::ZERO;
            let mut frames = 0u32;
            for index in 0..60 {
                let started = Instant::now();
                let prepared =
                    prepare_screen_frame_with_preview(&frame, profile.width, profile.height)
                        .expect("prepare");
                assert_eq!(
                    prepared.bgra.len(),
                    profile.width as usize * profile.height as usize * 4
                );
                prepare_time += started.elapsed();
                let started = Instant::now();
                let encoded = encoder
                    .encode(&prepared.i420, index == 0)
                    .expect("encode")
                    .expect("packet");
                encode_time += started.elapsed();
                let started = Instant::now();
                let decoded = decoder.decode(&encoded.data).expect("decode");
                assert!(decoded.is_some());
                decode_time += started.elapsed();
                frames += 1;
            }
            println!(
                "screen pipeline {}x{}@{} ({} frames): prepare+preview={:.2}ms encode={:.2}ms decode={:.2}ms total={:.2}ms",
                profile.width,
                profile.height,
                profile.fps,
                frames,
                prepare_time.as_secs_f64() * 1000.0 / f64::from(frames),
                encode_time.as_secs_f64() * 1000.0 / f64::from(frames),
                decode_time.as_secs_f64() * 1000.0 / f64::from(frames),
                (prepare_time + encode_time + decode_time).as_secs_f64() * 1000.0
                    / f64::from(frames)
            );
        }
    }
}
