//! Biquad sections, coefficients from the RBJ Audio EQ Cookbook.
//!
//! Transposed Direct Form II: two state variables per section, best numerical
//! behaviour for f32 in a long-running chain.

/// Tiny DC injected into the input. Without it, IIR state decays into denormal
/// floats when the signal goes quiet, and denormal arithmetic is 10-100x slower
/// on the CPU — a classic cause of audio dropouts that only show up in silence.
const ANTI_DENORMAL: f32 = 1e-15;

#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Pass-through, used to fill unused band slots.
    pub fn identity() -> Self {
        Biquad {
            b0: 1.0,
            ..Default::default()
        }
    }

    /// Peaking EQ: `gain_db` boost/cut centred on `freq_hz`, width set by `q`.
    pub fn peaking(sample_rate: f32, freq_hz: f32, gain_db: f32, q: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (freq_hz / sample_rate).clamp(1e-6, 0.49);
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.01));

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        Biquad::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// Low shelf: `gain_db` applied below `freq_hz`.
    pub fn low_shelf(sample_rate: f32, freq_hz: f32, gain_db: f32, slope: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (freq_hz / sample_rate).clamp(1e-6, 0.49);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / slope.max(0.01) - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Biquad::normalized(b0, b1, b2, a0, a1, a2)
    }

    /// High shelf: `gain_db` applied above `freq_hz`.
    pub fn high_shelf(sample_rate: f32, freq_hz: f32, gain_db: f32, slope: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (freq_hz / sample_rate).clamp(1e-6, 0.49);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / slope.max(0.01) - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Biquad::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        let inv = 1.0 / a0;
        Biquad {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x + ANTI_DENORMAL;
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Swaps in new coefficients while **keeping** z1/z2.
    ///
    /// Retuning a live filter by replacing the whole struct would drop the state
    /// on the floor, which reads as a click every time the user moves a slider.
    pub fn replace_coefficients(&mut self, src: &Biquad) {
        self.b0 = src.b0;
        self.b1 = src.b1;
        self.b2 = src.b2;
        self.a1 = src.a1;
        self.a2 = src.a2;
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peaking filter at unity gain must not change the signal.
    #[test]
    fn unity_gain_is_transparent() {
        let mut f = Biquad::peaking(48000.0, 1000.0, 0.0, 1.0);
        for i in 0..1000 {
            let x = (i as f32 * 0.01).sin();
            let y = f.process(x);
            assert!((y - x).abs() < 1e-3, "sample {i}: {x} -> {y}");
        }
    }

    /// A +6 dB peaking filter at DC-ish should pass low frequencies roughly untouched.
    #[test]
    fn peaking_leaves_far_bands_alone() {
        let mut f = Biquad::peaking(48000.0, 3000.0, 6.0, 1.0);
        // 100 Hz is two decades below the centre; near-unity there.
        let mut max = 0.0f32;
        for i in 0..4800 {
            let x = (2.0 * std::f32::consts::PI * 100.0 * i as f32 / 48000.0).sin();
            let y = f.process(x);
            if i > 1000 {
                max = max.max(y.abs());
            }
        }
        assert!((max - 1.0).abs() < 0.1, "low band gained: {max}");
    }
}
