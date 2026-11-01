//! Local audio activity and smooth per-listener gain; never sent to signaling.
use crate::OPUS_FRAME_SAMPLES;

#[derive(Debug, Default)]
pub struct VoiceActivity {
    pub level: f32,
    pub speaking: bool,
    hold: u8,
}
impl VoiceActivity {
    pub fn update(&mut self, level: f32, received: bool) {
        let level = if received && level.is_finite() {
            level.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.level = level.max(self.level * 0.7);
        let threshold = if self.speaking { 0.009 } else { 0.015 };
        if level >= threshold {
            self.hold = 13;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        self.speaking = self.hold > 0;
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame
        .iter()
        .map(|v| if v.is_finite() { v * v } else { 0.0 })
        .sum::<f32>()
        / frame.len() as f32)
        .sqrt()
}

pub fn mix_participant(
    mixed: &mut [f32; OPUS_FRAME_SAMPLES],
    frame: &[f32],
    gain: &mut f32,
    volume: u16,
) {
    let target = f32::from(volume.min(200)) / 100.0;
    if target == 0.0 {
        *gain = 0.0;
        return;
    }
    let step = (target - *gain) / mixed.len() as f32;
    for (output, sample) in mixed.iter_mut().zip(frame) {
        *gain += step;
        if sample.is_finite() {
            *output += sample * *gain;
        }
    }
    *gain = target;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_holds_brief_gaps_but_never_treats_concealment_as_new_speech() {
        let mut activity = VoiceActivity::default();
        activity.update(0.1, true);
        assert!(activity.speaking);
        for _ in 0..12 {
            activity.update(0.5, false);
        }
        assert!(activity.speaking);
        activity.update(0.5, false);
        assert!(!activity.speaking);
        activity.update(f32::NAN, true);
        assert!(!activity.speaking);
    }
    #[test]
    fn participant_volume_is_local_ramped_and_zero_is_immediate() {
        let frame = [0.2; OPUS_FRAME_SAMPLES];
        let mut mixed = [0.0; OPUS_FRAME_SAMPLES];
        let mut gain = 1.0;
        mix_participant(&mut mixed, &frame, &mut gain, 200);
        assert!(mixed[0] > 0.2 && mixed[0] < 0.201);
        assert!((mixed[OPUS_FRAME_SAMPLES - 1] - 0.4).abs() < 0.0001);
        mixed.fill(0.0);
        mix_participant(&mut mixed, &frame, &mut gain, 0);
        assert!(mixed.iter().all(|v| *v == 0.0));
        assert_eq!(gain, 0.0);
    }
}
