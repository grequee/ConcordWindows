//! Native media contracts and capture backends.
//!
//! The WebView is intentionally absent from this crate. It handles only
//! codecs, capture, and bounded preview-frame data; the app decides how to
//! display previews.
//!
//! Implemented in milestone 2:
//! - Linux X11/Xwayland screen capture through MIT-SHM (`x11rb`), behind the
//!   `x11` feature.
//! - Linux Wayland capture through the ScreenCast portal and PipeWire, behind
//!   the `wayland` feature. Interactive desktop validation is still pending.
//! - CPAL device enumeration, microphone capture and local playback loopback
//!   behind the `audio` feature (enabled by default).
//!
//! - Windows: Windows Graphics Capture / Direct3D11, behind `windows`.
//!
//! Codecs:
//! - Codecs: capture PCM -> Opus and screen frames -> VP8; decoded media is
//!   played/rendered natively with a small jitter buffer.

use std::time::Instant;

use thiserror::Error;

#[cfg(feature = "audio")]
pub mod audio;

#[cfg(feature = "audio")]
pub mod codec;

#[cfg(feature = "audio")]
pub mod jitter;

#[cfg(feature = "audio")]
pub mod voice;

#[cfg(feature = "audio")]
pub mod microphone;

#[cfg(feature = "audio")]
pub mod activity;

#[cfg(feature = "audio")]
pub use microphone::{
    MicrophoneProcessor, MicrophoneSettings, MicrophoneStats, NOISE_SUPPRESSION_AVAILABLE,
};

#[cfg(feature = "video")]
pub mod video;

#[cfg(feature = "audio")]
pub use audio::{AudioDeviceInfo, LoopbackConfig, LoopbackStats, list_audio_devices, run_loopback};

#[cfg(feature = "audio")]
pub use codec::{DEFAULT_OPUS_BITRATE_BPS, MAX_OPUS_PACKET_BYTES, VoiceDecoder, VoiceEncoder};

#[cfg(feature = "audio")]
pub use jitter::{JitterBuffer, JitterStats};

#[cfg(feature = "audio")]
pub use voice::{PreparedAudioChange, VoiceAudio, VoiceAudioConfig, VoiceAudioStats};

#[cfg(feature = "video")]
pub use video::{
    EncodedVideoFrame, I420Frame, PreparedScreenFrame, Vp8Decoder, Vp8Encoder, bgra_to_i420,
    prepare_screen_frame, prepare_screen_frame_with_preview, scale_bgra,
};

#[cfg(all(feature = "x11", target_os = "linux"))]
pub mod x11;

#[cfg(all(feature = "x11", target_os = "linux"))]
pub use x11::{X11Capturer, list_x11_sources};

#[cfg(all(feature = "wayland", target_os = "linux"))]
pub mod wayland;

#[cfg(all(feature = "wayland", target_os = "linux"))]
pub use wayland::WaylandCapturer;

#[cfg(all(feature = "windows", target_os = "windows"))]
pub mod windows;

#[cfg(all(feature = "windows", target_os = "windows"))]
pub use windows::WindowsCapturer;

pub const SCREEN_WIDTH_720P: u32 = 1280;
pub const SCREEN_HEIGHT_720P: u32 = 720;
pub const SCREEN_WIDTH_1080P: u32 = 1920;
pub const SCREEN_HEIGHT_1080P: u32 = 1080;

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
pub const OPUS_FRAME_MS: u32 = 20;
pub const OPUS_FRAME_SAMPLES: usize =
    (OPUS_SAMPLE_RATE as usize / 1_000) * (OPUS_FRAME_MS as usize);

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("capture backend is not available on this platform")]
    UnsupportedPlatform,
    #[error("permission to capture the screen was denied")]
    PermissionDenied,
    #[error("capture source no longer exists")]
    SourceLost,
    #[error("no capture source matched the request")]
    SourceNotFound,
    #[error("invalid media settings: {0}")]
    InvalidSettings(&'static str),
    #[error("codec error: {0}")]
    Codec(String),
    #[error("audio device error: {0}")]
    Audio(String),
    #[error("capture error: {0}")]
    Capture(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub max_bitrate_kbps: u32,
}

pub const PROFILE_720P30: VideoProfile = VideoProfile {
    width: SCREEN_WIDTH_720P,
    height: SCREEN_HEIGHT_720P,
    fps: 30,
    max_bitrate_kbps: 2_500,
};

/// More temporal detail at the safe 720p resolution. The extra bitrate keeps
/// the per-frame budget from collapsing at 60 fps.
pub const PROFILE_720P60: VideoProfile = VideoProfile {
    width: SCREEN_WIDTH_720P,
    height: SCREEN_HEIGHT_720P,
    fps: 60,
    max_bitrate_kbps: 3_500,
};

/// More spatial detail for text-heavy screens while staying within the
/// software-VP8 budget of the reference machine.
pub const PROFILE_1080P30: VideoProfile = VideoProfile {
    width: SCREEN_WIDTH_1080P,
    height: SCREEN_HEIGHT_1080P,
    fps: 30,
    max_bitrate_kbps: 4_500,
};

/// Experimental. Software VP8 cannot sustain 60 fps at this resolution on the
/// reference i7-1355U; the quality selector labels this profile experimental.
pub const PROFILE_1080P60: VideoProfile = VideoProfile {
    width: SCREEN_WIDTH_1080P,
    height: SCREEN_HEIGHT_1080P,
    fps: 60,
    max_bitrate_kbps: 6_000,
};

impl VideoProfile {
    pub fn validate(self) -> Result<Self, MediaError> {
        if self.width == 0 || self.height == 0 {
            return Err(MediaError::InvalidSettings("video dimensions are zero"));
        }
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
            return Err(MediaError::InvalidSettings(
                "4:2:0 video needs even dimensions",
            ));
        }
        if !(1..=60).contains(&self.fps) {
            return Err(MediaError::InvalidSettings("fps must be between 1 and 60"));
        }
        if !(100..=20_000).contains(&self.max_bitrate_kbps) {
            return Err(MediaError::InvalidSettings(
                "bitrate must be between 100 and 20000 kbps",
            ));
        }
        Ok(self)
    }

    /// Pick a conservative profile for the reported link budget. The encoder
    /// can be reconfigured later without tearing down the peer connection.
    ///
    /// Below 2.5 Mbps the resolution drops to 720p; below 1.2 Mbps it drops to
    /// 960×540. Above that, keep the resolution and cap the frame rate at 30
    /// before sacrificing spatial detail.
    pub fn for_budget(self, available_kbps: u32) -> Self {
        if available_kbps >= self.max_bitrate_kbps {
            return self;
        }

        let fps = self.fps.min(30);
        if available_kbps >= PROFILE_720P30.max_bitrate_kbps {
            Self {
                fps,
                max_bitrate_kbps: available_kbps,
                ..self
            }
        } else if available_kbps >= 1_200 {
            Self {
                width: SCREEN_WIDTH_720P,
                height: SCREEN_HEIGHT_720P,
                fps,
                max_bitrate_kbps: available_kbps,
            }
        } else {
            Self {
                width: 960,
                height: 540,
                fps,
                max_bitrate_kbps: available_kbps.clamp(600, 1_200),
            }
        }
    }

    pub fn min_fps(mut self, fps: u32) -> Self {
        self.fps = self.fps.min(fps);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    Monitor {
        id: u32,
        name: String,
    },
    Window {
        id: u64,
        title: String,
        app_name: Option<String>,
    },
}

impl CaptureSource {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Monitor { name, .. } => name,
            Self::Window { title, .. } => title,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
}

#[derive(Debug)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    pub captured_at: Instant,
}

impl CapturedFrame {
    pub fn validate(&self) -> Result<(), MediaError> {
        let min_stride = self.width as usize * 4;
        if self.width == 0 || self.height == 0 || (self.stride as usize) < min_stride {
            return Err(MediaError::InvalidSettings("invalid captured frame"));
        }
        let expected = self.stride as usize * self.height as usize;
        if self.data.len() < expected {
            return Err(MediaError::InvalidSettings(
                "captured frame buffer is smaller than its stride",
            ));
        }
        Ok(())
    }
}

/// Synchronous capture contract. Create, use and drop a capturer on one
/// dedicated OS thread. Windows graphics interfaces are apartment bound;
/// implementations need not be Send and must never run on the async runtime.
pub trait ScreenCapturer {
    fn source(&self) -> &CaptureSource;
    fn next_frame(&mut self) -> Result<CapturedFrame, MediaError>;
    fn stop(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub fn opus_mono() -> Self {
        Self {
            sample_rate: OPUS_SAMPLE_RATE,
            channels: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub format: AudioFormat,
    pub samples: Vec<f32>,
}

pub trait AudioSource: Send {
    fn format(&self) -> AudioFormat;
    fn read(&mut self) -> Result<AudioFrame, MediaError>;
    fn stop(&mut self);
}

pub trait AudioSink: Send {
    fn format(&self) -> AudioFormat;
    fn play(&mut self, frame: &AudioFrame) -> Result<(), MediaError>;
    fn stop(&mut self);
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn profiles_validate() {
        assert_eq!(PROFILE_720P30.validate().ok(), Some(PROFILE_720P30));
        assert_eq!(PROFILE_720P60.validate().ok(), Some(PROFILE_720P60));
        assert_eq!(PROFILE_1080P30.validate().ok(), Some(PROFILE_1080P30));
        assert_eq!(PROFILE_1080P60.validate().ok(), Some(PROFILE_1080P60));
        assert!(
            VideoProfile {
                height: 721,
                ..PROFILE_720P30
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn budget_degrades_resolution_or_fps() {
        assert_eq!(PROFILE_1080P60.for_budget(10_000), PROFILE_1080P60);
        assert_eq!(
            PROFILE_1080P60.for_budget(3_000),
            VideoProfile {
                fps: 30,
                max_bitrate_kbps: 3_000,
                ..PROFILE_1080P60
            }
        );
        assert_eq!(
            PROFILE_720P60.for_budget(3_000),
            VideoProfile {
                fps: 30,
                max_bitrate_kbps: 3_000,
                ..PROFILE_720P60
            }
        );
        assert_eq!(
            PROFILE_1080P30.for_budget(2_000),
            VideoProfile {
                width: SCREEN_WIDTH_720P,
                height: SCREEN_HEIGHT_720P,
                fps: 30,
                max_bitrate_kbps: 2_000,
            }
        );
        let low = PROFILE_1080P60.for_budget(800);
        assert_eq!(
            (low.width, low.height, low.max_bitrate_kbps),
            (960, 540, 800)
        );
    }

    #[test]
    fn captured_frames_are_checked_before_encoding() {
        let frame = CapturedFrame {
            width: 2,
            height: 2,
            stride: 8,
            format: PixelFormat::Bgra8,
            data: vec![0; 16],
            captured_at: Instant::now(),
        };
        assert!(frame.validate().is_ok());

        let short = CapturedFrame {
            data: vec![0; 15],
            ..frame
        };
        assert!(short.validate().is_err());
    }
}
