//! Wayland capture through the ScreenCast portal and its restricted PipeWire
//! remote. GIO/PipeWire objects remain on a single native worker thread.
//! The shared slot retains only the latest frame; static screens can reuse it
//! at the encoder cadence without treating an idle compositor as a failure.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use crate::{CaptureSource, CapturedFrame, MediaError, PixelFormat, ScreenCapturer};

#[derive(Default)]
struct CaptureState {
    latest: Option<CapturedFrame>,
    error: Option<String>,
}

pub struct WaylandCapturer {
    source: CaptureSource,
    state: Arc<Mutex<CaptureState>>,
    running: Arc<AtomicBool>,
}

impl WaylandCapturer {
    /// Waits for user consent and the first PipeWire frame. Call on a blocking
    /// worker; setting `running` to false cancels a pending portal request.
    pub fn new(running: Arc<AtomicBool>) -> Result<Self, MediaError> {
        let state = Arc::new(Mutex::new(CaptureState::default()));
        let worker_state = state.clone();
        let worker_running = running.clone();
        let (ready, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("concord-pipewire".to_owned())
            .spawn(move || ffi::run(worker_state, worker_running, ready))?;
        result
            .recv()
            .map_err(|_| MediaError::Capture("a thread PipeWire foi encerrada".to_owned()))?
            .map_err(MediaError::Capture)?;
        Ok(Self {
            source: CaptureSource::Monitor {
                id: 0,
                name: "Tela/janela selecionada no portal Wayland".to_owned(),
            },
            state,
            running,
        })
    }
}

impl ScreenCapturer for WaylandCapturer {
    fn source(&self) -> &CaptureSource {
        &self.source
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, MediaError> {
        let state = self
            .state
            .lock()
            .map_err(|_| MediaError::Capture("estado PipeWire indisponível".to_owned()))?;
        if let Some(error) = &state.error {
            return Err(MediaError::Capture(error.clone()));
        }
        let frame = state.latest.as_ref().ok_or(MediaError::SourceLost)?;
        Ok(CapturedFrame {
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            format: frame.format,
            data: frame.data.clone(),
            captured_at: Instant::now(),
        })
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for WaylandCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn copy_frame(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: i32,
    format: u32,
) -> Result<CapturedFrame, MediaError> {
    // Bound allocations and reject negative/short strides before reading any
    // row. MemFd/MemPtr are negotiated; DMA-BUF pointers are never read here.
    if width == 0 || height == 0 || width > 8192 || height > 8192 || format > 3 {
        return Err(MediaError::InvalidSettings(
            "invalid PipeWire dimensions/format",
        ));
    }
    let row = width as usize * 4;
    let stride = usize::try_from(stride)
        .map_err(|_| MediaError::InvalidSettings("negative PipeWire stride"))?;
    let required = stride
        .checked_mul(height as usize - 1)
        .and_then(|offset| offset.checked_add(row))
        .ok_or(MediaError::InvalidSettings("PipeWire frame is too large"))?;
    if stride < row || required > bytes.len() {
        return Err(MediaError::InvalidSettings("truncated PipeWire frame"));
    }
    let mut data = vec![0; row * height as usize];
    for y in 0..height as usize {
        let source = &bytes[y * stride..y * stride + row];
        let target = &mut data[y * row..(y + 1) * row];
        target.copy_from_slice(source);
        for pixel in target.as_chunks_mut::<4>().0 {
            if format >= 2 {
                pixel.swap(0, 2);
            }
            // A screen/window is composited as opaque in the native viewer.
            pixel[3] = 255;
        }
    }
    Ok(CapturedFrame {
        width,
        height,
        stride: width * 4,
        format: PixelFormat::Bgra8,
        data,
        captured_at: Instant::now(),
    })
}

#[allow(unsafe_code)]
mod ffi {
    use super::*;
    use std::ffi::{CStr, c_char, c_int, c_void};

    unsafe extern "C" {
        fn concord_wayland_run(
            user: *mut c_void,
            stopped: extern "C" fn(*mut c_void) -> c_int,
            ready: extern "C" fn(*mut c_void),
            frame: extern "C" fn(*mut c_void, *const u8, usize, u32, u32, i32, u32) -> c_int,
            error: *mut c_char,
            error_len: usize,
        );
    }

    struct Worker {
        state: Arc<Mutex<CaptureState>>,
        running: Arc<AtomicBool>,
        ready: Option<mpsc::SyncSender<Result<(), String>>>,
    }

    extern "C" fn stopped(user: *mut c_void) -> c_int {
        // SAFETY: run owns Worker until the synchronous C call returns. All
        // callbacks execute on that same thread, with this exact pointer.
        let worker = unsafe { &*user.cast::<Worker>() };
        i32::from(!worker.running.load(Ordering::SeqCst))
    }

    extern "C" fn ready(user: *mut c_void) {
        // SAFETY: same lifetime and thread guarantee as stopped.
        let worker = unsafe { &mut *user.cast::<Worker>() };
        if let Some(ready) = worker.ready.take()
            && ready.send(Ok(())).is_err()
        {
            worker.running.store(false, Ordering::SeqCst);
        }
    }

    extern "C" fn frame(
        user: *mut c_void,
        data: *const u8,
        length: usize,
        width: u32,
        height: u32,
        stride: i32,
        format: u32,
    ) -> c_int {
        if data.is_null() || length > 512 * 1024 * 1024 {
            return -1;
        }
        // SAFETY: the C bridge bounds length/offset against spa_data.maxsize;
        // PW_STREAM_FLAG_MAP_BUFFERS maps the memory. The buffer stays dequeued
        // until this callback returns. copy_frame owns its pixels thereafter.
        let bytes = unsafe { std::slice::from_raw_parts(data, length) };
        // SAFETY: Worker is live and callbacks never overlap on another thread.
        let worker = unsafe { &mut *user.cast::<Worker>() };
        match (
            copy_frame(bytes, width, height, stride, format),
            worker.state.lock(),
        ) {
            (Ok(frame), Ok(mut state)) => {
                state.latest = Some(frame);
                0
            }
            _ => -1,
        }
    }

    pub(super) fn run(
        state: Arc<Mutex<CaptureState>>,
        running: Arc<AtomicBool>,
        ready_tx: mpsc::SyncSender<Result<(), String>>,
    ) {
        let mut worker = Worker {
            state,
            running,
            ready: Some(ready_tx),
        };
        let mut error = [0 as c_char; 1024];
        // SAFETY: pointers refer to stack-owned storage for this synchronous
        // call; the bridge retains no pointers and destroys all native objects
        // before returning. Callbacks never panic or enter the async runtime.
        unsafe {
            concord_wayland_run(
                (&mut worker as *mut Worker).cast(),
                stopped,
                ready,
                frame,
                error.as_mut_ptr(),
                error.len(),
            );
        }
        // SAFETY: error is zero-initialized and the bridge uses g_strlcpy with
        // its explicit capacity, which always leaves a terminating NUL.
        let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
        let message = if message.is_empty() {
            "compartilhamento de tela encerrado".to_owned()
        } else {
            message.into_owned()
        };
        if let Some(ready) = worker.ready.take() {
            let _ = ready.send(Err(message.clone()));
        }
        if let Ok(mut state) = worker.state.lock() {
            state.latest = None;
            state.error = Some(message);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_capture_returns_without_contacting_the_desktop() {
        let started = Instant::now();
        assert!(WaylandCapturer::new(Arc::new(AtomicBool::new(false))).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    #[ignore = "requires an interactive Wayland session and portal source selection"]
    fn portal_captures_real_frames_and_closes() {
        let running = Arc::new(AtomicBool::new(true));
        let mut capturer = WaylandCapturer::new(running.clone()).expect("portal capture");
        for _ in 0..30 {
            let frame = capturer.next_frame().expect("PipeWire frame");
            frame.validate().expect("valid BGRA");
            assert!(
                frame
                    .data
                    .iter()
                    .skip(3)
                    .step_by(4)
                    .all(|alpha| *alpha == 255)
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        capturer.stop();
        assert!(!running.load(Ordering::SeqCst));
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while capturer.state.lock().expect("state").error.is_none() {
            assert!(Instant::now() < deadline, "portal session did not close");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn strips_row_padding_and_makes_bgrx_opaque() {
        let pixels = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0];
        let frame = copy_frame(&pixels, 1, 2, 8, 1).expect("padded BGRx");
        assert_eq!(frame.data, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(frame.stride, 4);
        assert!(frame.validate().is_ok());
    }

    #[test]
    fn converts_rgba_and_rgbx_to_bgra() {
        for format in [2, 3] {
            let frame = copy_frame(&[30, 20, 10, 0], 1, 1, 4, format).expect("RGB");
            assert_eq!(frame.data, [10, 20, 30, 255]);
        }
    }

    #[test]
    fn rejects_invalid_mapped_buffers_before_copying() {
        for (width, height, stride, format) in [
            (0, 1, 4, 0),
            (1, 0, 4, 0),
            (8193, 1, 32772, 0),
            (1, 1, -4, 0),
            (2, 1, 4, 0),
            (1, 2, 4, 0),
            (1, 1, 4, 4),
        ] {
            assert!(copy_frame(&[0; 4], width, height, stride, format).is_err());
        }
    }

    #[test]
    fn cached_static_frame_is_reused_but_revocation_is_an_error() {
        let state = Arc::new(Mutex::new(CaptureState {
            latest: Some(copy_frame(&[1, 2, 3, 0], 1, 1, 4, 0).expect("frame")),
            error: None,
        }));
        let mut capturer = WaylandCapturer {
            source: CaptureSource::Monitor {
                id: 0,
                name: "test".to_owned(),
            },
            state: state.clone(),
            running: Arc::new(AtomicBool::new(true)),
        };
        assert_eq!(
            capturer.next_frame().expect("first").data,
            capturer.next_frame().expect("static").data
        );
        state.lock().expect("state").error = Some("revoked".to_owned());
        assert!(capturer.next_frame().is_err());
    }
}
