//! EQ curves, defined next to the DSP so they can be measured rather than
//! asserted.
//!
//! These used to live in Kotlin, where the only way to find out what a curve
//! actually did was to put headphones on and guess. Living here means a test can
//! sweep a tone through the real engine and check the response against the claim.
//!
//! ## What a static EQ can and cannot do
//!
//! It cannot separate a voice from the backing track — they occupy the same
//! frequency range, and a filter has no idea which signal is which. All a static
//! curve can do is change the overall spectral tilt. "Vocals forward" in this
//! file therefore means *mid-forward*: less energy in the bass register, more in
//! the 1-4 kHz band where consonants live. Making one specific element stand out
//! against another needs a dynamic tool (multiband compression or a dynamic EQ),
//! which this engine does not have yet.

use crate::engine::{Params, KIND_HIGH_SHELF, KIND_LOW_SHELF, KIND_PEAKING};

/// One EQ band: `(kind, freq_hz, gain_db, q_or_slope)`.
pub type Band = (u32, f32, f32, f32);

pub struct Preset {
    pub bands: &'static [Band],
    pub output_gain_db: f32,
    pub threshold_db: f32,
}

/// Slot count on the audio side. Every preset must stay within it.
pub const MAX_BANDS: usize = 10;

pub const PRESETS: &[Preset] = &[
    // 0 — control. No bands and the caller also disables the engine outright, so
    // an A/B against this is a comparison against nothing at all.
    Preset {
        bands: &[],
        output_gain_db: 0.0,
        threshold_db: -1.0,
    },
    // 1 — mid-forward ("vocals forward").
    //
    // Deliberately NOT a big bass cut. An earlier version shelved 200 Hz down by
    // 7 dB, which made the whole mix thin rather than the vocal prominent, and
    // read to the ear as "airy" instead of "clear". Keeping the body and lifting
    // the 1-4 kHz region is what actually moves the perceived balance.
    Preset {
        bands: &[
            (KIND_LOW_SHELF, 150.0, -3.0, 0.7),
            (KIND_PEAKING, 320.0, -3.0, 1.0),
            (KIND_PEAKING, 1600.0, 4.0, 0.8),
            (KIND_PEAKING, 3200.0, 4.0, 0.9),
            (KIND_HIGH_SHELF, 9000.0, 2.0, 0.7),
        ],
        output_gain_db: 0.0,
        threshold_db: -1.0,
    },
    // 2 — near-inverse of preset 1: scoop the vocal band, lift both extremes.
    // This one is confirmed by ear to be obviously different from bypass, which
    // makes it the reference that proves the chain is live.
    Preset {
        bands: &[
            (KIND_LOW_SHELF, 120.0, 7.0, 0.7),
            (KIND_PEAKING, 1200.0, -7.0, 0.9),
            (KIND_PEAKING, 3500.0, 2.0, 1.0),
            (KIND_HIGH_SHELF, 9000.0, 6.0, 0.7),
        ],
        output_gain_db: 0.0,
        threshold_db: -1.0,
    },
    // 3 — gentle balanced improvement.
    Preset {
        bands: &[
            (KIND_LOW_SHELF, 130.0, 3.0, 0.7),
            (KIND_PEAKING, 400.0, -2.0, 1.2),
            (KIND_PEAKING, 2500.0, 2.0, 1.0),
            (KIND_HIGH_SHELF, 8000.0, 2.0, 0.7),
        ],
        output_gain_db: 0.0,
        threshold_db: -1.0,
    },
    // 4 — preset 3 pushed. Should be the first one to make the limiter work,
    // if the source material has peaks near full scale. On quiet material the
    // gain-reduction readout will stay at 0 and that is correct, not a bug.
    Preset {
        bands: &[
            (KIND_LOW_SHELF, 130.0, 7.0, 0.7),
            (KIND_PEAKING, 400.0, -5.0, 1.0),
            (KIND_PEAKING, 2500.0, 5.0, 1.0),
            (KIND_HIGH_SHELF, 8000.0, 5.0, 0.7),
        ],
        output_gain_db: 0.0,
        threshold_db: -1.0,
    },
];

pub fn preset_count() -> usize {
    PRESETS.len()
}

/// Writes a preset into the shared parameter block. Safe to call from any thread.
pub fn apply(params: &Params, index: usize) {
    let Some(preset) = PRESETS.get(index) else {
        return;
    };
    for (i, &(kind, freq, gain, q)) in preset.bands.iter().enumerate() {
        params.set_band(i, kind, freq, gain, q);
    }
    // Slots the preset does not use have to be flattened explicitly, otherwise
    // the tail of whatever preset ran before keeps filtering.
    for i in preset.bands.len()..MAX_BANDS {
        params.set_band(i, KIND_PEAKING, 1000.0, 0.0, 1.0);
    }
    params.set_output_gain_db(preset.output_gain_db);
    params.set_threshold_db(preset.threshold_db);
    // Preset 0 is a true bypass: no EQ, and the limiter's latency drops out too.
    params.set_enabled(index != 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    const SR: f32 = 48000.0;
    const AMP: f32 = 0.2;

    /// Steady-state gain of a live engine at `freq`, in dB.
    ///
    /// AMP is low enough that even the loudest curve stays under the -1 dBFS
    /// limiter ceiling, so this measures the EQ and nothing else.
    fn gain_of(e: &mut Engine, freq: f32) -> f32 {
        let n = 48_000usize;
        let mut buf = vec![0i16; n * 2];
        for i in 0..n {
            let x = AMP * (2.0 * std::f32::consts::PI * freq * i as f32 / SR).sin();
            let v = (x * 32767.0) as i16;
            buf[i * 2] = v;
            buf[i * 2 + 1] = v;
        }
        e.process_i16(&mut buf);

        // Second half only: the filters need time to reach steady state.
        let tail = &buf[(n / 2) * 2..];
        let sum: f64 = tail.iter().map(|v| (*v as f64) * (*v as f64)).sum();
        let out_rms = (sum / tail.len() as f64).sqrt();
        let in_rms = (AMP as f64 / 2f64.sqrt()) * 32767.0;

        (20.0 * (out_rms / in_rms).log10()) as f32
    }

    fn gain_db(index: usize, freq: f32) -> f32 {
        let mut e = Engine::new(SR, 2);
        apply(&e.params, index);
        gain_of(&mut e, freq)
    }

    fn assert_near(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1.5,
            "{what}: expected ~{expected:.1} dB, measured {actual:.1} dB"
        );
    }

    /// Not an assertion — a measurement you can read.
    ///
    /// `cargo test --manifest-path rust/Cargo.toml print_response_table -- --nocapture`
    #[test]
    fn print_response_table() {
        const FREQS: [f32; 11] = [60.0, 120.0, 250.0, 500.0, 1000.0, 1600.0, 2500.0, 3200.0, 6000.0, 10000.0, 14000.0];

        print!("\n{:>9}", "Hz");
        for i in 0..PRESETS.len() {
            print!("{:>10}", format!("preset{i}"));
        }
        println!();
        for f in FREQS {
            print!("{f:>9.0}");
            for i in 0..PRESETS.len() {
                print!("{:>9.1}dB", gain_db(i, f));
            }
            println!();
        }
        println!();
    }

    #[test]
    fn every_preset_stays_within_the_band_budget() {
        for (i, p) in PRESETS.iter().enumerate() {
            assert!(
                p.bands.len() <= MAX_BANDS,
                "preset {i} uses {} bands, limit is {MAX_BANDS}",
                p.bands.len()
            );
        }
    }

    #[test]
    fn bypass_is_flat() {
        for f in [80.0, 400.0, 1200.0, 3000.0, 10000.0] {
            assert_near(gain_db(0, f), 0.0, &format!("bypass at {f} Hz"));
        }
    }

    /// The claim on preset 1 is "mid-forward", so: less bass, more presence,
    /// and no thinning out of the top.
    #[test]
    fn vocals_forward_lifts_the_mid_and_trims_the_bass() {
        let bass = gain_db(1, 80.0);
        let presence = gain_db(1, 2500.0);
        let air = gain_db(1, 12000.0);

        assert!(bass < -1.0, "bass should be cut, measured {bass:.1} dB");
        assert!(
            presence > 3.0,
            "presence should be clearly lifted, measured {presence:.1} dB"
        );
        // The previous version shelved the top down, which is what made it sound
        // thin rather than clear.
        assert!(air > -0.5, "the top must not be dulled, measured {air:.1} dB");
        assert!(
            presence - bass > 5.0,
            "the mid/bass tilt is the whole point: {presence:.1} vs {bass:.1} dB"
        );
    }

    /// Preset 2 should be close to the opposite of preset 1.
    #[test]
    fn backing_forward_is_the_inverse_of_vocals_forward() {
        let bass = gain_db(2, 80.0);
        let vocal_band = gain_db(2, 1200.0);
        let air = gain_db(2, 12000.0);

        assert!(bass > 3.0, "bass should be lifted, measured {bass:.1} dB");
        assert!(vocal_band < -3.0, "the vocal band should be scooped, measured {vocal_band:.1} dB");
        assert!(air > 3.0, "air should be lifted, measured {air:.1} dB");

        // The point of keeping both is that they are audibly different, so the
        // gap between them has to be large at the vocal band.
        let gap = gain_db(1, 1200.0) - gain_db(2, 1200.0);
        assert!(gap > 6.0, "presets 1 and 2 are too similar at 1.2 kHz: {gap:.1} dB apart");
    }

    /// Preset 3 is preset 4 at a third of the amount; the ordering must hold.
    #[test]
    fn strong_is_stronger_than_mild() {
        for f in [130.0, 2500.0, 8000.0] {
            let mild = gain_db(3, f);
            let strong = gain_db(4, f);
            assert!(
                strong > mild + 1.0,
                "at {f} Hz strong ({strong:.1}) should exceed mild ({mild:.1})"
            );
        }
    }

    /// Switching presets must not leave the previous curve's tail filtering.
    ///
    /// Measured on one long-lived engine on purpose: a fresh engine per reading
    /// would reset the very state this is checking for.
    #[test]
    fn switching_presets_clears_the_previous_curve() {
        let mut e = Engine::new(SR, 2);
        apply(&e.params, 4); // strong
        apply(&e.params, 0); // bypass
        apply(&e.params, 1); // vocals
        apply(&e.params, 2); // backing
        apply(&e.params, 0); // bypass again

        for f in [80.0, 1200.0, 2500.0, 12000.0] {
            assert_near(
                gain_of(&mut e, f),
                0.0,
                &format!("bypass after other presets at {f} Hz"),
            );
        }
    }
}
