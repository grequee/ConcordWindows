//! Shared native capture/VP8 pipeline for Linux and Windows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::{DecodedFrame, FrameSender, PreviewFrame, ScreenEvent, ScreenFrame};
use concord_media::{
    CapturedFrame, ScreenCapturer, VideoProfile, Vp8Decoder, Vp8Encoder,
    prepare_screen_frame_with_preview,
};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

pub struct ShareSession {
    instance: u64,
    control: ShareControl,
    source_name: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone)]
struct ShareControl {
    running: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    preview: Arc<Mutex<Option<PreviewFrame>>>,
    fps: Arc<AtomicU32>,
    reconfigure: Arc<Mutex<Option<(VideoProfile, Vp8Encoder)>>>,
}

/// Cancelling preparation (leaving a call, timeout, closed command receiver)
/// also closes a pending portal request and its private PipeWire remote.
struct PreparingCapture(Option<Arc<AtomicBool>>);

impl Drop for PreparingCapture {
    fn drop(&mut self) {
        if let Some(running) = &self.0 {
            running.store(false, Ordering::SeqCst);
        }
    }
}

impl ShareSession {
    pub fn instance(&self) -> u64 {
        self.instance
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    pub fn force_keyframe(&self) {
        self.control.force_keyframe.store(true, Ordering::SeqCst);
    }

    /// Take the newest preview frame, if the capture thread produced one since
    /// the last call. Overwriting older previews keeps memory bounded.
    pub fn take_preview(&self) -> Option<PreviewFrame> {
        self.control.preview.lock().take()
    }

    /// Keep the authorized capture session alive while replacing only VP8.
    /// In particular, changing quality must never ask for portal consent again.
    pub async fn reconfigure(&self, profile: VideoProfile) -> Result<(), String> {
        profile.validate().map_err(|error| error.to_string())?;
        let encoder = tokio::task::spawn_blocking(move || Vp8Encoder::new(profile))
            .await
            .map_err(|error| format!("thread do codec falhou: {error}"))?
            .map_err(|error| format!("codec VP8 indisponível: {error}"))?;
        if !self.control.running.load(Ordering::SeqCst) {
            return Err("a captura de tela foi encerrada".to_owned());
        }
        *self.control.reconfigure.lock() = Some((profile, encoder));
        Ok(())
    }

    pub fn stop(&mut self) {
        self.control.running.store(false, Ordering::SeqCst);
        // Do not join on the async runtime: capture may be finishing one
        // synchronous ShmGetImage call. The thread owns its X11 resources and
        // exits on the next loop check.
        self.thread.take();
    }
}

impl Drop for ShareSession {
    fn drop(&mut self) {
        self.control.running.store(false, Ordering::SeqCst);
    }
}

pub async fn start_share(
    profile: VideoProfile,
    instance: u64,
    events: FrameSender,
) -> Result<ShareSession, String> {
    profile.validate().map_err(|error| error.to_string())?;
    let control = ShareControl {
        running: Arc::new(AtomicBool::new(true)),
        force_keyframe: Arc::new(AtomicBool::new(false)),
        preview: Arc::new(Mutex::new(None)),
        fps: Arc::new(AtomicU32::new(profile.fps)),
        reconfigure: Arc::new(Mutex::new(None)),
    };
    let mut preparing = PreparingCapture(Some(control.running.clone()));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let thread_control = control.clone();
    let thread = std::thread::Builder::new()
        .name("concord-screen-capture".to_owned())
        .spawn(move || {
            // Capture is created, used and destroyed on this same OS thread.
            // Windows COM apartments and the D3D context never migrate.
            #[cfg(target_os = "windows")]
            let _apartment = match super::windows::CaptureApartment::new() {
                Ok(apartment) => apartment,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let prepared = (|| {
                let encoder = Vp8Encoder::new(profile)
                    .map_err(|error| format!("codec VP8 indisponível: {error}"))?;
                let capturer = super::platform::prepare_capture(thread_control.running.clone())?;
                Ok::<_, String>((capturer, encoder))
            })();
            match prepared {
                Ok((capturer, encoder)) => {
                    let source_name = capturer.source().display_name().to_owned();
                    if ready_tx.send(Ok(source_name)).is_err() {
                        return;
                    }
                    capture_loop(capturer, encoder, profile, instance, thread_control, events);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .map_err(|error| format!("não foi possível iniciar a captura: {error}"))?;
    let source_name = ready_rx
        .await
        .map_err(|error| format!("thread de captura falhou: {error}"))??;

    preparing.0 = None;

    Ok(ShareSession {
        instance,
        control,
        source_name,
        thread: Some(thread),
    })
}

fn capture_loop(
    mut capturer: Box<dyn ScreenCapturer>,
    encoder: Vp8Encoder,
    profile: VideoProfile,
    instance: u64,
    control: ShareControl,
    events: UnboundedSender<ScreenEvent>,
) {
    // Capture and encode run on separate threads. A two-frame queue absorbs
    // encoder jitter; capture drops new frames when the queue is full.
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<CapturedFrame>(2);
    std::thread::scope(|scope| {
        let encoder_control = control.clone();
        let encoder_events = events.clone();
        scope.spawn(move || {
            encode_loop(
                encoder,
                profile,
                instance,
                encoder_control,
                encoder_events,
                frame_rx,
            );
        });

        capture_frames(capturer.as_mut(), instance, &control, &events, &frame_tx);
        drop(frame_tx);
    });
    capturer.stop();
    debug!(%instance, "screen capture thread stopped");
}

fn capture_frames(
    capturer: &mut dyn ScreenCapturer,
    instance: u64,
    control: &ShareControl,
    events: &UnboundedSender<ScreenEvent>,
    frames: &std::sync::mpsc::SyncSender<CapturedFrame>,
) {
    let loop_started = Instant::now();
    let mut captured_frames = 0u64;
    let mut sent_frames = 0u64;
    let mut dropped = 0u64;
    while control.running.load(Ordering::SeqCst) {
        let started = Instant::now();
        let frame_time =
            Duration::from_secs_f64(1.0 / f64::from(control.fps.load(Ordering::SeqCst)));
        let captured = match capturer.next_frame() {
            Ok(frame) => frame,
            Err(error) => {
                let _ = events.send(ScreenEvent::Error {
                    instance,
                    message: format!("captura de tela interrompida: {error}"),
                });
                control.running.store(false, Ordering::SeqCst);
                break;
            }
        };
        captured_frames = captured_frames.saturating_add(1);
        match frames.try_send(captured) {
            Ok(()) => sent_frames = sent_frames.saturating_add(1),
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                dropped = dropped.saturating_add(1);
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
        }

        if captured_frames.is_multiple_of(60) {
            debug!(
                %instance,
                captured = captured_frames,
                sent = sent_frames,
                dropped,
                elapsed = ?loop_started.elapsed(),
                "screen capture cadence"
            );
        }
        if let Some(remaining) = frame_time.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
    if dropped > 0 {
        debug!(%instance, dropped, "screen capture dropped stale frames");
    }
}

fn encode_loop(
    mut encoder: Vp8Encoder,
    mut profile: VideoProfile,
    instance: u64,
    control: ShareControl,
    events: UnboundedSender<ScreenEvent>,
    frames: std::sync::mpsc::Receiver<CapturedFrame>,
) {
    let loop_started = Instant::now();
    let mut encoded_frames = 0u64;
    let mut prepare_time = Duration::ZERO;
    let mut encode_time = Duration::ZERO;
    while control.running.load(Ordering::SeqCst) {
        let captured = match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => frame,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // Apply after waiting: a reconfiguration may arrive while recv blocks.
        if let Some((next_profile, next_encoder)) = control.reconfigure.lock().take() {
            encoder = next_encoder;
            profile = next_profile;
            control.fps.store(profile.fps, Ordering::SeqCst);
            control.force_keyframe.store(true, Ordering::SeqCst);
            prepare_time = Duration::ZERO;
            encode_time = Duration::ZERO;
            encoded_frames = 0;
        }
        let prepare_started = Instant::now();
        let prepared =
            match prepare_screen_frame_with_preview(&captured, profile.width, profile.height) {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = events.send(ScreenEvent::Error {
                        instance,
                        message: format!("preparação do quadro falhou: {error}"),
                    });
                    control.running.store(false, Ordering::SeqCst);
                    break;
                }
            };
        prepare_time += prepare_started.elapsed();
        *control.preview.lock() = Some(PreviewFrame {
            data: prepared.bgra,
            width: profile.width,
            height: profile.height,
        });
        let force = control.force_keyframe.swap(false, Ordering::SeqCst);
        let encode_started = Instant::now();
        let encoded = match encoder.encode(&prepared.i420, force) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(error) => {
                let _ = events.send(ScreenEvent::Error {
                    instance,
                    message: format!("codificação VP8 falhou: {error}"),
                });
                control.running.store(false, Ordering::SeqCst);
                break;
            }
        };
        encode_time += encode_started.elapsed();
        encoded_frames = encoded_frames.saturating_add(1);
        if encoded_frames.is_multiple_of(60) {
            let count = encoded_frames as f64;
            debug!(
                %instance,
                encoded = encoded_frames,
                prepare_ms = prepare_time.as_secs_f64() * 1000.0 / count,
                encode_ms = encode_time.as_secs_f64() * 1000.0 / count,
                elapsed = ?loop_started.elapsed(),
                "screen encode cadence"
            );
        }
        if events
            .send(ScreenEvent::Frame {
                instance,
                frame: ScreenFrame {
                    data: encoded.data,
                    keyframe: encoded.keyframe,
                },
            })
            .is_err()
        {
            control.running.store(false, Ordering::SeqCst);
            break;
        }
    }
}

/// Decodes remote VP8 into BGRA for the embedded screen preview.
pub struct RemoteDecoder {
    decoder: Option<Vp8Decoder>,
}

impl RemoteDecoder {
    pub fn new() -> Self {
        match Vp8Decoder::new() {
            Ok(decoder) => Self {
                decoder: Some(decoder),
            },
            Err(error) => {
                warn!(%error, "could not initialize the remote VP8 decoder");
                Self { decoder: None }
            }
        }
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, String> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| "decodificador VP8 indisponível".to_owned())?;
        let frame = decoder
            .decode(data)
            .map_err(|error| format!("decodificação VP8 falhou: {error}"))?;
        let Some(frame) = frame else {
            return Ok(None);
        };
        let width = frame.width;
        let height = frame.height;
        let bgra = frame
            .to_bgra()
            .map_err(|error| format!("conversão para BGRA falhou: {error}"))?;
        Ok(Some(DecodedFrame {
            width,
            height,
            bgra,
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn aborting_preparation_cancels_the_native_worker() {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _preparing = PreparingCapture(Some(worker_running));
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.expect("preparation started");
        task.abort();
        assert!(task.await.expect_err("cancelled task").is_cancelled());
        assert!(!running.load(Ordering::SeqCst));
    }

    #[tokio::test]
    #[allow(clippy::panic)]
    async fn quality_switch_reuses_capture_and_emits_decodable_keyframes() {
        let initial = VideoProfile {
            width: 32,
            height: 32,
            fps: 30,
            max_bitrate_kbps: 2500,
        };
        let control = ShareControl {
            running: Arc::new(AtomicBool::new(true)),
            force_keyframe: Arc::new(AtomicBool::new(false)),
            preview: Arc::new(Mutex::new(None)),
            fps: Arc::new(AtomicU32::new(initial.fps)),
            reconfigure: Arc::new(Mutex::new(None)),
        };
        let (frames, receiver) = std::sync::mpsc::sync_channel(2);
        let (events, mut screen_events) = tokio::sync::mpsc::unbounded_channel();
        let worker_control = control.clone();
        let encoder = Vp8Encoder::new(initial).expect("VP8");
        let thread = std::thread::spawn(move || {
            encode_loop(encoder, initial, 42, worker_control, events, receiver);
        });
        let mut session = ShareSession {
            instance: 42,
            control,
            source_name: "same authorized source".to_owned(),
            thread: Some(thread),
        };
        let mut decoder = Vp8Decoder::new().expect("decoder");
        for profile in [
            initial,
            VideoProfile {
                width: 64,
                height: 48,
                fps: 60,
                ..initial
            },
            initial,
        ] {
            session.reconfigure(profile).await.expect("new quality");
            frames
                .send(CapturedFrame {
                    width: 2,
                    height: 2,
                    stride: 8,
                    format: concord_media::PixelFormat::Bgra8,
                    data: [20, 200, 40, 255].repeat(4),
                    captured_at: Instant::now(),
                })
                .expect("capture source still connected");
            let event = tokio::time::timeout(Duration::from_secs(3), screen_events.recv())
                .await
                .expect("frame timeout")
                .expect("frame event");
            let ScreenEvent::Frame { instance, frame } = event else {
                panic!("encoder returned an error");
            };
            assert_eq!(instance, 42);
            assert!(frame.keyframe);
            let decoded = decoder
                .decode(&frame.data)
                .expect("decode")
                .expect("pixels");
            assert_eq!(
                (decoded.width, decoded.height),
                (profile.width, profile.height)
            );
            assert_eq!(session.control.fps.load(Ordering::SeqCst), profile.fps);
            let preview = session.take_preview().expect("preview");
            assert_eq!(
                (preview.width, preview.height),
                (profile.width, profile.height)
            );
        }
        session.control.running.store(false, Ordering::SeqCst);
        drop(frames);
        session
            .thread
            .take()
            .expect("worker")
            .join()
            .expect("clean shutdown");
        session.stop();
    }
}
