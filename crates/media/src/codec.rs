//! Opus codec wrapper used by the voice path.
//!
//! The rest of the application exchanges *encoded* Opus packets. Keeping the
//! libopus calls here means the RTC and app layers never depend on FFI or on
//! the sample type expected by libopus.

use opus::{Application, Channels};

use crate::{MediaError, OPUS_FRAME_SAMPLES, OPUS_SAMPLE_RATE};

/// Largest Opus packet for a 20 ms frame. RFC 6716 allows up to 1275 bytes.
pub const MAX_OPUS_PACKET_BYTES: usize = 1_275;
/// Opus can decode up to 120 ms per packet (5760 samples at 48 kHz).
const MAX_DECODE_SAMPLES: usize = 5_760;
pub const DEFAULT_OPUS_BITRATE_BPS: u32 = 32_000;

#[derive(Debug)]
pub struct VoiceEncoder {
    inner: opus::Encoder,
    buffer: [u8; MAX_OPUS_PACKET_BYTES],
}

impl VoiceEncoder {
    pub fn new(bitrate_bps: u32) -> Result<Self, MediaError> {
        if !(6_000..=510_000).contains(&bitrate_bps) {
            return Err(MediaError::InvalidSettings(
                "Opus bitrate must be between 6000 and 510000 bps",
            ));
        }

        let mut inner = opus::Encoder::new(OPUS_SAMPLE_RATE, Channels::Mono, Application::Voip)
            .map_err(codec_error)?;
        inner
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(codec_error)?;
        inner.set_complexity(5).map_err(codec_error)?;
        inner.set_signal(opus::Signal::Voice).map_err(codec_error)?;
        inner.set_inband_fec(true).map_err(codec_error)?;
        // The receiver can conceal gaps with Opus PLC. Letting the encoder know
        // that loss is possible enables the low-bitrate FEC/LBRR trade-off.
        inner.set_packet_loss_perc(10).map_err(codec_error)?;
        // Discontinuous transmission keeps an idle microphone from consuming
        // bandwidth; the UI still reports mute independently.
        inner.set_dtx(true).map_err(codec_error)?;

        Ok(Self {
            inner,
            buffer: [0; MAX_OPUS_PACKET_BYTES],
        })
    }

    /// Encode exactly one 20 ms mono frame at 48 kHz.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, MediaError> {
        if pcm.len() != OPUS_FRAME_SAMPLES {
            return Err(MediaError::InvalidSettings(
                "Opus voice frames must contain exactly 960 mono samples",
            ));
        }
        let len = self
            .inner
            .encode_float(pcm, &mut self.buffer)
            .map_err(codec_error)?;
        Ok(self.buffer[..len].to_vec())
    }

    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), MediaError> {
        self.inner
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(codec_error)
    }
}

#[derive(Debug)]
pub struct VoiceDecoder {
    inner: opus::Decoder,
    samples: Vec<f32>,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self, MediaError> {
        let inner = opus::Decoder::new(OPUS_SAMPLE_RATE, Channels::Mono).map_err(codec_error)?;
        Ok(Self {
            inner,
            samples: vec![0.0; MAX_DECODE_SAMPLES],
        })
    }

    /// Decode one Opus packet and return mono 48 kHz PCM.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, MediaError> {
        if packet.is_empty() {
            return Err(MediaError::InvalidSettings("empty Opus packet"));
        }
        self.decode_inner(packet, false, "invalid Opus packet")
    }

    /// Generate one 20 ms concealment frame (Opus PLC) for a lost packet.
    pub fn conceal(&mut self) -> Result<Vec<f32>, MediaError> {
        self.decode_inner(&[], false, "Opus concealment failed")
    }

    /// Decode using the in-band FEC copy of the next packet, when available.
    pub fn decode_fec(&mut self, packet: &[u8]) -> Result<Vec<f32>, MediaError> {
        self.decode_inner(packet, true, "invalid Opus FEC packet")
    }

    fn decode_inner(
        &mut self,
        packet: &[u8],
        fec: bool,
        context: &'static str,
    ) -> Result<Vec<f32>, MediaError> {
        let count = self
            .inner
            .decode_float(packet, &mut self.samples, fec)
            .map_err(|error| MediaError::Codec(format!("{context}: {error}")))?;
        if count > self.samples.len() {
            return Err(MediaError::Codec(
                "Opus decoder returned more samples than its buffer".to_owned(),
            ));
        }
        Ok(truncate_to_20ms(&self.samples[..count]))
    }
}

fn truncate_to_20ms(samples: &[f32]) -> Vec<f32> {
    if samples.len() <= OPUS_FRAME_SAMPLES {
        let mut frame = vec![0.0; OPUS_FRAME_SAMPLES];
        frame[..samples.len()].copy_from_slice(samples);
        frame
    } else {
        samples[..OPUS_FRAME_SAMPLES].to_vec()
    }
}

fn codec_error(error: opus::Error) -> MediaError {
    MediaError::Codec(error.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn sine(frequency: f32, phase: &mut f32) -> Vec<f32> {
        let mut frame = Vec::with_capacity(OPUS_FRAME_SAMPLES);
        for _ in 0..OPUS_FRAME_SAMPLES {
            frame.push((*phase * std::f32::consts::TAU).sin() * 0.2);
            *phase = (*phase + frequency / OPUS_SAMPLE_RATE as f32).fract();
        }
        frame
    }

    #[test]
    fn opus_round_trip_preserves_a_tone() {
        let mut encoder = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder");
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let mut phase = 0.0;
        let input = sine(440.0, &mut phase);
        let packet = encoder.encode(&input).expect("encode");
        assert!(!packet.is_empty());
        assert!(packet.len() <= MAX_OPUS_PACKET_BYTES);

        let output = decoder.decode(&packet).expect("decode");
        assert_eq!(output.len(), OPUS_FRAME_SAMPLES);
        let energy: f32 = output.iter().map(|sample| sample * sample).sum();
        assert!(energy > 0.05, "decoded tone should not be silent: {energy}");
    }

    #[test]
    fn encoder_rejects_the_wrong_frame_size() {
        let mut encoder = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder");
        assert!(encoder.encode(&[0.0; 10]).is_err());
    }

    #[test]
    fn concealment_produces_a_full_frame() {
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let frame = decoder.conceal().expect("plc");
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES);
    }
}
