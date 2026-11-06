//! Native Win32 video windows. Each owns its OS thread and message loop;
//! pixels are held in a bounded latest-frame slot and painted using GDI.

#![allow(unsafe_code)] // Native windows, COM initialization and GDI boundary.

#[cfg(test)]
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::time::Duration;

use concord_media::{ScreenCapturer, WindowsCapturer};
#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use tauri::{AppHandle, Runtime};
#[cfg(test)]
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
#[cfg(test)]
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_BRUSH, BeginPaint, COLORONCOLOR, DIB_RGB_COLORS,
    EndPaint, FillRect, GetStockObject, HBRUSH, InvalidateRect, PAINTSTRUCT, SRCCOPY,
    SetStretchBltMode, StretchDIBits,
};
#[cfg(test)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, IDC_ARROW, LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SW_SHOW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_PAINT, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
#[cfg(test)]
use windows::core::{PCWSTR, w};

#[cfg(test)]
use super::PreviewFrame;

pub(super) struct CaptureApartment(std::marker::PhantomData<std::rc::Rc<()>>);

impl CaptureApartment {
    pub(super) fn new() -> Result<Self, String> {
        // SAFETY: this is a dedicated, freshly spawned capture thread.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|error| format!("não foi possível iniciar o WinRT: {error}"))?;
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for CaptureApartment {
    fn drop(&mut self) {
        // SAFETY: paired on the same thread with successful RoInitialize.
        unsafe {
            RoUninitialize();
        }
    }
}

pub(super) fn prepare_capture(running: Arc<AtomicBool>) -> Result<Box<dyn ScreenCapturer>, String> {
    Ok(Box::new(WindowsCapturer::new(running).map_err(
        |error| format!("não foi possível compartilhar o monitor principal: {error}"),
    )?))
}

#[cfg(test)]
const REDRAW: u32 = WM_APP + 1;
#[cfg(test)]
const CLASS_NAME: PCWSTR = w!("ConcordNativeScreenViewer");

#[cfg(test)]
#[derive(Default)]
struct ViewerState {
    frame: Mutex<Option<PreviewFrame>>,
    hwnd: AtomicUsize,
    redraw_pending: AtomicBool,
    close_requested: AtomicBool,
    closed: AtomicBool,
    #[cfg(test)]
    painted: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
thread_local! {
    // One viewer per thread; no raw Rust pointer is attached to an HWND.
    static VIEWER: RefCell<Option<Arc<ViewerState>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub struct Viewer {
    state: Arc<ViewerState>,
}

#[cfg(test)]
impl Viewer {
    pub fn open<R: Runtime>(
        _app: &AppHandle<R>,
        title: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("tamanho de janela inválido".to_owned());
        }
        let state = Arc::new(ViewerState::default());
        let window_state = state.clone();
        let title = format!("Concord — {title}");
        let (ready, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("concord-video-viewer".to_owned())
            .spawn(move || {
                VIEWER.with(|slot| *slot.borrow_mut() = Some(window_state.clone()));
                // SAFETY: all window and GDI operations happen on this thread.
                let created = unsafe { create_window(&title, width, height, &window_state) };
                match created {
                    Ok(hwnd) => {
                        if ready.send(Ok(())).is_err()
                            || window_state.close_requested.load(Ordering::SeqCst)
                        {
                            let _ = unsafe { DestroyWindow(hwnd) };
                        } else {
                            let mut message = MSG::default();
                            while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
                                unsafe {
                                    let _ = TranslateMessage(&message);
                                    DispatchMessageW(&message);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
                window_state.hwnd.store(0, Ordering::SeqCst);
                window_state.closed.store(true, Ordering::SeqCst);
                window_state.frame.lock().take();
                VIEWER.with(|slot| slot.borrow_mut().take());
            })
            .map_err(|error| format!("não foi possível iniciar o visualizador: {error}"))?;
        let viewer = Self { state };
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("a janela nativa não respondeu: {error}"))??;
        Ok(viewer)
    }

    pub fn push_frame(&self, data: Vec<u8>, width: u32, height: u32) {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if !self.is_open()
            || width == 0
            || height == 0
            || width > i32::MAX as u32
            || height > i32::MAX as u32
            || expected != Some(data.len())
        {
            return;
        }
        *self.state.frame.lock() = Some(PreviewFrame {
            data,
            width,
            height,
        });
        if !self.state.redraw_pending.swap(true, Ordering::SeqCst) {
            self.post(REDRAW);
        }
    }

    pub fn is_open(&self) -> bool {
        !self.state.closed.load(Ordering::SeqCst)
            && !self.state.close_requested.load(Ordering::SeqCst)
    }

    fn post(&self, message: u32) {
        let hwnd = self.state.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: only a message is posted; no borrowed data crosses threads.
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), message, WPARAM(0), LPARAM(0)) };
        }
    }

    fn close(&mut self) {
        self.state.close_requested.store(true, Ordering::SeqCst);
        self.post(WM_CLOSE);
    }
}

#[cfg(test)]
impl Drop for Viewer {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
unsafe fn create_window(
    title: &str,
    width: u32,
    height: u32,
    state: &ViewerState,
) -> Result<HWND, String> {
    static CLASS: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let instance = HINSTANCE(
        unsafe { GetModuleHandleW(None) }
            .map_err(|error| error.to_string())?
            .0,
    );
    CLASS
        .get_or_init(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME,
                hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }
                    .map_err(|error| error.to_string())?,
                ..Default::default()
            };
            if unsafe { RegisterClassW(&class) } == 0 {
                Err(windows::core::Error::from_thread().to_string())
            } else {
                Ok(())
            }
        })
        .clone()?;
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let window_width = width.clamp(320, 1280) as i32;
    let window_height =
        (u64::from(height) * window_width as u64 / u64::from(width)).clamp(180, 900) as i32;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS_NAME,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window_width,
            window_height,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|error| format!("não foi possível abrir a janela nativa: {error}"))?;
    state.hwnd.store(hwnd.0 as usize, Ordering::SeqCst);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    Ok(hwnd)
}

#[cfg(test)]
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        REDRAW | WM_SIZE => {
            VIEWER.with(|slot| {
                if let Some(state) = slot.borrow().as_ref() {
                    state.redraw_pending.store(false, Ordering::SeqCst);
                }
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                paint(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            VIEWER.with(|slot| {
                if let Some(state) = slot.borrow().as_ref() {
                    state.hwnd.store(0, Ordering::SeqCst);
                    state.closed.store(true, Ordering::SeqCst);
                }
            });
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
unsafe fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut rect = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut rect) };
    unsafe {
        FillRect(dc, &rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
    }
    VIEWER.with(|slot| {
        let slot = slot.borrow();
        let Some(state) = slot.as_ref() else {
            return;
        };
        let frame = state.frame.lock();
        let Some(frame) = frame.as_ref() else {
            return;
        };
        let available_width = rect.right - rect.left;
        let available_height = rect.bottom - rect.top;
        if available_width <= 0 || available_height <= 0 {
            return;
        }
        let scale = (f64::from(available_width) / f64::from(frame.width))
            .min(f64::from(available_height) / f64::from(frame.height));
        let width = (f64::from(frame.width) * scale).round() as i32;
        let height = (f64::from(frame.height) * scale).round() as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: frame.width as i32,
                biHeight: -(frame.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let _painted = unsafe {
            SetStretchBltMode(dc, COLORONCOLOR);
            StretchDIBits(
                dc,
                (available_width - width) / 2,
                (available_height - height) / 2,
                width,
                height,
                0,
                0,
                frame.width as i32,
                frame.height as i32,
                Some(frame.data.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        #[cfg(test)]
        if _painted > 0 {
            state.painted.fetch_add(1, Ordering::SeqCst);
        }
    });
    unsafe {
        let _ = EndPaint(hwnd, &paint);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn native_viewer_opens_draws_and_closes() {
        let app = tauri::test::mock_app();
        let mut viewer = Viewer::open(app.handle(), "teste Windows", 640, 360).expect("viewer");
        viewer.push_frame([20, 200, 40, 255].repeat(640 * 360), 640, 360);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while viewer.state.painted.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "no native paint");
            std::thread::sleep(Duration::from_millis(10));
        }
        viewer.close();
        while !viewer.state.closed.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "window did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!viewer.is_open());
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop with WGC"]
    fn wgc_capture_returns_valid_pixels_and_stops() {
        let _apartment = CaptureApartment::new().expect("WinRT");
        let running = Arc::new(AtomicBool::new(true));
        let mut capture = WindowsCapturer::new(running.clone()).expect("WGC");
        capture
            .next_frame()
            .expect("first frame")
            .validate()
            .expect("BGRA");
        capture
            .next_frame()
            .expect("second frame")
            .validate()
            .expect("BGRA");
        capture.stop();
        assert!(!running.load(Ordering::SeqCst));
        assert!(capture.next_frame().is_err());
    }
}
