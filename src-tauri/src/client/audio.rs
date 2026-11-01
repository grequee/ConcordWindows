//! Local device choices and a short native record/playback test. No audio
//! samples or recordings leave this process or get saved to disk.
use super::*;
use concord_media::{MicrophoneProcessor, OPUS_FRAME_SAMPLES, VoiceAudio, VoiceAudioConfig};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AudioTestRequest {
    pub id: String,
    pub devices: VoiceAudioConfig,
    pub microphone: MicrophoneSettings,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AudioTestEvent {
    pub id: String,
    pub phase: &'static str,
    pub remaining_ms: u64,
    pub level: f32,
    pub clipped: bool,
    pub error: Option<String>,
}
pub(super) struct AudioTestHandle {
    id: String,
    cancel: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for AudioTestHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl DesktopState {
    pub(crate) fn audio_device_settings(&self) -> VoiceAudioConfig {
        self.shared.audio_devices.lock().clone()
    }

    pub(crate) fn initialize_audio<R: Runtime>(&self, app: &AppHandle<R>) {
        let loaded = app
            .path()
            .app_config_dir()
            .map_err(|e| e.to_string())
            .and_then(|dir| {
                crate::preferences::load_audio_devices(&dir.join("audio-devices.json"))
            });
        let config = match loaded {
            Ok(Some(config)) => config,
            Ok(None) => VoiceAudioConfig {
                input_device: std::env::var("CONCORD_AUDIO_INPUT").ok(),
                output_device: std::env::var("CONCORD_AUDIO_OUTPUT").ok(),
            },
            Err(error) => {
                warn!(%error,"using default audio devices");
                VoiceAudioConfig::default()
            }
        };
        *self.shared.audio_devices.lock() = config;
    }

    pub async fn set_audio_devices<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: VoiceAudioConfig,
    ) -> Result<VoiceAudioConfig, ClientError> {
        settings
            .validate()
            .map_err(|e| ClientError::Microphone(e.to_string()))?;
        let _update = self.shared.audio_update.lock().await;
        let _lifecycle = self.shared.call_lifecycle.lock().await;
        self.stop_audio_test().await;
        let previous = self.audio_device_settings();
        if previous == settings {
            return Ok(settings);
        }
        let commands = match &*self.shared.call.lock() {
            CallSlot::Active(handle) => Some(handle.commands.clone()),
            _ => None,
        };
        let before = previous.clone();
        let next = settings.clone();
        let in_call = commands.is_some();
        let prepared = tokio::task::spawn_blocking(move || {
            if in_call {
                VoiceAudio::prepare_change(&before, &next).map(Some)
            } else {
                VoiceAudio::validate_devices(&next).map(|()| None)
            }
        })
        .await
        .map_err(|e| ClientError::Microphone(e.to_string()))?
        .map_err(|e| {
            ClientError::Microphone(format!(
                "Não foi possível usar os dispositivos escolhidos: {e}"
            ))
        })?;
        let path = app
            .path()
            .app_config_dir()
            .map_err(|e| ClientError::Microphone(e.to_string()))?
            .join("audio-devices.json");
        let saved = settings.clone();
        tokio::task::spawn_blocking(move || crate::preferences::save_audio_devices(&path, &saved))
            .await
            .map_err(|e| ClientError::Microphone(e.to_string()))?
            .map_err(ClientError::Microphone)?;
        *self.shared.audio_devices.lock() = settings.clone();
        if let (Some(commands), Some(change)) = (commands, prepared) {
            let (ack, receiver) = oneshot::channel();
            if commands
                .send(CallCommand::InstallAudio { change, ack })
                .is_ok()
            {
                // If the call ends concurrently, the saved choice still applies
                // to the next call and the prepared streams are dropped.
                let _ = receiver.await;
            }
        }
        self.publish(app);
        Ok(settings)
    }

    pub async fn set_participant_volume(
        &self,
        user_id: &str,
        volume: u16,
    ) -> Result<(), ClientError> {
        if volume > 200 {
            return Err(ClientError::Call(
                "O volume deve ficar entre 0% e 200%.".into(),
            ));
        }
        let user_id = UserId::parse(user_id)
            .map_err(|_| ClientError::Call("Participante inválido.".into()))?;
        let (ack, receiver) = oneshot::channel();
        {
            let slot = self.shared.call.lock();
            let CallSlot::Active(handle) = &*slot else {
                return Err(ClientError::Call(
                    "Entre na chamada para ajustar o volume.".into(),
                ));
            };
            handle
                .commands
                .send(CallCommand::SetPeerVolume {
                    user_id,
                    volume,
                    ack,
                })
                .map_err(|_| ClientError::Call("A chamada foi encerrada.".into()))?;
        }
        receiver
            .await
            .map_err(|_| ClientError::Call("A chamada foi encerrada.".into()))?
            .map_err(ClientError::Call)
    }

    pub(super) async fn stop_audio_test(&self) {
        let handle = self.shared.audio_test.lock().take();
        if let Some(handle) = handle {
            handle.cancel.store(true, Ordering::Relaxed);
            // Borrow the task because the handle has Drop cancellation.
            let mut handle = handle;
            let _ = (&mut handle.task).await;
        }
    }
    pub async fn stop_audio_test_requested(&self) {
        let _lifecycle = self.shared.call_lifecycle.lock().await;
        self.stop_audio_test().await;
    }

    pub(crate) async fn start_audio_test<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        request: AudioTestRequest,
    ) -> Result<(), ClientError> {
        request
            .devices
            .validate()
            .map_err(|e| ClientError::Microphone(e.to_string()))?;
        request
            .microphone
            .validate()
            .map_err(|e| ClientError::Microphone(e.to_string()))?;
        uuid::Uuid::parse_str(&request.id)
            .map_err(|_| ClientError::Microphone("Teste de áudio inválido.".into()))?;
        let _lifecycle = self.shared.call_lifecycle.lock().await;
        if !matches!(*self.shared.call.lock(), CallSlot::Idle) {
            return Err(ClientError::Call(
                "O teste está disponível antes de entrar na chamada.".into(),
            ));
        }
        self.stop_audio_test().await;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let id = request.id.clone();
        let desktop = self.clone();
        let app = app.clone();
        let (ready, gate) = std::sync::mpsc::channel();
        let task = tokio::task::spawn_blocking(move || {
            if gate.recv().is_err() {
                return;
            }
            let emit = |phase, remaining_ms, level, clipped, error| {
                let _ = app.emit(
                    "concord:audio-test",
                    AudioTestEvent {
                        id: request.id.clone(),
                        phase,
                        remaining_ms,
                        level,
                        clipped,
                        error,
                    },
                );
            };
            emit("opening", 0, 0.0, false, None);
            let result = run_test(
                &request,
                &worker_cancel,
                |phase, remaining, level, clipped| emit(phase, remaining, level, clipped, None),
            );
            match result {
                Ok(()) => emit("stopped", 0, 0.0, false, None),
                Err(error) => emit("error", 0, 0.0, false, Some(error)),
            }
            let mut slot = desktop.shared.audio_test.lock();
            if slot.as_ref().is_some_and(|handle| handle.id == request.id) {
                slot.take();
            }
        });
        *self.shared.audio_test.lock() = Some(AudioTestHandle { id, cancel, task });
        let _ = ready.send(());
        Ok(())
    }
}

trait TestAudio {
    fn capture(&mut self) -> [f32; OPUS_FRAME_SAMPLES];
    fn playback(&mut self, frame: &[f32; OPUS_FRAME_SAMPLES]);
    fn failed(&self) -> bool;
}
impl TestAudio for VoiceAudio {
    fn capture(&mut self) -> [f32; OPUS_FRAME_SAMPLES] {
        let mut frame = [0.0; OPUS_FRAME_SAMPLES];
        self.read_capture_frame(&mut frame);
        frame
    }
    fn playback(&mut self, frame: &[f32; OPUS_FRAME_SAMPLES]) {
        self.push_playback_frame(frame);
    }
    fn failed(&self) -> bool {
        self.stats().stream_errors > 0
    }
}

fn run_test(
    request: &AudioTestRequest,
    cancel: &AtomicBool,
    report: impl FnMut(&'static str, u64, f32, bool),
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut audio = VoiceAudio::open(&request.devices)
        .map_err(|e| format!("Não foi possível iniciar o teste: {e}"))?;
    let step = Duration::from_millis(20);
    let mut deadline = std::time::Instant::now();
    run_test_session(&mut audio, request.microphone, cancel, report, || {
        deadline += step;
        std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    })
}

fn run_test_session(
    audio: &mut impl TestAudio,
    settings: MicrophoneSettings,
    cancel: &AtomicBool,
    mut report: impl FnMut(&'static str, u64, f32, bool),
    mut wait: impl FnMut(),
) -> Result<(), String> {
    let mut processor = MicrophoneProcessor::new().map_err(|e| e.to_string())?;
    let mut frames = Vec::with_capacity(250);
    for index in 0..250 {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut frame = audio.capture();
        let stats = processor.process(&mut frame, settings);
        frames.push(frame);
        if index % 5 == 0 {
            report("recording", (250 - index) * 20, stats.level, stats.clipped);
        }
        if audio.failed() {
            return Err(
                "O dispositivo parou durante o teste. Atualize a lista e tente novamente.".into(),
            );
        }
        wait();
    }
    for (index, frame) in frames.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        audio.playback(frame);
        if index % 5 == 0 {
            report(
                "playing",
                ((250 - index) * 20) as u64,
                concord_media::activity::rms(frame),
                false,
            );
        }
        if audio.failed() {
            return Err("A saída de áudio parou durante o teste.".into());
        }
        wait();
    }
    for _ in 0..8 {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        wait();
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeAudio {
        captured: usize,
        played: Vec<[f32; OPUS_FRAME_SAMPLES]>,
        fail: bool,
    }
    impl TestAudio for FakeAudio {
        fn capture(&mut self) -> [f32; OPUS_FRAME_SAMPLES] {
            self.captured += 1;
            [0.2; OPUS_FRAME_SAMPLES]
        }
        fn playback(&mut self, frame: &[f32; OPUS_FRAME_SAMPLES]) {
            assert_eq!(self.captured, 250);
            self.played.push(*frame);
        }
        fn failed(&self) -> bool {
            self.fail
        }
    }
    #[test]
    fn voice_test_records_before_playing_and_applies_saved_processing() {
        let mut audio = FakeAudio::default();
        let cancel = AtomicBool::new(false);
        let mut phases = vec![];
        run_test_session(
            &mut audio,
            MicrophoneSettings {
                volume_percent: 50,
                noise_suppression: false,
            },
            &cancel,
            |phase, _, _, _| phases.push(phase),
            || {},
        )
        .expect("test");
        assert_eq!(audio.captured, 250);
        assert_eq!(audio.played.len(), 250);
        assert!((audio.played[249][0] - 0.1).abs() < 0.00001);
        assert!(phases[..50].iter().all(|phase| *phase == "recording"));
        assert!(phases[50..].iter().all(|phase| *phase == "playing"));
    }
    #[test]
    fn stopping_or_device_errors_prevent_playback() {
        let mut audio = FakeAudio::default();
        let cancel = AtomicBool::new(false);
        let mut ticks = 0;
        run_test_session(
            &mut audio,
            MicrophoneSettings::default(),
            &cancel,
            |_, _, _, _| {},
            || {
                ticks += 1;
                if ticks == 3 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .expect("cancel");
        assert_eq!(audio.captured, 3);
        assert!(audio.played.is_empty());
        let mut failed = FakeAudio {
            fail: true,
            ..FakeAudio::default()
        };
        assert!(
            run_test_session(
                &mut failed,
                MicrophoneSettings::default(),
                &AtomicBool::new(false),
                |_, _, _, _| {},
                || {}
            )
            .is_err()
        );
        assert!(failed.played.is_empty());
    }
    #[test]
    fn canceled_test_does_not_open_devices() {
        let request = AudioTestRequest {
            id: uuid::Uuid::new_v4().to_string(),
            devices: VoiceAudioConfig {
                input_device: Some("invalid-device".to_owned()),
                output_device: None,
            },
            microphone: MicrophoneSettings::default(),
        };
        run_test(&request, &AtomicBool::new(true), |_, _, _, _| {}).expect("already canceled");
    }
}
