//! X11/Xwayland screen capture using MIT-SHM.
//!
//! This is the first native backend. It captures a crop of a root window (the
//! monitor), converts it to tightly packed BGRA, and never touches the WebView.
//! Wayland-native windows are only visible when the user is on an X11 session
//! or when Xwayland is showing them; a portal/PipeWire backend is future work.
//!
//! The server writes pixels directly into a System V shared-memory segment via
//! `ShmGetImage`. This avoids copying the frame through the X11 socket and,
//! importantly, works on Xwayland setups where core `GetImage` returns
//! `BadMatch`. Capture is synchronous, so callers must use a dedicated thread.

use std::borrow::Cow;
use std::ffi::c_void;
use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::image::{BitsPerPixel, Image, ImageOrder, PixelLayout, ScanlinePad};
use x11rb::protocol::shm::{ConnectionExt as ShmExt, Seg};
use x11rb::protocol::xproto::{ImageFormat, Screen, VisualClass, Visualid, Visualtype, Window};
use x11rb::rust_connection::RustConnection;

use crate::{CaptureSource, CapturedFrame, MediaError, PixelFormat, ScreenCapturer, VideoProfile};

pub fn list_x11_sources() -> Result<Vec<CaptureSource>, MediaError> {
    let (connection, _) =
        x11rb::connect(None).map_err(|error| capture_error("could not connect to X11", error))?;
    let mut sources = Vec::new();
    for (index, screen) in connection.setup().roots.iter().enumerate() {
        sources.push(CaptureSource::Monitor {
            id: u32::try_from(index)
                .map_err(|_| MediaError::Capture("too many X11 screens".to_owned()))?,
            name: format!(
                "Screen {index} ({}x{})",
                screen.width_in_pixels, screen.height_in_pixels
            ),
        });
    }
    Ok(sources)
}

pub struct X11Capturer {
    connection: RustConnection,
    root: Window,
    root_visual: Visualtype,
    screen_width: u16,
    screen_height: u16,
    width: u16,
    height: u16,
    shm: ShmSegment,
    segment: Seg,
    source: CaptureSource,
    stopped: bool,
}

/// System V shared memory owned by this process. The X server attaches the
/// same segment by its numeric id; it must outlive the server-side attachment.
struct ShmSegment {
    /// Stored as an integer so `X11Capturer` remains `Send` without an unsafe
    /// `Send` implementation. The address is only dereferenced while the
    /// owning capturer is alive.
    address: usize,
    length: usize,
    id: i32,
}

#[allow(unsafe_code)]
impl ShmSegment {
    fn new(length: usize) -> Result<Self, MediaError> {
        // SAFETY: shmget only creates a new IPC object; no pointers cross this
        // call. IPC_PRIVATE makes the segment exclusive to this process until
        // it is attached by the X server.
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, length, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(capture_error(
                "could not create the XShm segment",
                std::io::Error::last_os_error(),
            ));
        }

        // SAFETY: id refers to the segment created above and length is the
        // exact requested size. A null address lets the kernel choose.
        let address = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if address as isize == -1 {
            let error = std::io::Error::last_os_error();
            // SAFETY: id is valid and has not been attached by us yet.
            unsafe {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            }
            return Err(capture_error("could not attach the XShm segment", error));
        }

        Ok(Self {
            address: address as usize,
            length,
            id,
        })
    }

    fn slice(&self) -> &[u8] {
        // SAFETY: the address is the successful shmat result for exactly
        // `length` bytes, and the segment is not detached until Drop.
        unsafe { std::slice::from_raw_parts(self.address as *const u8, self.length) }
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        if self.address == 0 {
            return;
        }
        // SAFETY: this is the pointer returned by shmat and it is detached
        // exactly once here. Removing the IPC id lets the kernel reclaim the
        // segment after both our mapping and the X server detach.
        #[allow(unsafe_code)]
        unsafe {
            libc::shmdt(self.address as *const c_void);
            if self.id >= 0 {
                libc::shmctl(self.id, libc::IPC_RMID, std::ptr::null_mut());
            }
        }
        self.address = 0;
    }
}

impl X11Capturer {
    /// Capture a crop with the exact profile dimensions. This is used by the
    /// benchmark/probe tool and by callers that already know the screen is no
    /// larger than the profile.
    pub fn new(
        source: CaptureSource,
        profile: VideoProfile,
        display: Option<&str>,
    ) -> Result<Self, MediaError> {
        profile.validate()?;
        Self::create(source, Some((profile.width, profile.height)), display)
    }

    /// Capture the whole X11 screen at its native size. The screen-sharing
    /// pipeline scales this frame down to the negotiated profile, so a
    /// 1920×1080 monitor is shown in full at 720p instead of being cropped.
    pub fn new_full_screen(
        source: CaptureSource,
        display: Option<&str>,
    ) -> Result<Self, MediaError> {
        Self::create(source, None, display)
    }

    fn create(
        source: CaptureSource,
        requested: Option<(u32, u32)>,
        display: Option<&str>,
    ) -> Result<Self, MediaError> {
        let screen_id = match &source {
            CaptureSource::Monitor { id, .. } => *id,
            CaptureSource::Window { .. } => {
                return Err(MediaError::InvalidSettings(
                    "X11 window capture is not implemented yet",
                ));
            }
        };

        let (connection, _) = x11rb::connect(display)
            .map_err(|error| capture_error("could not connect to X11", error))?;
        let screen = connection
            .setup()
            .roots
            .get(screen_id as usize)
            .ok_or(MediaError::SourceNotFound)?;
        let root_visual = find_visual(screen, screen.root_visual)?;
        let root = screen.root;
        let screen_width = screen.width_in_pixels;
        let screen_height = screen.height_in_pixels;

        let (width, height) = if let Some((requested_width, requested_height)) = requested {
            let width = u16::try_from(requested_width)
                .ok()
                .filter(|width| *width <= screen_width)
                .ok_or(MediaError::InvalidSettings(
                    "capture width exceeds the X11 screen",
                ))?;
            let height = u16::try_from(requested_height)
                .ok()
                .filter(|height| *height <= screen_height)
                .ok_or(MediaError::InvalidSettings(
                    "capture height exceeds the X11 screen",
                ))?;
            (width, height)
        } else {
            (screen_width, screen_height)
        };

        let shm_length = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| MediaError::Capture("X11 capture dimensions overflow".to_owned()))?;
        let shm = ShmSegment::new(shm_length)?;
        let segment = connection
            .generate_id()
            .map_err(|error| capture_error("could not allocate an XShm segment id", error))?;
        let shm_id = u32::try_from(shm.id)
            .map_err(|_| MediaError::Capture("invalid XShm segment id".to_owned()))?;
        connection
            .shm_attach(segment, shm_id, false)
            .map_err(|error| capture_error("could not attach the XShm segment", error))?
            .check()
            .map_err(|error| capture_error("X11 rejected the XShm segment", error))?;

        Ok(Self {
            connection,
            root,
            root_visual,
            screen_width,
            screen_height,
            width,
            height,
            shm,
            segment,
            source,
            stopped: false,
        })
    }

    pub fn screen_size(&self) -> (u16, u16) {
        (self.screen_width, self.screen_height)
    }

    pub fn capture_size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn next_image(&self) -> Result<CapturedFrame, MediaError> {
        let reply = self
            .connection
            .shm_get_image(
                self.root,
                0,
                0,
                self.width,
                self.height,
                u32::MAX,
                u8::from(ImageFormat::Z_PIXMAP),
                self.segment,
                0,
            )
            .map_err(|error| capture_error("could not request an XShm image", error))?
            .reply()
            .map_err(|error| capture_error("X11 ShmGetImage failed", error))?;

        let setup = self.connection.setup();
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == reply.depth)
            .ok_or_else(|| {
                MediaError::Capture(format!(
                    "X11 has no pixmap format for depth {}",
                    reply.depth
                ))
            })?;
        let scanline_pad = ScanlinePad::try_from(format.scanline_pad)
            .map_err(|error| capture_error("invalid X11 scanline padding", error))?;
        let bits_per_pixel = BitsPerPixel::try_from(format.bits_per_pixel)
            .map_err(|error| capture_error("invalid X11 bits per pixel", error))?;
        let byte_order = ImageOrder::try_from(setup.image_byte_order)
            .map_err(|error| capture_error("invalid X11 image byte order", error))?;

        let image = Image::new(
            self.width,
            self.height,
            scanline_pad,
            reply.depth,
            bits_per_pixel,
            byte_order,
            Cow::Borrowed(self.shm.slice()),
        )
        .map_err(|error| capture_error("X11 returned an invalid image", error))?;

        let visual = if reply.visual == self.root_visual.visual_id {
            self.root_visual
        } else {
            find_visual_by_id(&setup.roots, reply.visual)?
        };
        let layout = PixelLayout::from_visual_type(visual)
            .map_err(|error| capture_error("unsupported X11 visual layout", error))?;
        let data = image_to_bgra(&image, layout, is_standard_rgb_visual(visual))?;

        Ok(CapturedFrame {
            width: u32::from(self.width),
            height: u32::from(self.height),
            stride: u32::from(self.width) * 4,
            format: PixelFormat::Bgra8,
            data,
            captured_at: Instant::now(),
        })
    }
}

impl Drop for X11Capturer {
    fn drop(&mut self) {
        if self.segment == 0 {
            return;
        }
        match self.connection.shm_detach(self.segment) {
            Ok(cookie) => {
                if let Err(error) = cookie.check() {
                    tracing::debug!("could not detach the XShm segment: {error}");
                }
            }
            Err(error) => {
                tracing::debug!("could not send the XShm detach request: {error}");
            }
        }
        self.segment = 0;
    }
}

impl ScreenCapturer for X11Capturer {
    fn source(&self) -> &CaptureSource {
        &self.source
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, MediaError> {
        if self.stopped {
            return Err(MediaError::SourceLost);
        }
        self.next_image()
    }

    fn stop(&mut self) {
        self.stopped = true;
    }
}

fn find_visual(screen: &Screen, visual_id: Visualid) -> Result<Visualtype, MediaError> {
    find_visual_in_screen(screen, visual_id).ok_or_else(|| {
        MediaError::Capture(format!(
            "X11 visual {visual_id} is not advertised by the screen"
        ))
    })
}

fn find_visual_by_id(roots: &[Screen], visual_id: Visualid) -> Result<Visualtype, MediaError> {
    roots
        .iter()
        .find_map(|screen| find_visual_in_screen(screen, visual_id))
        .ok_or_else(|| MediaError::Capture(format!("X11 visual {visual_id} was not found")))
}

fn find_visual_in_screen(screen: &Screen, visual_id: Visualid) -> Option<Visualtype> {
    screen
        .allowed_depths
        .iter()
        .flat_map(|depth| depth.visuals.iter())
        .copied()
        .find(|visual| visual.visual_id == visual_id)
}

fn is_standard_rgb_visual(visual: Visualtype) -> bool {
    visual.class == VisualClass::TRUE_COLOR
        && visual.red_mask == 0x00ff_0000
        && visual.green_mask == 0x0000_ff00
        && visual.blue_mask == 0x0000_00ff
}

fn image_to_bgra(
    image: &Image<'_>,
    layout: PixelLayout,
    standard_rgb: bool,
) -> Result<Vec<u8>, MediaError> {
    let width = usize::from(image.width());
    let height = usize::from(image.height());
    if width == 0 || height == 0 || image.data().len() < width * height {
        return Err(MediaError::Capture(
            "X11 returned an empty image".to_owned(),
        ));
    }

    let stride = image.data().len() / height;

    if standard_rgb && image.bits_per_pixel() == BitsPerPixel::B32 && image.depth() >= 24 {
        let row_bytes = width * 4;
        let mut output = vec![0u8; row_bytes * height];
        for row in 0..height {
            let start = row * stride;
            let end = start + row_bytes;
            let row_data = image
                .data()
                .get(start..end)
                .ok_or_else(|| MediaError::Capture("short X11 scanline".to_owned()))?;
            let out_start = row * row_bytes;
            let out_row = &mut output[out_start..out_start + row_bytes];
            match image.byte_order() {
                ImageOrder::LsbFirst => {
                    // BGRX and BGRA share the same byte layout; only the
                    // unused alpha byte needs to become opaque.
                    out_row.copy_from_slice(row_data);
                    for pixel in out_row.as_chunks_mut::<4>().0 {
                        pixel[3] = 255;
                    }
                }
                ImageOrder::MsbFirst => {
                    for (dst, src) in out_row
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(row_data.as_chunks::<4>().0)
                    {
                        *dst = [src[3], src[2], src[1], 255];
                    }
                }
            }
        }
        return Ok(output);
    }

    let mut output = Vec::with_capacity(width * height * 4);

    for y in 0..u16::try_from(height)
        .map_err(|_| MediaError::Capture("X11 image height does not fit in u16".to_owned()))?
    {
        for x in 0..u16::try_from(width)
            .map_err(|_| MediaError::Capture("X11 image width does not fit in u16".to_owned()))?
        {
            let pixel = image.get_pixel(x, y);
            let (red, green, blue) = layout.decode(pixel);
            output.extend_from_slice(&[
                (blue >> 8) as u8,
                (green >> 8) as u8,
                (red >> 8) as u8,
                255,
            ]);
        }
    }

    Ok(output)
}

fn capture_error(context: &str, error: impl std::fmt::Display) -> MediaError {
    MediaError::Capture(format!("{context}: {error}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use x11rb::image::{ColorComponent, ScanlinePad};

    #[test]
    fn converts_standard_lsb_bgrx_to_bgra() {
        let image = Image::new(
            2,
            1,
            ScanlinePad::Pad8,
            24,
            BitsPerPixel::B32,
            ImageOrder::LsbFirst,
            Cow::Owned(vec![1, 2, 3, 0, 4, 5, 6, 0]),
        )
        .expect("image");
        let layout = PixelLayout::new(
            ColorComponent::new(8, 16).expect("red"),
            ColorComponent::new(8, 8).expect("green"),
            ColorComponent::new(8, 0).expect("blue"),
        );

        let bgra = image_to_bgra(&image, layout, true).expect("convert");
        assert_eq!(bgra, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    #[ignore = "requires an X11 display"]
    fn full_screen_capture_timing_probe() {
        let source = list_x11_sources()
            .expect("list X11 screens")
            .into_iter()
            .find(|source| matches!(source, CaptureSource::Monitor { .. }))
            .expect("monitor");
        let mut capturer = X11Capturer::new_full_screen(source, None).expect("capturer");
        let mut elapsed = std::time::Duration::ZERO;
        let mut frames = 0u32;
        for _ in 0..60 {
            let started = Instant::now();
            let frame = capturer.next_frame().expect("frame");
            frame.validate().expect("valid frame");
            elapsed += started.elapsed();
            frames += 1;
        }
        println!(
            "x11 full-screen capture: {:.2}ms/frame over {frames} frames",
            elapsed.as_secs_f64() * 1000.0 / f64::from(frames)
        );
    }

    #[test]
    fn converts_nonstandard_layout_through_pixel_decode() {
        let image = Image::new(
            1,
            1,
            ScanlinePad::Pad8,
            24,
            BitsPerPixel::B32,
            ImageOrder::LsbFirst,
            Cow::Owned(vec![0x10, 0x20, 0x30, 0]),
        )
        .expect("image");
        let layout = PixelLayout::new(
            ColorComponent::new(8, 0).expect("red"),
            ColorComponent::new(8, 8).expect("green"),
            ColorComponent::new(8, 16).expect("blue"),
        );

        let bgra = image_to_bgra(&image, layout, false).expect("convert");
        assert_eq!(bgra, vec![0x30, 0x20, 0x10, 255]);
    }
}
