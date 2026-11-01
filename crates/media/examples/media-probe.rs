//! Manual media probe for milestone 2.
//!
//! Run with:
//! `cargo run -p concord-media --example media-probe -- audio-devices`
//! `cargo run -p concord-media --example media-probe -- audio-loopback --seconds 10`
//! `cargo run -p concord-media --features x11 --example media-probe -- screen-sources`
//! `cargo run -p concord-media --features x11 --example media-probe -- screen-probe --seconds 10 --fps 30`

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
#[cfg(all(feature = "x11", target_os = "linux"))]
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use concord_media::{LoopbackConfig, list_audio_devices, run_loopback};

#[cfg(all(feature = "x11", target_os = "linux"))]
use concord_media::{PROFILE_720P30, ScreenCapturer, X11Capturer, list_x11_sources};

#[derive(Debug, Parser)]
#[command(
    name = "media-probe",
    version,
    about = "Native capture and loopback probe"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List input/output devices and their default formats.
    AudioDevices,
    /// Capture the microphone and optionally play it back locally.
    AudioLoopback {
        /// Stable device id shown by `audio-devices`.
        #[arg(long)]
        input: Option<String>,
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value_t = 150)]
        latency_ms: u32,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Do not open the output device (avoids speaker feedback).
        #[arg(long)]
        no_playback: bool,
    },
    /// List X11/Xwayland monitors.
    #[cfg(all(feature = "x11", target_os = "linux"))]
    ScreenSources,
    /// Capture a 1280x720 crop for a few seconds and report frame timing.
    #[cfg(all(feature = "x11", target_os = "linux"))]
    ScreenProbe {
        #[arg(long, default_value_t = 0)]
        source: u32,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value_t = 10)]
        fps: u32,
        #[arg(long)]
        display: Option<String>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::AudioDevices => list_devices(),
        Command::AudioLoopback {
            input,
            output,
            latency_ms,
            seconds,
            no_playback,
        } => loopback(input, output, latency_ms, seconds, !no_playback),
        #[cfg(all(feature = "x11", target_os = "linux"))]
        Command::ScreenSources => screen_sources(),
        #[cfg(all(feature = "x11", target_os = "linux"))]
        Command::ScreenProbe {
            source,
            seconds,
            fps,
            display,
        } => screen_probe(source, seconds, fps, display),
    }
}

fn list_devices() -> Result<()> {
    let devices = list_audio_devices().context("could not enumerate audio devices")?;
    if devices.is_empty() {
        println!("no audio devices found");
        return Ok(());
    }

    for device in devices {
        let input = device
            .input_channels
            .zip(device.input_sample_rate)
            .map(|(channels, rate)| format!("{channels}ch {rate}Hz"))
            .unwrap_or_else(|| "-".to_owned());
        let output = device
            .output_channels
            .zip(device.output_sample_rate)
            .map(|(channels, rate)| format!("{channels}ch {rate}Hz"))
            .unwrap_or_else(|| "-".to_owned());
        let mut markers = Vec::new();
        if device.is_default_input {
            markers.push("default-in");
        }
        if device.is_default_output {
            markers.push("default-out");
        }
        println!(
            "{}{}\n  id: {}\n  input:  {input}\n  output: {output}",
            device.name,
            if markers.is_empty() {
                String::new()
            } else {
                format!(" [{}]", markers.join(", "))
            },
            device.id
        );
    }
    Ok(())
}

fn loopback(
    input: Option<String>,
    output: Option<String>,
    latency_ms: u32,
    seconds: u64,
    playback: bool,
) -> Result<()> {
    let config = LoopbackConfig {
        input_device: input,
        output_device: output,
        latency_ms,
        duration: Some(Duration::from_secs(seconds)),
        playback,
    };
    println!("running loopback for {seconds}s at {latency_ms}ms (playback: {playback})");
    let stats =
        run_loopback(&config, Arc::new(AtomicBool::new(false))).context("audio loopback failed")?;
    println!("{stats:#?}");

    let captured = stats.input_samples as f64
        / f64::from(stats.format.channels.max(1))
        / f64::from(stats.format.sample_rate.max(1));
    let expected = stats.elapsed.as_secs_f64();
    println!(
        "captured {:.3}s of audio ({:.3}s expected); dropped {} samples, output underruns {}, stream errors {}",
        captured, expected, stats.input_dropped, stats.output_underruns, stats.stream_errors
    );
    Ok(())
}

#[cfg(all(feature = "x11", target_os = "linux"))]
fn screen_sources() -> Result<()> {
    let sources = list_x11_sources().context("could not list X11 screens")?;
    if sources.is_empty() {
        println!("no X11 screens found");
        return Ok(());
    }
    for source in sources {
        match &source {
            concord_media::CaptureSource::Monitor { id, .. } => {
                println!("{id}: {}", source.display_name());
            }
            concord_media::CaptureSource::Window { .. } => {}
        }
    }
    Ok(())
}

#[cfg(all(feature = "x11", target_os = "linux"))]
fn screen_probe(source_id: u32, seconds: u64, fps: u32, display: Option<String>) -> Result<()> {
    if seconds == 0 {
        anyhow::bail!("--seconds must be greater than zero");
    }
    if fps == 0 || fps > 60 {
        anyhow::bail!("--fps must be between 1 and 60");
    }

    let sources = list_x11_sources().context("could not list X11 screens")?;
    let source = sources
        .into_iter()
        .find(|source| {
            matches!(
                source,
                concord_media::CaptureSource::Monitor { id, .. } if *id == source_id
            )
        })
        .with_context(|| format!("X11 screen {source_id} was not found"))?;
    let name = source.display_name().to_owned();
    let mut capturer = X11Capturer::new(source, PROFILE_720P30, display.as_deref())
        .context("could not open the X11 screen")?;
    let (source_width, source_height) = capturer.screen_size();
    let (width, height) = capturer.capture_size();
    println!(
        "capturing {width}x{height} from {name} (screen {source_width}x{source_height}) at {fps} fps for {seconds}s"
    );

    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let started = Instant::now();
    let mut frames = 0u64;
    let mut total_bytes = 0u64;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut intervals = Vec::new();
    let mut previous = started;

    while started.elapsed() < Duration::from_secs(seconds) {
        let frame = capturer.next_frame().context("capture frame")?;
        frame.validate().context("validate frame")?;
        total_bytes += frame.data.len() as u64;
        for byte in &frame.data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        frames += 1;

        let now = Instant::now();
        intervals.push(now.duration_since(previous));
        previous = now;

        let deadline = started + period.mul_f64(frames as f64);
        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }
    }
    capturer.stop();

    let elapsed = started.elapsed();
    let average_interval = if intervals.is_empty() {
        0.0
    } else {
        intervals.iter().map(Duration::as_secs_f64).sum::<f64>() / intervals.len() as f64
    };
    let max_interval = intervals.iter().copied().max().unwrap_or(Duration::ZERO);
    println!(
        "frames={frames} elapsed={:.3}s fps={:.2} avg_interval={:.2}ms max_interval={:.2}ms throughput={:.1}MiB/s checksum={hash:016x}",
        elapsed.as_secs_f64(),
        frames as f64 / elapsed.as_secs_f64(),
        average_interval * 1_000.0,
        max_interval.as_secs_f64() * 1_000.0,
        total_bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0),
    );
    Ok(())
}
