//! Native screen-sharing pipeline and viewers.
//!
//! Linux/X11 uses MIT-SHM; Wayland uses the ScreenCast portal and PipeWire.
//! Windows uses Windows Graphics Capture.
//! Capture runs on OS threads. Encoded VP8 frames travel through
//! Tokio channels; the local preview
//! lives in a latest-only slot so a slow UI cannot queue full BGRA frames.
//! The call actor sends small, throttled PNG previews through Tauri events to
//! the main WebView. Full-resolution capture and VP8 media stay in Rust.
//!
//! Other platforms return a precise unsupported error until their capture
//! backend lands.

use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
pub struct ScreenFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

#[derive(Debug)]
pub struct PreviewFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub enum ScreenEvent {
    Frame { instance: u64, frame: ScreenFrame },
    Error { instance: u64, message: String },
}

#[derive(Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub type FrameSender = UnboundedSender<ScreenEvent>;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use native::{RemoteDecoder, ShareSession, start_share};

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub use unsupported::{RemoteDecoder, ShareSession, Viewer, start_share};
