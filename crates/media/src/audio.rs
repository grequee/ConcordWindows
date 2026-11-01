//! CPAL audio capture and local loopback.
//!
//! This is the first half of the audio path: enumerate devices, capture the
//! microphone, convert to interleaved `f32`, and optionally play the same
//! samples back. Opus encoding/decoding and network jitter buffering are added
//! later; keeping the callback path free of allocation is a prerequisite for
//! that work.
//!
//! The callbacks never allocate. They push into a lock-free SPSC ring buffer
//! and atomically count samples and drops so the caller can measure stability.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, DeviceId, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedStreamConfig,
};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::{AudioFormat, MediaError};

const DEFAULT_LATENCY_MS: u32 = 150;
const MIN_LATENCY_MS: u32 = 20;
const MAX_LATENCY_MS: u32 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioDeviceInfo {
    /// Stable identifier accepted by [`LoopbackConfig::input_device`] and
    /// [`LoopbackConfig::output_device`].
    pub id: String,
    pub name: String,
    pub is_default_input: bool,
    pub is_default_output: bool,
    pub input_channels: Option<u16>,
    pub output_channels: Option<u16>,
    pub input_sample_rate: Option<u32>,
    pub output_sample_rate: Option<u32>,
}

/// List the devices of the default CPAL host. Devices that expose neither an
/// input nor an output are omitted.
pub fn list_audio_devices() -> Result<Vec<AudioDeviceInfo>, MediaError> {
    let host = cpal::default_host();
    let default_input = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let default_output = host
        .default_output_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());

    let devices = host
        .devices()
        .map_err(|error| audio_error("could not enumerate audio devices", error))?;

    let mut result = Vec::new();
    for device in devices {
        let input_config = device.default_input_config().ok();
        let output_config = device.default_output_config().ok();
        if input_config.is_none() && output_config.is_none() {
            continue;
        }

        let id = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| device.to_string());
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| device.to_string());

        result.push(AudioDeviceInfo {
            is_default_input: default_input.as_deref() == Some(id.as_str()),
            is_default_output: default_output.as_deref() == Some(id.as_str()),
            id,
            name,
            input_channels: input_config.as_ref().map(SupportedStreamConfig::channels),
            output_channels: output_config.as_ref().map(SupportedStreamConfig::channels),
            input_sample_rate: input_config
                .as_ref()
                .map(SupportedStreamConfig::sample_rate),
            output_sample_rate: output_config
                .as_ref()
                .map(SupportedStreamConfig::sample_rate),
        });
    }

    Ok(result)
}

#[derive(Debug, Clone)]
pub struct LoopbackConfig {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Target delay in the local playback ring. Larger values absorb more
    /// scheduling jitter at the cost of echo.
    pub latency_ms: u32,
    /// Stop automatically after this duration. `None` runs until `stop` is set.
    pub duration: Option<Duration>,
    /// Capture-only mode: useful when testing a microphone without creating a
    /// feedback loop through the speakers.
    pub playback: bool,
}

impl Default for LoopbackConfig {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            latency_ms: DEFAULT_LATENCY_MS,
            duration: None,
            playback: true,
        }
    }
}

impl LoopbackConfig {
    pub fn validate(&self) -> Result<(), MediaError> {
        if !(MIN_LATENCY_MS..=MAX_LATENCY_MS).contains(&self.latency_ms) {
            return Err(MediaError::InvalidSettings(
                "latency must be between 20 and 2000 ms",
            ));
        }
        if self.duration.is_some_and(|duration| duration.is_zero()) {
            return Err(MediaError::InvalidSettings(
                "loopback duration must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LoopbackStats {
    pub elapsed: Duration,
    pub format: AudioFormat,
    pub output_format: Option<AudioFormat>,
    pub input_samples: u64,
    pub output_samples: u64,
    pub input_dropped: u64,
    pub output_underruns: u64,
    pub stream_errors: u64,
}

#[derive(Default)]
struct CallbackStats {
    input_samples: AtomicU64,
    output_samples: AtomicU64,
    input_dropped: AtomicU64,
    output_underruns: AtomicU64,
    stream_errors: AtomicU64,
}

/// Open the selected devices and run a local loopback until the duration
/// expires or `stop` becomes true. This is intentionally blocking: audio
/// callbacks and device setup stay on one thread, outside the async runtime.
pub fn run_loopback(
    config: &LoopbackConfig,
    stop: Arc<AtomicBool>,
) -> Result<LoopbackStats, MediaError> {
    config.validate()?;

    let host = cpal::default_host();
    let input_device = resolve_input_device(&host, config.input_device.as_deref())?;
    let input_supported = input_device
        .default_input_config()
        .map_err(|error| audio_error("could not query the input device", error))?;
    let format = AudioFormat {
        sample_rate: input_supported.sample_rate(),
        channels: input_supported.channels(),
    };

    let output = if config.playback {
        let output_device = resolve_output_device(&host, config.output_device.as_deref())?;
        let supported = select_output_config(&output_device, format)?;
        Some((output_device, supported))
    } else {
        None
    };

    let output_format = output.as_ref().map(|(_, supported)| AudioFormat {
        sample_rate: supported.sample_rate(),
        channels: supported.channels(),
    });

    let samples_of_latency = (format.sample_rate as usize * config.latency_ms as usize / 1_000)
        * format.channels as usize;
    let ring_capacity = samples_of_latency.saturating_mul(2).max(256);
    let ring = HeapRb::<f32>::new(ring_capacity);
    let (mut producer, consumer) = ring.split();

    if output.is_some() {
        for _ in 0..samples_of_latency {
            if producer.try_push(0.0).is_err() {
                break;
            }
        }
    }

    let stats = Arc::new(CallbackStats::default());
    let input_config = input_supported.config();
    let input_stream = build_input_stream(
        &input_device,
        input_supported.sample_format(),
        input_config,
        output.is_some().then_some(producer),
        stats.clone(),
    )?;

    let output_stream = if let Some((device, supported)) = output {
        let config = supported.config();
        Some(build_output_stream(
            &device,
            supported.sample_format(),
            config,
            consumer,
            stats.clone(),
        )?)
    } else {
        None
    };

    input_stream
        .play()
        .map_err(|error| audio_error("could not start the input stream", error))?;
    if let Some(stream) = &output_stream {
        stream
            .play()
            .map_err(|error| audio_error("could not start the output stream", error))?;
    }

    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if let Some(duration) = config.duration
            && started.elapsed() >= duration
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    drop(output_stream);
    drop(input_stream);

    Ok(LoopbackStats {
        elapsed: started.elapsed(),
        format,
        output_format,
        input_samples: stats.input_samples.load(Ordering::Relaxed),
        output_samples: stats.output_samples.load(Ordering::Relaxed),
        input_dropped: stats.input_dropped.load(Ordering::Relaxed),
        output_underruns: stats.output_underruns.load(Ordering::Relaxed),
        stream_errors: stats.stream_errors.load(Ordering::Relaxed),
    })
}

pub(crate) fn resolve_input_device(
    host: &cpal::Host,
    id: Option<&str>,
) -> Result<Device, MediaError> {
    match id {
        Some(id) => {
            let parsed: DeviceId = id
                .parse()
                .map_err(|error| audio_error("invalid input device id", error))?;
            host.device_by_id(&parsed).ok_or(MediaError::SourceNotFound)
        }
        None => host
            .default_input_device()
            .ok_or_else(|| MediaError::Audio("no default input device".to_owned())),
    }
}

pub(crate) fn resolve_output_device(
    host: &cpal::Host,
    id: Option<&str>,
) -> Result<Device, MediaError> {
    match id {
        Some(id) => {
            let parsed: DeviceId = id
                .parse()
                .map_err(|error| audio_error("invalid output device id", error))?;
            host.device_by_id(&parsed).ok_or(MediaError::SourceNotFound)
        }
        None => host
            .default_output_device()
            .ok_or_else(|| MediaError::Audio("no default output device".to_owned())),
    }
}

fn select_output_config(
    device: &Device,
    target: AudioFormat,
) -> Result<SupportedStreamConfig, MediaError> {
    let default = device
        .default_output_config()
        .map_err(|error| audio_error("could not query the output device", error))?;
    if default.channels() == target.channels && default.sample_rate() == target.sample_rate {
        return Ok(default);
    }

    let configs = device
        .supported_output_configs()
        .map_err(|error| audio_error("could not query output formats", error))?;
    for range in configs {
        if range.channels() != target.channels
            || target.sample_rate < range.min_sample_rate()
            || target.sample_rate > range.max_sample_rate()
        {
            continue;
        }
        if let Some(config) = range.try_with_sample_rate(target.sample_rate) {
            return Ok(config);
        }
    }

    Err(MediaError::InvalidSettings(
        "input and output devices must share sample rate and channel count",
    ))
}

fn build_input_stream(
    device: &Device,
    sample_format: SampleFormat,
    config: StreamConfig,
    producer: Option<ringbuf::HeapProd<f32>>,
    stats: Arc<CallbackStats>,
) -> Result<Stream, MediaError> {
    match sample_format {
        SampleFormat::I8 => build_input::<i8>(device, config, producer, stats),
        SampleFormat::I16 => build_input::<i16>(device, config, producer, stats),
        SampleFormat::I24 => build_input::<cpal::I24>(device, config, producer, stats),
        SampleFormat::I32 => build_input::<i32>(device, config, producer, stats),
        SampleFormat::I64 => build_input::<i64>(device, config, producer, stats),
        SampleFormat::U8 => build_input::<u8>(device, config, producer, stats),
        SampleFormat::U16 => build_input::<u16>(device, config, producer, stats),
        SampleFormat::U24 => build_input::<cpal::U24>(device, config, producer, stats),
        SampleFormat::U32 => build_input::<u32>(device, config, producer, stats),
        SampleFormat::U64 => build_input::<u64>(device, config, producer, stats),
        SampleFormat::F32 => build_input::<f32>(device, config, producer, stats),
        SampleFormat::F64 => build_input::<f64>(device, config, producer, stats),
        other => Err(MediaError::Audio(format!(
            "unsupported capture sample format: {other}"
        ))),
    }
}

fn build_input<T>(
    device: &Device,
    config: StreamConfig,
    producer: Option<ringbuf::HeapProd<f32>>,
    stats: Arc<CallbackStats>,
) -> Result<Stream, MediaError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let mut producer = producer;
    let error_stats = stats.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if let Some(producer) = producer.as_mut() {
                    let mut pushed = 0u64;
                    for sample in data {
                        if producer.try_push(f32::from_sample(*sample)).is_ok() {
                            pushed += 1;
                        }
                    }
                    stats.input_samples.fetch_add(pushed, Ordering::Relaxed);
                    stats
                        .input_dropped
                        .fetch_add(data.len() as u64 - pushed, Ordering::Relaxed);
                } else {
                    stats
                        .input_samples
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                }
            },
            move |error| {
                error_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("audio input stream error: {error}");
            },
            None,
        )
        .map_err(|error| audio_error("could not build the input stream", error))
}

fn build_output_stream(
    device: &Device,
    sample_format: SampleFormat,
    config: StreamConfig,
    consumer: ringbuf::HeapCons<f32>,
    stats: Arc<CallbackStats>,
) -> Result<Stream, MediaError> {
    match sample_format {
        SampleFormat::I8 => build_output::<i8>(device, config, consumer, stats),
        SampleFormat::I16 => build_output::<i16>(device, config, consumer, stats),
        SampleFormat::I24 => build_output::<cpal::I24>(device, config, consumer, stats),
        SampleFormat::I32 => build_output::<i32>(device, config, consumer, stats),
        SampleFormat::I64 => build_output::<i64>(device, config, consumer, stats),
        SampleFormat::U8 => build_output::<u8>(device, config, consumer, stats),
        SampleFormat::U16 => build_output::<u16>(device, config, consumer, stats),
        SampleFormat::U24 => build_output::<cpal::U24>(device, config, consumer, stats),
        SampleFormat::U32 => build_output::<u32>(device, config, consumer, stats),
        SampleFormat::U64 => build_output::<u64>(device, config, consumer, stats),
        SampleFormat::F32 => build_output::<f32>(device, config, consumer, stats),
        SampleFormat::F64 => build_output::<f64>(device, config, consumer, stats),
        other => Err(MediaError::Audio(format!(
            "unsupported playback sample format: {other}"
        ))),
    }
}

fn build_output<T>(
    device: &Device,
    config: StreamConfig,
    mut consumer: ringbuf::HeapCons<f32>,
    stats: Arc<CallbackStats>,
) -> Result<Stream, MediaError>
where
    T: SizedSample + Send + 'static,
    T: FromSample<f32>,
{
    let error_stats = stats.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut filled = 0u64;
                for output in data.iter_mut() {
                    if let Some(sample) = consumer.try_pop() {
                        *output = T::from_sample(sample);
                        filled += 1;
                    } else {
                        *output = T::EQUILIBRIUM;
                    }
                }
                stats.output_samples.fetch_add(filled, Ordering::Relaxed);
                stats
                    .output_underruns
                    .fetch_add(data.len() as u64 - filled, Ordering::Relaxed);
            },
            move |error| {
                error_stats.stream_errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("audio output stream error: {error}");
            },
            None,
        )
        .map_err(|error| audio_error("could not build the output stream", error))
}

pub(crate) fn audio_error(context: &str, error: impl std::fmt::Display) -> MediaError {
    MediaError::Audio(format!("{context}: {error}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn loopback_config_is_validated() {
        let mut config = LoopbackConfig::default();
        assert!(config.validate().is_ok());
        config.latency_ms = 0;
        assert!(config.validate().is_err());
        config.latency_ms = DEFAULT_LATENCY_MS;
        config.duration = Some(Duration::ZERO);
        assert!(config.validate().is_err());
    }
}
