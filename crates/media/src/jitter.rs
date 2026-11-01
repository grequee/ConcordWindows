//! Per-peer Opus jitter buffer.
//!
//! Each remote participant has a decoder and a small queue of decoded 20 ms
//! frames. The call actor consumes one frame per tick; an empty queue is
//! concealed with Opus PLC, which produces comfort noise/decay instead of a
//! hard click. The queue is deliberately short: losing an older frame is
//! preferable to letting end-to-end latency grow without bound.

use std::collections::VecDeque;

use crate::{MediaError, OPUS_FRAME_SAMPLES, VoiceDecoder};

const DEFAULT_MAX_FRAMES: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub packets_late: u64,
    pub packets_duplicate: u64,
    pub frames_dropped: u64,
    pub frames_concealed: u64,
}

pub struct JitterBuffer {
    decoder: VoiceDecoder,
    frames: VecDeque<Vec<f32>>,
    last_sequence: Option<u16>,
    max_frames: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new() -> Result<Self, MediaError> {
        Self::with_capacity(DEFAULT_MAX_FRAMES)
    }

    pub fn with_capacity(max_frames: usize) -> Result<Self, MediaError> {
        if max_frames == 0 {
            return Err(MediaError::InvalidSettings(
                "the jitter buffer needs at least one frame",
            ));
        }
        Ok(Self {
            decoder: VoiceDecoder::new()?,
            frames: VecDeque::with_capacity(max_frames),
            last_sequence: None,
            max_frames,
            stats: JitterStats::default(),
        })
    }

    /// Decode and enqueue an RTP audio packet. Sequence gaps are counted; the
    /// missing interval is concealed at playback time so a late packet does
    /// not shift every following frame.
    pub fn push_packet(&mut self, sequence: u16, payload: &[u8]) -> Result<(), MediaError> {
        if let Some(last) = self.last_sequence {
            let delta = sequence.wrapping_sub(last);
            if delta == 0 {
                self.stats.packets_duplicate = self.stats.packets_duplicate.saturating_add(1);
                return Ok(());
            }
            if delta >= 0x8000 {
                self.stats.packets_late = self.stats.packets_late.saturating_add(1);
                return Ok(());
            }
            if delta > 1 {
                self.stats.packets_lost =
                    self.stats.packets_lost.saturating_add(u64::from(delta - 1));
            }
        }

        let frame = self.decoder.decode(payload)?;
        if self.frames.len() >= self.max_frames {
            self.frames.pop_front();
            self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
        }
        self.frames.push_back(frame);
        self.last_sequence = Some(sequence);
        self.stats.packets_received = self.stats.packets_received.saturating_add(1);
        Ok(())
    }

    /// Pop the next frame, or synthesize one with Opus packet-loss concealment
    /// when the network has not delivered a packet for this tick.
    pub fn next_frame(&mut self) -> Result<Vec<f32>, MediaError> {
        if let Some(frame) = self.frames.pop_front() {
            return Ok(frame);
        }
        self.stats.frames_concealed = self.stats.frames_concealed.saturating_add(1);
        let frame = self.decoder.conceal()?;
        debug_assert_eq!(frame.len(), OPUS_FRAME_SAMPLES);
        Ok(frame)
    }

    pub fn queued_frames(&self) -> usize {
        self.frames.len()
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{DEFAULT_OPUS_BITRATE_BPS, VoiceEncoder};

    fn packet(encoder: &mut VoiceEncoder) -> Vec<u8> {
        let pcm: Vec<f32> = (0..OPUS_FRAME_SAMPLES)
            .map(|index| {
                (index as f32 / OPUS_FRAME_SAMPLES as f32 * std::f32::consts::TAU * 4.0).sin() * 0.2
            })
            .collect();
        encoder.encode(&pcm).expect("encode")
    }

    #[test]
    fn decoded_frames_arrive_in_order() {
        let mut encoder = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder");
        let mut jitter = JitterBuffer::new().expect("jitter");
        for sequence in 10..15 {
            jitter
                .push_packet(sequence, &packet(&mut encoder))
                .expect("push");
        }
        assert_eq!(jitter.queued_frames(), 4);
        assert_eq!(jitter.stats().frames_dropped, 1);
        let frame = jitter.next_frame().expect("frame");
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES);
    }

    #[test]
    fn gaps_and_duplicates_are_counted() {
        let mut encoder = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder");
        let mut jitter = JitterBuffer::new().expect("jitter");
        let first = packet(&mut encoder);
        jitter.push_packet(100, &first).expect("first");
        jitter.push_packet(100, &first).expect("duplicate");
        jitter.push_packet(102, &first).expect("gap");
        let stats = jitter.stats();
        assert_eq!(stats.packets_received, 2);
        assert_eq!(stats.packets_duplicate, 1);
        assert_eq!(stats.packets_lost, 1);
    }

    #[test]
    fn an_empty_buffer_conceals_a_full_frame() {
        let mut jitter = JitterBuffer::new().expect("jitter");
        let frame = jitter.next_frame().expect("plc");
        assert_eq!(frame.len(), OPUS_FRAME_SAMPLES);
        assert_eq!(jitter.stats().frames_concealed, 1);
    }
}
