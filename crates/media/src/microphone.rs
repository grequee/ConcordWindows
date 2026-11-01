//! Local microphone processing, after 48 kHz mono resampling and before Opus.
//! No allocations occur while processing a frame. SpeexDSP is optional at
//! build time; unavailable suppression is reported rather than silently faked.

use serde::{Deserialize, Serialize};

use crate::{MediaError, OPUS_FRAME_SAMPLES};

pub const NOISE_SUPPRESSION_AVAILABLE: bool = cfg!(concord_speexdsp);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MicrophoneSettings {
    pub volume_percent: u16,
    pub noise_suppression: bool,
}

impl Default for MicrophoneSettings {
    fn default() -> Self {
        Self {
            volume_percent: 100,
            noise_suppression: false,
        }
    }
}

impl MicrophoneSettings {
    pub fn validate(self) -> Result<Self, MediaError> {
        if self.volume_percent > 200 {
            return Err(MediaError::InvalidSettings(
                "o volume do microfone deve ficar entre 0% e 200%",
            ));
        }
        if self.noise_suppression && !NOISE_SUPPRESSION_AVAILABLE {
            return Err(MediaError::Audio(
                "supressão de ruído indisponível nesta compilação".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MicrophoneStats {
    /// RMS after processing, normalized to 0..1. This is metadata, not audio.
    pub level: f32,
    pub clipped: bool,
}

pub struct MicrophoneProcessor {
    gain: f32,
    #[cfg(concord_speexdsp)]
    denoiser: speex::Denoiser,
}

impl MicrophoneProcessor {
    pub fn new() -> Result<Self, MediaError> {
        Ok(Self {
            gain: 1.0,
            #[cfg(concord_speexdsp)]
            denoiser: speex::Denoiser::new()?,
        })
    }

    /// Caller supplies validated local settings. Gain changes ramp over one
    /// 20 ms frame to avoid clicks; 0% always produces exact silence immediately.
    pub fn process(
        &mut self,
        frame: &mut [f32; OPUS_FRAME_SAMPLES],
        settings: MicrophoneSettings,
    ) -> MicrophoneStats {
        for sample in frame.iter_mut() {
            if !sample.is_finite() {
                *sample = 0.0;
            }
        }
        #[cfg(concord_speexdsp)]
        if settings.noise_suppression {
            self.denoiser.process(frame);
        }
        let target = f32::from(settings.volume_percent.min(200)) / 100.0;
        if target == 0.0 {
            self.gain = 0.0;
            frame.fill(0.0);
            return MicrophoneStats::default();
        }
        let step = (target - self.gain) / frame.len() as f32;
        let mut energy = 0.0f32;
        let mut clipped = false;
        for sample in frame.iter_mut() {
            self.gain += step;
            let amplified = *sample * self.gain;
            clipped |= amplified.abs() > 1.0;
            *sample = amplified.clamp(-1.0, 1.0);
            energy += *sample * *sample;
        }
        self.gain = target;
        MicrophoneStats {
            level: (energy / frame.len() as f32).sqrt(),
            clipped,
        }
    }
}

#[cfg(concord_speexdsp)]
#[allow(unsafe_code)]
mod speex {
    use super::*;
    use std::ffi::{c_int, c_void};
    use std::ptr::NonNull;

    unsafe extern "C" {
        fn speex_preprocess_state_init(frame_size: c_int, sampling_rate: c_int) -> *mut c_void;
        fn speex_preprocess_state_destroy(state: *mut c_void);
        fn speex_preprocess_ctl(state: *mut c_void, request: c_int, value: *mut c_void) -> c_int;
        fn speex_preprocess_run(state: *mut c_void, samples: *mut i16) -> c_int;
    }

    pub(super) struct Denoiser {
        state: NonNull<c_void>,
        samples: [i16; OPUS_FRAME_SAMPLES],
    }

    // SAFETY: each preprocessor is exclusively owned, has no thread affinity,
    // and is accessed only through &mut self. It is never shared concurrently.
    unsafe impl Send for Denoiser {}

    impl Denoiser {
        pub(super) fn new() -> Result<Self, MediaError> {
            // SAFETY: 960 samples = 20 ms of one-channel 48 kHz PCM, matching
            // the fixed-size array passed to every subsequent preprocess call.
            let state = NonNull::new(unsafe {
                speex_preprocess_state_init(
                    OPUS_FRAME_SAMPLES as c_int,
                    crate::OPUS_SAMPLE_RATE as c_int,
                )
            })
            .ok_or_else(|| {
                MediaError::Audio("não foi possível iniciar a supressão de ruído".to_owned())
            })?;
            let denoiser = Self {
                state,
                samples: [0; OPUS_FRAME_SAMPLES],
            };
            // Speex preprocess controls, from speex_preprocess.h. Only noise
            // suppression is enabled: AGC, VAD and dereverb remain disabled.
            for (request, mut value) in [(0, 1), (2, 0), (8, 0), (18, -20)] {
                // SAFETY: state is live, requests take an int pointer, and the
                // stack value remains valid throughout this synchronous call.
                let result = unsafe {
                    speex_preprocess_ctl(state.as_ptr(), request, (&mut value as *mut c_int).cast())
                };
                if result != 0 {
                    return Err(MediaError::Audio(
                        "não foi possível configurar a supressão de ruído".to_owned(),
                    ));
                }
            }
            Ok(denoiser)
        }

        pub(super) fn process(&mut self, frame: &mut [f32; OPUS_FRAME_SAMPLES]) {
            for (output, input) in self.samples.iter_mut().zip(frame.iter()) {
                *output = (input.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
            // SAFETY: exactly the frame length specified at construction; the
            // buffer is exclusively borrowed and libspeex retains no pointer.
            unsafe {
                speex_preprocess_run(self.state.as_ptr(), self.samples.as_mut_ptr());
            }
            for (output, input) in frame.iter_mut().zip(self.samples.iter()) {
                *output = f32::from(*input) / 32768.0;
            }
        }
    }

    impl Drop for Denoiser {
        fn drop(&mut self) {
            // SAFETY: the non-null state is owned and destroyed exactly once.
            unsafe {
                speex_preprocess_state_destroy(self.state.as_ptr());
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn gain_scales_and_zero_volume_is_immediate_silence() {
        let mut processor = MicrophoneProcessor::new().expect("processor");
        let half = MicrophoneSettings {
            volume_percent: 50,
            ..Default::default()
        };
        processor.process(&mut [0.2; OPUS_FRAME_SAMPLES], half);
        let mut frame = [0.2; OPUS_FRAME_SAMPLES];
        processor.process(&mut frame, half);
        assert!(frame.iter().all(|sample| (*sample - 0.1).abs() < 1e-5));
        let stats = processor.process(
            &mut frame,
            MicrophoneSettings {
                volume_percent: 0,
                ..half
            },
        );
        assert!(frame.iter().all(|sample| *sample == 0.0));
        assert_eq!(stats.level, 0.0);
    }

    #[test]
    fn gain_limits_output_and_sanitizes_nonfinite_input() {
        let mut processor = MicrophoneProcessor::new().expect("processor");
        let settings = MicrophoneSettings {
            volume_percent: 200,
            ..Default::default()
        };
        let mut frame = [0.8; OPUS_FRAME_SAMPLES];
        frame[0] = f32::NAN;
        frame[1] = f32::INFINITY;
        let stats = processor.process(&mut frame, settings);
        assert!(stats.clipped);
        assert!(
            frame
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() <= 1.0)
        );
        assert!(
            MicrophoneSettings {
                volume_percent: 201,
                ..settings
            }
            .validate()
            .is_err()
        );
    }

    #[cfg(concord_speexdsp)]
    #[test]
    fn denoiser_preserves_a_voice_like_burst() {
        let mut processor = MicrophoneProcessor::new().expect("SpeexDSP");
        let settings = MicrophoneSettings {
            noise_suppression: true,
            ..Default::default()
        };
        for _ in 0..100 {
            processor.process(&mut [0.0; OPUS_FRAME_SAMPLES], settings);
        }
        let mut before = 0.0f64;
        let mut after = 0.0f64;
        for block in 0..30 {
            let mut frame = [0.0; OPUS_FRAME_SAMPLES];
            for (index, sample) in frame.iter_mut().enumerate() {
                let time = (block * OPUS_FRAME_SAMPLES + index) as f32 / 48000.0;
                let phase = 2.0 * std::f32::consts::PI * (180.0 * time + 20.0 * time * time);
                *sample = 0.18 * phase.sin() + 0.06 * (2.0 * phase).sin();
            }
            before += frame.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            processor.process(&mut frame, settings);
            after += frame.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
        }
        assert!(
            after > before * 0.2,
            "speech-like energy: before={before} after={after}"
        );
    }

    #[cfg(concord_speexdsp)]
    #[test]
    fn denoiser_reduces_stationary_noise_without_a_device() {
        let mut processor = MicrophoneProcessor::new().expect("SpeexDSP");
        let settings = MicrophoneSettings {
            noise_suppression: true,
            ..Default::default()
        };
        let mut seed = 42u32;
        let mut before = 0.0f64;
        let mut after = 0.0f64;
        for block in 0..250 {
            let mut frame = [0.0; OPUS_FRAME_SAMPLES];
            for sample in &mut frame {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *sample = ((seed >> 8) as f32 / 16777216.0 - 0.5) * 0.03;
            }
            if block >= 150 {
                before += frame.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            }
            processor.process(&mut frame, settings);
            if block >= 150 {
                after += frame.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
            }
        }
        assert!(
            after < before * 0.5,
            "noise energy: before={before} after={after}"
        );
    }
}
