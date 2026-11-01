//! Native Linux capture selection and GTK3 viewers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::time::Duration;

use concord_media::{
    CaptureSource, ScreenCapturer, WaylandCapturer, X11Capturer, list_x11_sources,
};
use gtk::prelude::*;
use tauri::{AppHandle, Runtime};
use tracing::warn;

fn use_wayland(session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    match session_type {
        Some("wayland") => true,
        Some("x11") => false,
        _ => wayland_display.is_some_and(|display| !display.is_empty()),
    }
}

pub(super) fn prepare_capture(running: Arc<AtomicBool>) -> Result<Box<dyn ScreenCapturer>, String> {
    let wayland = use_wayland(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    );
    if wayland {
        Ok(Box::new(WaylandCapturer::new(running).map_err(
            |error| format!("não foi possível capturar pelo portal Wayland: {error}"),
        )?))
    } else {
        let source = list_x11_sources()
            .map_err(|error| format!("não foi possível listar as telas X11: {error}"))?
            .into_iter()
            .find(|source| matches!(source, CaptureSource::Monitor { .. }))
            .ok_or_else(|| "nenhum monitor X11 disponível".to_owned())?;
        Ok(Box::new(
            X11Capturer::new_full_screen(source, None)
                .map_err(|error| format!("não foi possível capturar a tela X11: {error}"))?,
        ))
    }
}

struct ViewerFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

struct ViewerState {
    latest: parking_lot::Mutex<Option<ViewerFrame>>,
    redraw_scheduled: AtomicBool,
    close_requested: AtomicBool,
    closed: AtomicBool,
}

struct GtkViewer {
    window: gtk::Window,
    area: gtk::DrawingArea,
    surface: std::rc::Rc<RefCell<Option<gtk::cairo::ImageSurface>>>,
}

thread_local! {
    static VIEWERS: RefCell<HashMap<u64, GtkViewer>> = RefCell::new(HashMap::new());
}

static NEXT_VIEWER_ID: AtomicU64 = AtomicU64::new(1);

/// A native GTK viewer. The handle is Send; all GTK objects stay on the Tauri
/// main thread and are addressed by id through an idle callback.
pub struct NativeViewer {
    id: u64,
    state: Arc<ViewerState>,
}

impl NativeViewer {
    pub fn open<R: Runtime>(
        app: &AppHandle<R>,
        title: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("tamanho de janela inválido".to_owned());
        }
        if !gtk::is_initialized() {
            return Err("a interface nativa GTK não está disponível".to_owned());
        }
        let id = NEXT_VIEWER_ID.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(ViewerState {
            latest: parking_lot::Mutex::new(None),
            redraw_scheduled: AtomicBool::new(false),
            close_requested: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        let title = format!("Concord — {title}");
        let window_state = state.clone();
        app.run_on_main_thread(move || {
            create_window(id, &title, width, height, window_state);
        })
        .map_err(|error| format!("não foi possível abrir a janela nativa: {error}"))?;
        Ok(Self { id, state })
    }

    pub fn push_frame(&self, data: Vec<u8>, width: u32, height: u32) {
        if self.state.closed.load(Ordering::SeqCst)
            || self.state.close_requested.load(Ordering::SeqCst)
            || width == 0
            || height == 0
        {
            return;
        }
        *self.state.latest.lock() = Some(ViewerFrame {
            data,
            width,
            height,
        });
        if !self.state.redraw_scheduled.swap(true, Ordering::SeqCst) {
            schedule_redraw(self.id, self.state.clone());
        }
    }

    pub fn is_open(&self) -> bool {
        !self.state.closed.load(Ordering::SeqCst)
            && !self.state.close_requested.load(Ordering::SeqCst)
    }

    pub fn close(&mut self) {
        if self.state.close_requested.swap(true, Ordering::SeqCst) {
            return;
        }
        self.state.latest.lock().take();
        schedule_redraw(self.id, self.state.clone());
    }
}

impl Drop for NativeViewer {
    fn drop(&mut self) {
        self.close();
    }
}

fn create_window(id: u64, title: &str, width: u32, height: u32, state: Arc<ViewerState>) {
    if state.close_requested.load(Ordering::SeqCst) {
        state.closed.store(true, Ordering::SeqCst);
        return;
    }

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title(title);
    let window_width = width.clamp(320, 1280) as i32;
    let window_height =
        ((height as f64 * f64::from(window_width) / f64::from(width)).round() as i32).max(180);
    window.set_default_size(window_width, window_height);
    window.set_position(gtk::WindowPosition::Center);

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    window.add(&area);

    let surface: std::rc::Rc<RefCell<Option<gtk::cairo::ImageSurface>>> =
        std::rc::Rc::new(RefCell::new(None));
    let draw_surface = surface.clone();
    area.connect_draw(move |widget, context| {
        let draw_width = widget.allocated_width();
        let draw_height = widget.allocated_height();
        context.set_source_rgb(0.035, 0.04, 0.035);
        let _ = context.paint();
        let surface = draw_surface.borrow();
        let Some(surface) = surface.as_ref() else {
            return gtk::glib::Propagation::Stop;
        };
        let frame_width = f64::from(surface.width());
        let frame_height = f64::from(surface.height());
        if frame_width <= 0.0 || frame_height <= 0.0 || draw_width <= 0 || draw_height <= 0 {
            return gtk::glib::Propagation::Stop;
        }
        let scale = (f64::from(draw_width) / frame_width)
            .min(f64::from(draw_height) / frame_height)
            .max(0.0);
        let target_width = frame_width * scale;
        let target_height = frame_height * scale;
        let translate_x = (f64::from(draw_width) - target_width) / 2.0;
        let translate_y = (f64::from(draw_height) - target_height) / 2.0;

        context.save().ok();
        context.translate(translate_x, translate_y);
        context.scale(scale, scale);
        if context.set_source_surface(surface, 0.0, 0.0).is_ok() {
            context.source().set_filter(gtk::cairo::Filter::Bilinear);
            let _ = context.paint();
        }
        context.restore().ok();
        gtk::glib::Propagation::Stop
    });

    let delete_state = state.clone();
    window.connect_delete_event(move |_, _| {
        delete_state.closed.store(true, Ordering::SeqCst);
        VIEWERS.with(|viewers| {
            viewers.borrow_mut().remove(&id);
        });
        gtk::glib::Propagation::Proceed
    });

    window.show_all();
    VIEWERS.with(|viewers| {
        viewers.borrow_mut().insert(
            id,
            GtkViewer {
                window,
                area,
                surface,
            },
        );
    });

    // The first frame may have arrived while the main thread was creating the
    // window. Schedule a draw for it instead of waiting for the next frame.
    if state.latest.lock().is_some() {
        schedule_redraw(id, state);
    }
}

fn schedule_redraw(id: u64, state: Arc<ViewerState>) {
    gtk::glib::idle_add_once(move || {
        state.redraw_scheduled.store(false, Ordering::SeqCst);
        if state.close_requested.load(Ordering::SeqCst) {
            close_window(id, &state);
            return;
        }
        let Some(frame) = state.latest.lock().take() else {
            return;
        };
        let stride = match i32::try_from(frame.width)
            .ok()
            .and_then(|width| width.checked_mul(4).filter(|stride| *stride > 0))
        {
            Some(stride) => stride,
            None => return,
        };
        let surface = match gtk::cairo::ImageSurface::create_for_data(
            frame.data,
            gtk::cairo::Format::Rgb24,
            i32::try_from(frame.width).unwrap_or(0),
            i32::try_from(frame.height).unwrap_or(0),
            stride,
        ) {
            Ok(surface) => surface,
            Err(error) => {
                warn!(%error, "could not create the screen preview surface");
                return;
            }
        };
        VIEWERS.with(|viewers| {
            let mut viewers = viewers.borrow_mut();
            let Some(viewer) = viewers.get_mut(&id) else {
                return;
            };
            *viewer.surface.borrow_mut() = Some(surface);
            viewer.area.queue_draw();
        });
    });
}

fn close_window(id: u64, state: &ViewerState) {
    let viewer = VIEWERS.with(|viewers| viewers.borrow_mut().remove(&id));
    state.closed.store(true, Ordering::SeqCst);
    if let Some(viewer) = viewer {
        viewer.window.close();
    }
}

pub struct Viewer {
    inner: Option<NativeViewer>,
}

impl Viewer {
    pub fn open<R: Runtime>(
        app: &AppHandle<R>,
        title: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: Some(NativeViewer::open(app, title, width, height)?),
        })
    }

    pub fn push_frame(&self, data: Vec<u8>, width: u32, height: u32) {
        if let Some(viewer) = &self.inner {
            viewer.push_frame(data, width, height);
        }
    }

    pub fn is_open(&self) -> bool {
        self.inner.as_ref().is_some_and(NativeViewer::is_open)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn chooses_portal_even_when_xwayland_display_is_present() {
        assert!(use_wayland(Some("wayland"), Some("wayland-1")));
        assert!(use_wayland(Some("wayland"), None));
        assert!(use_wayland(None, Some("wayland-1")));
        assert!(!use_wayland(Some("x11"), Some("wayland-1")));
        assert!(!use_wayland(None, None));
        assert!(!use_wayland(None, Some("")));
    }

    fn pump_gtk() {
        for _ in 0..50 {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires an X11/Wayland display"]
    fn native_viewer_opens_draws_and_closes() {
        gtk::init().expect("initialize GTK");
        let app = tauri::test::mock_app();
        let mut viewer = NativeViewer::open(app.handle(), "teste", 4, 4).expect("open viewer");
        assert!(viewer.is_open());

        let mut bgra = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            bgra.extend_from_slice(&[20, 200, 40, 255]);
        }
        viewer.push_frame(bgra, 4, 4);
        pump_gtk();

        let has_surface = VIEWERS.with(|viewers| {
            viewers
                .borrow()
                .get(&viewer.id)
                .is_some_and(|gtk_viewer| gtk_viewer.surface.borrow().is_some())
        });
        assert!(has_surface, "viewer did not receive the first frame");

        viewer.close();
        pump_gtk();
        assert!(!viewer.is_open());
        let still_registered = VIEWERS.with(|viewers| viewers.borrow().contains_key(&viewer.id));
        assert!(!still_registered, "viewer window was not destroyed");
    }
}
