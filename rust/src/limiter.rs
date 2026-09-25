//! Look-ahead peak limiter.
//!
//! Without look-ahead, any limiter either lets transients through (slow attack)
//! or distorts them (instant attack). The fix is to delay the audio by the same
//! window you use to look ahead, so the gain is already reduced by the time the
//! peak arrives. Two milliseconds is enough to catch a drum hit cleanly.
//!
//! The detector is linked across channels: one gain for all of them, otherwise
//! the stereo image wobbles whenever one side is louder.

/// Cubic-ish soft clipper, unity below 0.5 and asymptotically approaching ±1.
///
/// Chosen over `tanh` because it is branch-simple and has no transcendentals, so
/// it stays cheap on the audio thread. It is C1-continuous at the knee, which
/// matters: a slope discontinuity there is audible as a buzz on transients.
#[inline]
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.5 {
        x
    } else {
        let y = 1.0 - 0.25 / a;
        if x < 0.0 {
            -y
        } else {
            y
        }
    }
}

pub struct Limiter {
    channels: usize,
    lookahead: usize,
    /// Delay-line length, one longer than `lookahead` so that the sample being
    /// emitted is exactly `lookahead` frames behind the one just written. With
    /// exactly `lookahead` slots the oldest available sample is one frame short,
    /// and the latency the limiter reports would not be the latency it applies.
    slots: usize,
    /// Ring buffer, `channels * slots` samples, channel-major.
    delay: Vec<f32>,
    write: usize,
    threshold: f32,
    release_coef: f32,
    gain: f32,
}

impl Limiter {
    /// `threshold_db` is the ceiling; `lookahead_ms` trades latency for transparency.
    pub fn new(sample_rate: f32, channels: usize, threshold_db: f32, lookahead_ms: f32) -> Self {
        let lookahead = ((sample_rate * lookahead_ms / 1000.0) as usize).max(1);
        let slots = lookahead + 1;
        let release_ms = 80.0;
        Limiter {
            channels,
            lookahead,
            slots,
            delay: vec![0.0; channels * slots],
            write: 0,
            threshold: 10f32.powf(threshold_db / 20.0),
            // One-pole coefficient. exp(-1/(t*fs)) is the time to fall to 1/e,
            // so this releases ~63% of the way over `release_ms`.
            release_coef: (-1.0 / (release_ms / 1000.0 * sample_rate)).exp(),
            gain: 1.0,
        }
    }

    /// Latency this limiter adds, in frames. Callers should know.
    pub fn latency_frames(&self) -> usize {
        self.lookahead
    }

    pub fn set_threshold_db(&mut self, db: f32) {
        self.threshold = 10f32.powf(db / 20.0);
    }

    /// Processes one interleaved frame in place. `frame.len()` must equal `channels`.
    #[inline]
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        let w = self.write;
        for (c, s) in frame.iter().enumerate().take(self.channels) {
            self.delay[c * self.slots + w] = *s;
        }
        self.write = if w + 1 == self.slots { 0 } else { w + 1 };

        // Peak across the whole window and all channels. O(channels * lookahead)
        // per sample; at 2 channels and 2 ms this is ~10 M ops/sec, noise on a
        // modern core, and it buys a correct sliding maximum for free because
        // the delay line already holds exactly the window we need.
        let mut peak = 0.0f32;
        for v in self.delay.iter() {
            let a = v.abs();
            if a > peak {
                peak = a;
            }
        }

        let target = if peak > self.threshold {
            self.threshold / peak
        } else {
            1.0
        };

        // Instant attack (the look-ahead already covers the transient), smooth
        // release (so the gain does not pump audibly between hits).
        self.gain = if target < self.gain {
            target
        } else {
            target + self.release_coef * (self.gain - target)
        };

        // After the increment above, `self.write` points at the oldest sample,
        // which is exactly `lookahead` frames behind the one just written.
        let r = self.write;
        for (c, s) in frame.iter_mut().enumerate().take(self.channels) {
            *s = soft_clip(self.delay[c * self.slots + r] * self.gain);
        }
    }

    /// Current gain reduction in dB, for metering.
    pub fn reduction_db(&self) -> f32 {
        20.0 * self.gain.max(1e-6).log10()
    }

    pub fn reset(&mut self) {
        self.delay.iter_mut().for_each(|v| *v = 0.0);
        self.write = 0;
        self.gain = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing below the threshold should be touched once the look-ahead has
    /// flushed — the limiter must be transparent when it is not working.
    ///
    /// The comparison has to be against the *delayed* input: the limiter is
    /// supposed to shift the signal by its look-ahead, so comparing output[i] to
    /// input[i] would just measure that delay.
    #[test]
    fn quiet_signal_passes_through() {
        let mut lim = Limiter::new(48000.0, 2, -1.0, 2.0);
        let la = lim.latency_frames();
        let n = 4800;
        let input: Vec<f32> = (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin())
            .collect();

        let mut worst: f32 = 0.0;
        for (i, &x) in input.iter().enumerate() {
            let mut frame = [x, x];
            lim.process_frame(&mut frame);
            if i >= la {
                worst = worst.max((frame[0] - input[i - la]).abs());
            }
        }
        assert!(worst < 1e-3, "quiet signal was altered by {worst}");
    }

    /// A hard step above the threshold must never come out above unity.
    #[test]
    fn nothing_escapes_above_unity() {
        let mut lim = Limiter::new(48000.0, 2, -1.0, 2.0);
        let mut worst: f32 = 0.0;
        for i in 0..48000 {
            // A full-scale square wave: the worst case a limiter will ever see.
            let x = if (i / 100) % 2 == 0 { 1.0 } else { -1.0 };
            let mut frame = [x, x];
            lim.process_frame(&mut frame);
            worst = worst.max(frame[0].abs());
        }
        assert!(worst <= 1.0, "peak escaped at {worst}");
    }

    /// The limiter must actually reduce gain on sustained loud material.
    #[test]
    fn loud_signal_is_reduced() {
        let mut lim = Limiter::new(48000.0, 2, -12.0, 2.0);
        for _ in 0..48000 {
            let mut frame = [1.0f32, 1.0];
            lim.process_frame(&mut frame);
        }
        // -12 dBFS ceiling means gain should settle near 0.25.
        assert!(lim.gain < 0.3, "gain did not settle: {}", lim.gain);
    }
}
