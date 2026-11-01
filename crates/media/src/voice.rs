//! Full-duplex voice audio for the native call path.
//!
//! CPAL callbacks run on OS audio threads. They never allocate and never talk
//! to Tokio directly: capture is downmixed and resampled to 48 kHz mono into a
//! lock-free ring, while decoded peer audio arrives through a second ring that
//! the output callback resamples to the device format. The async call actor
//! drains/produces exactly one 20 ms Opus frame per tick.
//!
//! The resamplers are deliberately small linear interpolators. Voice traffic
//! at 48 kHz through the default device is the common case; resampling only
//! keeps 44.1 kHz hardware usable without a heavyweight DSP dependency.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};

use crate::audio::{audio_error, resolve_input_device, resolve_output_device};
use crate::{MediaError, OPUS_FRAME_SAMPLES, OPUS_SAMPLE_RATE};

const INPUT_RING_SAMPLES: usize = OPUS_SAMPLE_RATE as usize; // 1 second
const OUTPUT_RING_SAMPLES: usize = (OPUS_SAMPLE_RATE as usize) / 4; // 250 ms
const OUTPUT_PREFILL_MS: usize = 120;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceAudioConfig {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceAudioStats {
    pub capture_samples: u64,
    pub capture_dropped: u64,
    pub capture_underruns: u64,
    pub playback_samples: u64,
    pub playback_dropped: u64,
    pub playback_underruns: u64,
    pub stream_errors: u64,
}

#[derive(Default)]
struct VoiceCounters {
    capture_samples: AtomicU64,
    capture_dropped: AtomicU64,
    capture_underruns: AtomicU64,
    playback_samples: AtomicU64,
    playback_dropped: AtomicU64,
    playback_underruns: AtomicU64,
    stream_errors: AtomicU64,
}

impl VoiceCounters {
    fn snapshot(&self) -> VoiceAudioStats {
        VoiceAudioStats {
            capture_samples: self.capture_samples.load(Ordering::Relaxed),
            capture_dropped: self.capture_dropped.load(Ordering::Relaxed),
            capture_underruns: self.capture_underruns.load(Ordering::Relaxed),
            playback_samples: self.playback_samples.load(Ordering::Relaxed),
            playback_dropped: self.playback_dropped.load(Ordering::Relaxed),
            playback_underruns: self.playback_underruns.load(Ordering::Relaxed),
            stream_errors: self.stream_errors.load(Ordering::Relaxed),
        }
    }
}

impl VoiceAudioConfig {
    pub fn validate(&self) -> Result<(), MediaError> {
        for id in [&self.input_device, &self.output_device]
            .into_iter()
            .flatten()
        {
            if id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
                return Err(MediaError::InvalidSettings(
                    "identificador de dispositivo de áudio inválido",
                ));
            }
        }
        Ok(())
    }
}

struct InputAudio {
    _stream: Stream,
    capture: ringbuf::HeapCons<f32>,
    counters: Arc<VoiceCounters>,
    format: cpal::SupportedStreamConfig,
}
struct OutputAudio {
    _stream: Stream,
    playback: ringbuf::HeapProd<f32>,
    counters: Arc<VoiceCounters>,
    format: cpal::SupportedStreamConfig,
}

/// Prepared on a blocking worker while the current call continues. Only the
/// changed direction is opened; unchanged exclusive devices are not reopened.
pub struct PreparedAudioChange {
    input: Option<InputAudio>,
    output: Option<OutputAudio>,
}
impl std::fmt::Debug for PreparedAudioChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedAudioChange")
            .field("input", &self.input.is_some())
            .field("output", &self.output.is_some())
            .finish()
    }
}

pub struct VoiceAudio {
    input: InputAudio,
    output: OutputAudio,
}

fn open_input(id: Option<&str>) -> Result<InputAudio, MediaError> {
    let host = cpal::default_host();
    let device = resolve_input_device(&host, id)?;
    let format = device
        .default_input_config()
        .map_err(|e| audio_error("não foi possível consultar o microfone", e))?;
    let (producer, consumer) = HeapRb::<f32>::new(INPUT_RING_SAMPLES).split();
    let counters = Arc::new(VoiceCounters::default());
    let stream = build_input_stream(
        &device,
        format.sample_format(),
        format.config(),
        producer,
        counters.clone(),
    )?;
    stream
        .play()
        .map_err(|e| audio_error("não foi possível abrir o microfone", e))?;
    Ok(InputAudio {
        _stream: stream,
        capture: consumer,
        counters,
        format,
    })
}
fn open_output(id: Option<&str>) -> Result<OutputAudio, MediaError> {
    let host = cpal::default_host();
    let device = resolve_output_device(&host, id)?;
    let format = device
        .default_output_config()
        .map_err(|e| audio_error("não foi possível consultar a saída de áudio", e))?;
    let (mut producer, consumer) = HeapRb::<f32>::new(OUTPUT_RING_SAMPLES).split();
    for _ in 0..OUTPUT_RING_SAMPLES.min(OPUS_SAMPLE_RATE as usize * OUTPUT_PREFILL_MS / 1000) {
        let _ = producer.try_push(0.0);
    }
    let counters = Arc::new(VoiceCounters::default());
    let stream = build_output_stream(
        &device,
        format.sample_format(),
        format.config(),
        consumer,
        counters.clone(),
    )?;
    stream
        .play()
        .map_err(|e| audio_error("não foi possível abrir a saída de áudio", e))?;
    Ok(OutputAudio {
        _stream: stream,
        playback: producer,
        counters,
        format,
    })
}
fn prepare_selected<I, O>(
    previous: &VoiceAudioConfig,
    next: &VoiceAudioConfig,
    input: impl FnOnce(Option<&str>) -> Result<I, MediaError>,
    output: impl FnOnce(Option<&str>) -> Result<O, MediaError>,
) -> Result<(Option<I>, Option<O>), MediaError> {
    next.validate()?;
    let input = if previous.input_device != next.input_device {
        Some(input(next.input_device.as_deref())?)
    } else {
        None
    };
    let output = if previous.output_device != next.output_device {
        Some(output(next.output_device.as_deref())?)
    } else {
        None
    };
    Ok((input, output))
}

impl VoiceAudio {
    pub fn open(config: &VoiceAudioConfig) -> Result<Self, MediaError> {
        config.validate()?;
        Ok(Self {
            input: open_input(config.input_device.as_deref())?,
            output: open_output(config.output_device.as_deref())?,
        })
    }
    /// Resolve selections without starting capture or playback.
    pub fn validate_devices(config: &VoiceAudioConfig) -> Result<(), MediaError> {
        config.validate()?;
        let host = cpal::default_host();
        resolve_input_device(&host, config.input_device.as_deref())?
            .default_input_config()
            .map_err(|e| audio_error("microfone indisponível", e))?;
        resolve_output_device(&host, config.output_device.as_deref())?
            .default_output_config()
            .map_err(|e| audio_error("saída de áudio indisponível", e))?;
        Ok(())
    }
    pub fn prepare_change(
        previous: &VoiceAudioConfig,
        next: &VoiceAudioConfig,
    ) -> Result<PreparedAudioChange, MediaError> {
        let (input, output) = prepare_selected(previous, next, open_input, open_output)?;
        Ok(PreparedAudioChange { input, output })
    }
    pub fn apply_change(&mut self, change: PreparedAudioChange) {
        if let Some(mut input) = change.input {
            // Discard anything recorded while the replacement was being saved.
            while input.capture.try_pop().is_some() {}
            self.input = input;
        }
        if let Some(output) = change.output {
            self.output = output;
        }
    }
    pub fn read_capture_frame(&mut self, frame: &mut [f32; OPUS_FRAME_SAMPLES]) -> usize {
        let mut read = 0;
        while read < frame.len() {
            match self.input.capture.try_pop() {
                Some(sample) => {
                    frame[read] = sample;
                    read += 1;
                }
                None => break,
            }
        }
        if read < frame.len() {
            frame[read..].fill(0.0);
            self.input
                .counters
                .capture_underruns
                .fetch_add(1, Ordering::Relaxed);
        }
        read
    }
    pub fn push_playback_frame(&mut self, frame: &[f32]) {
        if self.output.playback.vacant_len() < frame.len() {
            self.output
                .counters
                .playback_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.output.playback.push_slice(frame) < frame.len() {
            self.output
                .counters
                .playback_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn stats(&self) -> VoiceAudioStats {
        let input = self.input.counters.snapshot();
        let output = self.output.counters.snapshot();
        VoiceAudioStats {
            capture_samples: input.capture_samples,
            capture_dropped: input.capture_dropped,
            capture_underruns: input.capture_underruns,
            playback_samples: output.playback_samples,
            playback_dropped: output.playback_dropped,
            playback_underruns: output.playback_underruns,
            stream_errors: input.stream_errors.saturating_add(output.stream_errors),
        }
    }
    pub fn input_device_format(&self) -> (u32, u16) {
        (
            self.input.format.sample_rate(),
            self.input.format.channels(),
        )
    }
    pub fn output_device_format(&self) -> (u32, u16) {
        (
            self.output.format.sample_rate(),
            self.output.format.channels(),
        )
    }
}

/// End of the owned stream declarations is also where the compiler can prove
/// the actor may keep the device handles on the Tokio task.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<VoiceAudio>();
};

#[derive(Debug)]
struct CaptureResampler {
    channels: usize,
    step: f64,
    phase: f64,
    previous: f32,
    has_previous: bool,
}

impl CaptureResampler {
    fn new(input_rate: u32, channels: u16) -> Self {
        Self {
            channels: usize::from(channels.max(1)),
            step: f64::from(input_rate) / f64::from(OPUS_SAMPLE_RATE),
            phase: 0.0,
            previous: 0.0,
            has_previous: false,
        }
    }

    fn push_frames(&mut self, data: &[f32], mut out: impl FnMut(f32)) {
        for frame in data.chunks_exact(self.channels) {
            let mono = if self.channels == 1 {
                frame[0]
            } else {
                frame.iter().copied().sum::<f32>() / self.channels as f32
            };

            if !self.has_previous {
                self.previous = mono;
                self.has_previous = true;
                continue;
            }

            while self.phase < 1.0 {
                let sample = self.previous + (mono - self.previous) * self.phase as f32;
                out(sample);
                self.phase += self.step;
            }
            self.phase -= 1.0;
            self.previous = mono;
        }
    }
}

#[derive(Debug)]
struct PlaybackResampler {
    step: f64,
    phase: f64,
    previous: f32,
    current: f32,
    has_current: bool,
}

impl PlaybackResampler {
    fn new(output_rate: u32) -> Self {
        Self {
            step: f64::from(OPUS_SAMPLE_RATE) / f64::from(output_rate),
            phase: 0.0,
            previous: 0.0,
            current: 0.0,
            has_current: false,
        }
    }

    fn next_sample(&mut self, mut source: impl FnMut() -> Option<f32>) -> (f32, bool) {
        if !self.has_current {
            match source() {
                Some(sample) => {
                    self.previous = sample;
                    self.current = sample;
                    self.has_current = true;
                }
                None => return (0.0, true),
            }
        }

        while self.phase >= 1.0 {
            self.previous = self.current;
            match source() {
                Some(sample) => self.current = sample,
                None => {
                    self.current = 0.0;
                    self.phase -= 1.0;
                    return (0.0, true);
                }
            }
            self.phase -= 1.0;
        }

        let sample = self.previous + (self.current - self.previous) * self.phase as f32;
        self.phase += self.step;
        (sample, false)
    }
}

fn build_input_stream(
    device: &Device,
    sample_format: SampleFormat,
    config: StreamConfig,
    producer: ringbuf::HeapProd<f32>,
    counters: Arc<VoiceCounters>,
) -> Result<Stream, MediaError> {
    match sample_format {
        SampleFormat::I8 => build_input::<i8>(device, config, producer, counters),
        SampleFormat::I16 => build_input::<i16>(device, config, producer, counters),
        SampleFormat::I24 => build_input::<cpal::I24>(device, config, producer, counters),
        SampleFormat::I32 => build_input::<i32>(device, config, producer, counters),
        SampleFormat::I64 => build_input::<i64>(device, config, producer, counters),
        SampleFormat::U8 => build_input::<u8>(device, config, producer, counters),
        SampleFormat::U16 => build_input::<u16>(device, config, producer, counters),
        SampleFormat::U24 => build_input::<cpal::U24>(device, config, producer, counters),
        SampleFormat::U32 => build_input::<u32>(device, config, producer, counters),
        SampleFormat::U64 => build_input::<u64>(device, config, producer, counters),
        SampleFormat::F32 => build_input::<f32>(device, config, producer, counters),
        SampleFormat::F64 => build_input::<f64>(device, config, producer, counters),
        other => Err(MediaError::Audio(format!(
            "unsupported microphone sample format: {other}"
        ))),
    }
}

fn build_input<T>(
    device: &Device,
    config: StreamConfig,
    mut producer: ringbuf::HeapProd<f32>,
    counters: Arc<VoiceCounters>,
) -> Result<Stream, MediaError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let sample_rate = config.sample_rate;
    let channels = config.channels;
    let mut resampler = CaptureResampler::new(sample_rate, channels);
    let error_counters = counters.clone();
    // Allocate once on the setup thread. The callback only fills these
    // preallocated slices; it never grows the heap. Keeping chunks aligned to
    // whole interleaved frames matters for multichannel devices.
    let mut converted = vec![0.0f32; 8_192];
    let channel_count = usize::from(channels.max(1));
    let chunk_samples = (converted.len() / channel_count).max(1) * channel_count;

    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut pushed = 0u64;
                let mut attempted = 0u64;
                for chunk in data.chunks(chunk_samples) {
                    for (destination, sample) in converted.iter_mut().zip(chunk.iter()) {
                        *destination = f32::from_sample(*sample);
                    }
                    resampler.push_frames(&converted[..chunk.len()], |sample| {
                        attempted += 1;
                        if producer.try_push(sample).is_ok() {
                            pushed += 1;
                        }
                    });
                }
                counters
                    .capture_samples
                    .fetch_add(pushed, Ordering::Relaxed);
                counters
                    .capture_dropped
                    .fetch_add(attempted.saturating_sub(pushed), Ordering::Relaxed);
            },
            move |error| {
                error_counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("voice microphone stream error: {error}");
            },
            None,
        )
        .map_err(|error| audio_error("could not build the voice input stream", error))
}

fn build_output_stream(
    device: &Device,
    sample_format: SampleFormat,
    config: StreamConfig,
    consumer: ringbuf::HeapCons<f32>,
    counters: Arc<VoiceCounters>,
) -> Result<Stream, MediaError> {
    match sample_format {
        SampleFormat::I8 => build_output::<i8>(device, config, consumer, counters),
        SampleFormat::I16 => build_output::<i16>(device, config, consumer, counters),
        SampleFormat::I24 => build_output::<cpal::I24>(device, config, consumer, counters),
        SampleFormat::I32 => build_output::<i32>(device, config, consumer, counters),
        SampleFormat::I64 => build_output::<i64>(device, config, consumer, counters),
        SampleFormat::U8 => build_output::<u8>(device, config, consumer, counters),
        SampleFormat::U16 => build_output::<u16>(device, config, consumer, counters),
        SampleFormat::U24 => build_output::<cpal::U24>(device, config, consumer, counters),
        SampleFormat::U32 => build_output::<u32>(device, config, consumer, counters),
        SampleFormat::U64 => build_output::<u64>(device, config, consumer, counters),
        SampleFormat::F32 => build_output::<f32>(device, config, consumer, counters),
        SampleFormat::F64 => build_output::<f64>(device, config, consumer, counters),
        other => Err(MediaError::Audio(format!(
            "unsupported playback sample format: {other}"
        ))),
    }
}

fn build_output<T>(
    device: &Device,
    config: StreamConfig,
    mut consumer: ringbuf::HeapCons<f32>,
    counters: Arc<VoiceCounters>,
) -> Result<Stream, MediaError>
where
    T: SizedSample + Send + 'static,
    T: FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    let mut resampler = PlaybackResampler::new(config.sample_rate);
    let error_counters = counters.clone();

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let frames = data.len() / channels;
                let mut underruns = 0u64;
                for frame in 0..frames {
                    let (sample, underrun) = resampler.next_sample(|| consumer.try_pop());
                    if underrun {
                        underruns += 1;
                    }
                    for channel in 0..channels {
                        data[frame * channels + channel] = T::from_sample(sample);
                    }
                }
                counters
                    .playback_samples
                    .fetch_add(frames as u64, Ordering::Relaxed);
                if underruns > 0 {
                    counters
                        .playback_underruns
                        .fetch_add(underruns, Ordering::Relaxed);
                }
            },
            move |error| {
                error_counters.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("voice playback stream error: {error}");
            },
            None,
        )
        .map_err(|error| audio_error("could not build the voice output stream", error))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn capture_resampling_produces_the_expected_rate() {
        for input_rate in [44_100u32, 48_000, 96_000] {
            let mut resampler = CaptureResampler::new(input_rate, 1);
            let input_frames = OPUS_SAMPLE_RATE as usize;
            let input_samples = (input_frames as f64 * f64::from(input_rate)
                / f64::from(OPUS_SAMPLE_RATE))
            .round() as usize;
            let input = vec![0.25f32; input_samples];
            let mut output = Vec::new();
            resampler.push_frames(&input, |sample| output.push(sample));
            let expected = input_samples * OPUS_SAMPLE_RATE as usize / input_rate as usize;
            let tolerance = expected / 100;
            assert!(
                output.len().abs_diff(expected) <= tolerance,
                "rate {input_rate}: got {}, expected about {expected}",
                output.len()
            );
        }
    }

    #[test]
    fn playback_resampling_is_bounded_at_48k() {
        let mut resampler = PlaybackResampler::new(48_000);
        let mut source = std::collections::VecDeque::from(vec![0.5f32; 960]);
        let mut output = Vec::new();
        for _ in 0..960 {
            let (sample, underrun) = resampler.next_sample(|| source.pop_front());
            assert!(!underrun || source.is_empty());
            output.push(sample);
        }
        assert_eq!(output.len(), 960);
        assert!(output.iter().all(|sample| (*sample - 0.5).abs() < 1e-5));
    }

    #[test]
    fn playback_resampling_upsamples_without_stalling() {
        let mut resampler = PlaybackResampler::new(44_100);
        let mut source = std::collections::VecDeque::from(vec![0.5f32; 960]);
        let mut produced = 0;
        for _ in 0..882 {
            let (_, _) = resampler.next_sample(|| source.pop_front());
            produced += 1;
        }
        assert_eq!(produced, 882);
    }

    /// Manual smoke test for the real CPAL path. It intentionally does not
    /// assert that the microphone is non-silent: privacy controls may mute the
    /// device while the stream is still healthy.
    #[test]
    #[ignore = "requires a real microphone and speaker"]
    fn real_devices_capture_and_play_for_one_second() {
        let mut audio = VoiceAudio::open(&VoiceAudioConfig::default()).expect("open audio");
        let mut captured = 0usize;
        let mut capture = [0.0f32; OPUS_FRAME_SAMPLES];
        for tick in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            captured += audio.read_capture_frame(&mut capture);
            let phase = tick as f32 * 0.02;
            let tone: Vec<f32> = (0..OPUS_FRAME_SAMPLES)
                .map(|index| {
                    let t = phase + index as f32 / OPUS_SAMPLE_RATE as f32;
                    (t * std::f32::consts::TAU * 440.0).sin() * 0.05
                })
                .collect();
            audio.push_playback_frame(&tone);
        }
        std::thread::sleep(Duration::from_millis(100));

        let stats = audio.stats();
        println!(
            "input {:?}, output {:?}, stats {stats:?}",
            audio.input_device_format(),
            audio.output_device_format()
        );
        assert!(captured > 0, "no capture samples arrived");
        assert!(stats.playback_samples > 0, "no playback callback ran");
        assert_eq!(stats.stream_errors, 0);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod device_change_tests {
    use super::*;
    #[test]
    fn changing_output_does_not_reopen_the_unchanged_microphone() {
        let previous = VoiceAudioConfig {
            input_device: Some("mic".into()),
            output_device: None,
        };
        let next = VoiceAudioConfig {
            output_device: Some("headset".into()),
            ..previous.clone()
        };
        let (input, output) = prepare_selected(
            &previous,
            &next,
            |_| Err::<(), _>(MediaError::SourceNotFound),
            |id| Ok(id.map(str::to_owned)),
        )
        .expect("output change");
        assert!(input.is_none());
        assert_eq!(output, Some(Some("headset".to_owned())));
    }
    #[test]
    fn failed_preparation_releases_new_streams_without_consuming_current_ones() {
        struct StreamGuard(Arc<AtomicU64>);
        impl Drop for StreamGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicU64::new(0));
        let previous = VoiceAudioConfig::default();
        let next = VoiceAudioConfig {
            input_device: Some("new-mic".into()),
            output_device: Some("missing-output".into()),
        };
        let result = prepare_selected(
            &previous,
            &next,
            |_| Ok(StreamGuard(drops.clone())),
            |_| Err::<(), _>(MediaError::SourceNotFound),
        );
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(previous, VoiceAudioConfig::default());
    }
}
