//! Virtual bass: giving a phone speaker low end it cannot physically produce.
//!
//! A 12 mm driver cannot move air at 60 Hz. What it can do is play 300 Hz and
//! 420 Hz, and the ear will infer the 60 Hz fundamental from them — pitch is read
//! off harmonic *spacing*, not off the presence of the fundamental itself. That is
//! the whole trick, and it is what makes this worth doing on hardware that is
//! honest about its limits.
//!
//! ## The harmonics have to be odd, and this is the part that is easy to get wrong
//!
//! The wrong version still "works" — it just sounds like a bass an octave too high.
//!
//! Generate only even harmonics of 60 Hz: 120, 240, 360, 480. Every component is a
//! multiple of 120, so 120 is the finest periodicity the waveform has, and 120 Hz
//! is the pitch the ear reports. **Full-wave rectification — the obvious choice —
//! does exactly this**: DC plus even harmonics, nothing else.
//!
//! Generate odd harmonics instead: 180, 300, 420, 540. They share no common factor
//! beyond 60, the waveform repeats at 60 Hz, and the ear reports 60 Hz. (This is
//! why a clarinet, which is close to odd-harmonic, sounds at its written pitch and
//! not an octave up.)
//!
//! So the generator is **odd-symmetric**, `f(-x) == -f(x)`, which rules out
//! rectifiers and any deliberate DC offset into the clipper. A symmetric saturator
//! gives odd harmonics for free and has two more things going for it: its harmonic
//! amplitudes fall off as `1/n` rather than the `1/n²` a rectifier gives, so there
//! are enough high harmonics to reach the band the speaker can actually play; and
//! its output is bounded, so it cannot hand the limiter a surprise.
//!
//! There is a third, less obvious benefit, and the drive is set to get it: once
//! the saturator is driven hard the absolute level of each harmonic stops
//! depending on the input level. A signal clipped to a square has a fundamental
//! of `4/π` and a third harmonic of `4/(3π)` no matter how far past the knee it
//! was driven. Driving hard is therefore also a cheap form of normalisation — a
//! moderately quiet passage produces nearly as much harmonic layer as a loud one,
//! instead of the bass quietly disappearing whenever the music does.
//!
//! How hard is "hard" is a measurement, not a preference. [`DRIVE_DB`] was set
//! from one: [`soft_clip`] approaches its asymptote slowly (as `1 - 0.25/|x|`), so
//! square-wave behaviour needs an input well past unity, and the first attempt at
//! 30 dB still let the layer track the input almost proportionally on quiet
//! material. At the other end the drive must not be so deep that the layer is
//! full-amplitude from a whisper: a few dB of rumble would then produce a constant
//! buzz through quiet passages. What is wanted is a soft gate — constant above
//! roughly −30 dBFS of bass, faded out by −55 dBFS — and the drive is what places
//! the gate.
//!
//! ## Why there is no oversampling
//!
//! Clipping generates harmonics up to and past Nyquist, and everything past
//! Nyquist folds back down as inharmonic rubbish. The usual fix is to oversample
//! around the nonlinearity. It is not needed here for a reason worth stating: the
//! shaper only ever sees the output of the low-pass *in front of it*, which is
//! already limited to ~120 Hz. Its harmonics are one lone tone's harmonics falling
//! off as `1/n`, so what folds back is tens of dB down before the band-pass after
//! the shaper attenuates it further. `folding_stays_below_the_noise_floor` measures
//! this instead of trusting the argument.

use crate::biquad::Biquad;
use crate::engine::MAX_CHANNELS;
use crate::limiter::soft_clip;

/// Corner of the bass-extraction low-pass, 4th-order Linkwitz-Riley.
///
/// Not higher: everything the shaper sees goes on to be multiplied into
/// intermodulation products, and midrange content in the shaper is what makes a
/// virtual-bass effect sound gritty rather than deep.
pub const BASS_LP_HZ: f32 = 120.0;

/// Lower edge of the generated harmonic band, and the reason it sits this high:
/// the fundamental survives the shaper essentially untouched and has to be taken
/// back out — because the shaper is driven to a near-square, its fundamental comes
/// out at roughly the level of the original bass, so without this filter the
/// module would simply undo the bass cut it exists to make unnecessary.
///
/// Tuned down from 220 Hz after measuring: at 220, the third harmonic of a 40 Hz
/// note sits at 120 Hz and lands 21 dB into the stopband, so the deepest — and
/// most valuable — bass was getting the weakest treatment. At 180 Hz a 60 Hz
/// fundamental is still 38 dB down, which is all the rejection that is needed,
/// and deep-bass harmonics come through.
pub const HARMONIC_HP_HZ: f32 = 180.0;

/// Upper edge of the harmonic band. Harmonics above this are heard as buzz rather
/// than as bass, and the driver plays them perfectly well without help, so they
/// are only cost.
pub const HARMONIC_LP_HZ: f32 = 1800.0;

/// How hard the bass is pushed into the shaper.
///
/// Set by measurement, not taste. `soft_clip` saturates slowly, so reaching
/// square-wave behaviour — where the harmonic level stops depending on the input —
/// needs roughly `|x| > 5` into the shaper. At 44 dB that means a bass peak over
/// ~0.03 (about −30 dBFS), which is typical programme material rather than a
/// loud passage; below ~0.0015 (−56 dBFS) the shaper is inside its linear region
/// and the layer fades out, so silence stays silent.
const DRIVE_DB: f32 = 44.0;

/// Level of the harmonic layer at `amount == 1.0`.
///
/// The shaper's output is bounded near ±1.27 — a square wave's fundamental — and
/// is **independent of the input level** once driven hard, so this number means
/// the same thing for quiet and loud material alike.
///
/// 0.25 puts the harmonics around −30 dBFS for typical material. That is
/// deliberately in the same range as the dry signal *after* the phone speaker has
/// attenuated it: a driver that loses 25 dB below 300 Hz leaves the real
/// fundamental quieter than this layer, which is the point. Push it much further
/// and the layer starts costing peak headroom the limiter has to give back.
const MIX_MAX: f32 = 0.25;

/// Butterworth Q. Two of these in series is Linkwitz-Riley 4th order, i.e. the
/// 24 dB/oct slope a crossover is normally built from.
const LR4_Q: f32 = crate::biquad::BUTTERWORTH_Q;

/// The steps the UI cycles through, as `amount` values.
///
/// Rust owns these because they are DSP parameters, not copy — the same reason
/// the EQ curves live here. Kotlin holds only the names, and checks its table
/// against [`level_count`] so the two cannot drift apart silently.
pub const LEVELS: &[f32] = &[0.0, 0.35, 0.65, 1.0];

pub fn level_count() -> usize {
    LEVELS.len()
}

/// The `amount` for `index`, clamped to a valid step. Out-of-range input is
/// clamped rather than ignored: this is reached from a UI index, and a stale
/// index should land on the nearest real setting instead of doing nothing.
pub fn level_amount(index: usize) -> f32 {
    LEVELS[index.min(LEVELS.len() - 1)]
}

pub struct VirtualBass {
    channels: usize,
    /// Cascaded pairs, one pair per channel, per stage: extract → shape → place.
    bass: [[Biquad; 2]; MAX_CHANNELS],
    band_hp: [[Biquad; 2]; MAX_CHANNELS],
    band_lp: [[Biquad; 2]; MAX_CHANNELS],
    drive: f32,
    /// Linear level of the harmonic layer. Zero means fully bypassed.
    mix: f32,
}

fn lr4_low_pass(sample_rate: f32, freq_hz: f32) -> [Biquad; 2] {
    [
        Biquad::low_pass(sample_rate, freq_hz, LR4_Q),
        Biquad::low_pass(sample_rate, freq_hz, LR4_Q),
    ]
}

fn lr4_high_pass(sample_rate: f32, freq_hz: f32) -> [Biquad; 2] {
    [
        Biquad::high_pass(sample_rate, freq_hz, LR4_Q),
        Biquad::high_pass(sample_rate, freq_hz, LR4_Q),
    ]
}

impl VirtualBass {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        VirtualBass {
            channels,
            bass: std::array::from_fn(|_| lr4_low_pass(sample_rate, BASS_LP_HZ)),
            band_hp: std::array::from_fn(|_| lr4_high_pass(sample_rate, HARMONIC_HP_HZ)),
            band_lp: std::array::from_fn(|_| lr4_low_pass(sample_rate, HARMONIC_LP_HZ)),
            drive: 10f32.powf(DRIVE_DB / 20.0),
            mix: 0.0,
        }
    }

    /// `amount` runs 0..1. Zero disables the module outright rather than relying
    /// on a zero mix, so the dry path is bit-exact while it is off.
    pub fn set_amount(&mut self, amount: f32) {
        let amount = if amount.is_finite() { amount } else { 0.0 };
        self.mix = amount.clamp(0.0, 1.0) * MIX_MAX;
    }

    pub fn is_active(&self) -> bool {
        self.mix > 0.0
    }

    /// Adds the harmonic layer to one interleaved frame, in place.
    #[inline]
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        if self.mix == 0.0 {
            return;
        }
        for (c, s) in frame.iter_mut().enumerate().take(self.channels) {
            let mut bass = *s;
            for f in self.bass[c].iter_mut() {
                bass = f.process(bass);
            }

            let mut harm = soft_clip(bass * self.drive);

            for f in self.band_hp[c].iter_mut() {
                harm = f.process(harm);
            }
            for f in self.band_lp[c].iter_mut() {
                harm = f.process(harm);
            }

            *s += harm * self.mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;
    const FRAMES: usize = 48_000;

    /// Runs a mono sine through the module and returns what came out.
    fn run(vb: &mut VirtualBass, freq: f32, amp: f32) -> Vec<f32> {
        (0..FRAMES)
            .map(|i| {
                let mut frame = [
                    amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR).sin(),
                    0.0,
                ];
                vb.process_frame(&mut frame);
                frame[0]
            })
            .collect()
    }

    /// Second half only — the first is filter and envelope transient.
    fn settled(buf: &[f32]) -> &[f32] {
        &buf[FRAMES / 2..]
    }

    /// Amplitude of the single frequency `freq`, by direct DFT over the window.
    ///
    /// Every frequency this file asks about is a multiple of 2 Hz, so the window
    /// (0.5 s) holds a whole number of periods and there is no leakage to correct.
    fn magnitude(buf: &[f32], freq: f32) -> f32 {
        let n = buf.len() as f64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &x) in buf.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * freq as f64 * i as f64 / SR as f64;
            re += x as f64 * w.cos();
            im -= x as f64 * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() as f32 / n as f32
    }

    fn db(x: f32) -> f32 {
        20.0 * x.max(1e-9).log10()
    }

    /// Runs a 60 Hz sine through the module at `amount` and returns the settled
    /// output, which is the signal every claim below is checked against.
    fn settled_output(amount: f32, freq: f32, amp: f32) -> Vec<f32> {
        let mut vb = VirtualBass::new(SR, 2);
        vb.set_amount(amount);
        let out = run(&mut vb, freq, amp);
        settled(&out).to_vec()
    }

    /// The claim the module exists to make: a tone the speaker cannot reproduce
    /// comes out with energy in a band it can.
    #[test]
    fn puts_harmonics_where_the_speaker_can_play_them() {
        let dry = settled_output(0.0, 60.0, 0.2);
        let wet = settled_output(1.0, 60.0, 0.2);

        // 300 Hz and 420 Hz — the 5th and 7th of 60 Hz, both squarely inside what
        // a phone driver can move.
        for f in [300.0, 420.0] {
            let before = db(magnitude(&dry, f));
            let after = db(magnitude(&wet, f));
            println!("{f} Hz: {before:.1} -> {after:.1} dBFS");
            assert!(after > -40.0, "no usable energy at {f} Hz: {after:.1} dBFS");
            assert!(
                after > before + 20.0,
                "at {f} Hz the module barely changed anything: {before:.1} -> {after:.1}"
            );
        }
    }

    /// The property that separates this design from a rectifier, and the one a
    /// future "improvement" is most likely to break.
    ///
    /// Odd harmonics of 60 Hz share no common factor beyond 60, so the waveform
    /// repeats at 60 Hz and the ear hears 60 Hz. Even harmonics would make every
    /// component a multiple of 120 and the bass would sound an octave high.
    #[test]
    fn the_harmonics_are_odd_so_the_pitch_is_not_an_octave_up() {
        let out = settled_output(1.0, 60.0, 0.2);

        let odd: f32 = [180.0, 300.0, 420.0]
            .iter()
            .map(|&f| magnitude(&out, f))
            .fold(0.0, f32::max);
        let even: f32 = [120.0, 240.0, 360.0]
            .iter()
            .map(|&f| magnitude(&out, f))
            .fold(0.0, f32::max);

        println!(
            "loudest odd {:.1} dBFS, loudest even {:.1} dBFS",
            db(odd),
            db(even)
        );
        assert!(
            odd > 0.01,
            "there should be real odd harmonics here, got {:.1} dBFS",
            db(odd)
        );
        assert!(
            db(odd) > db(even) + 40.0,
            "even harmonics are present ({} vs {} dBFS) — the shaper is no longer \
             odd-symmetric, and the bass will sound an octave high",
            db(odd),
            db(even)
        );
    }

    /// The shaper saturates, and how it saturates is the whole behaviour of the
    /// module on real material — so it gets measured at three input levels rather
    /// than assumed.
    ///
    /// Wanted: **compressive** on loud material (the layer is a harmonic signature,
    /// not a second copy of the bass, so it must not scale with the input), and
    /// **faded out** on very quiet material (or a few dB of rumble would produce a
    /// constant full-amplitude buzz through every quiet passage).
    #[test]
    fn the_layer_compresses_on_loud_material_and_gates_on_quiet_material() {
        let level = |amp: f32| db(magnitude(&settled_output(1.0, 60.0, amp), 300.0));
        let (whisper, quiet, loud) = (level(0.002), level(0.04), level(0.4));
        println!("5th harmonic — amp 0.002: {whisper:.1}, 0.04: {quiet:.1}, 0.4: {loud:.1} dBFS");

        // 0.04 -> 0.4 is +20 dB of input. Proportional would move the layer 20 dB;
        // a saturated shaper should move it far less.
        assert!(
            loud - quiet < 10.0,
            "the layer tracked the input instead of saturating: {quiet:.1} -> {loud:.1} dBFS \
             for +20 dB of input"
        );
        // 0.04 -> 0.002 is −26 dB of input. The layer must fall much further, or
        // quiet passages carry a buzz.
        assert!(
            quiet - whisper > 26.0 + 10.0,
            "the layer did not gate on quiet material: {whisper:.1} dBFS at amp 0.002 is only \
             {:.1} dB below its level on normal material",
            quiet - whisper
        );
    }

    /// Zero must mean zero: not a small mix, but the module not running at all.
    #[test]
    fn amount_zero_is_bit_transparent() {
        let mut vb = VirtualBass::new(SR, 2);
        vb.set_amount(0.0);
        let out = run(&mut vb, 60.0, 0.2);
        for (i, &y) in out.iter().enumerate() {
            let x = 0.2 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / SR).sin();
            assert_eq!(y, x, "sample {i} was altered with the module off");
        }
    }

    /// Everything the shaper sees goes on to be multiplied together, so midrange
    /// leaking into the bass path is what turns a virtual bass into a fuzz pedal.
    /// The low-pass in front is the only thing preventing it.
    #[test]
    fn midrange_does_not_reach_the_shaper() {
        let dry = settled_output(0.0, 1000.0, 0.2);
        let wet = settled_output(1.0, 1000.0, 0.2);
        let worst = [1000.0, 2000.0, 3000.0]
            .iter()
            .map(|&f| db(magnitude(&wet, f)) - db(magnitude(&dry, f)))
            .fold(f32::NEG_INFINITY, f32::max);
        println!("worst change to a 1 kHz tone: {worst:+.1} dB");
        assert!(
            worst < 0.5,
            "a 1 kHz tone was altered by {worst:+.1} dB — midrange is reaching the shaper"
        );
    }

    /// A layer built only from odd harmonics and high-passed at both ends must
    /// still be zero-mean; anything else would be a DC offset into the limiter.
    #[test]
    fn adds_no_dc() {
        let out = settled_output(1.0, 60.0, 0.2);
        let mean: f64 = out.iter().map(|&x| x as f64).sum::<f64>() / out.len() as f64;
        println!("output mean {:.6}", mean);
        assert!(mean.abs() < 1e-3, "the module added DC: {mean}");
    }

    /// Clipping makes harmonics past Nyquist, which fold back as inharmonic
    /// rubbish. The band-pass after the shaper should have removed all of it.
    #[test]
    fn folding_stays_below_the_noise_floor() {
        let out = settled_output(1.0, 60.0, 0.2);

        // Frequencies that are deliberately not multiples of 60.
        let mut worst = 0.0f32;
        let mut worst_at = 0.0;
        for f in (250..18_000).step_by(190) {
            let m = magnitude(&out, f as f32);
            if m > worst {
                worst = m;
                worst_at = f as f32;
            }
        }
        let lowest_harmonic = db(magnitude(&out, 300.0));
        println!(
            "worst inharmonic content: {:.1} dBFS at {worst_at} Hz",
            db(worst)
        );
        assert!(
            db(worst) < lowest_harmonic - 30.0,
            "folding products at {worst_at} Hz reach {:.1} dBFS, against {lowest_harmonic:.1} \
             dBFS of real harmonic",
            db(worst)
        );
    }

    /// Turning the amount up must always mean more harmonics. If it ever does not,
    /// the knob is lying about what it does.
    #[test]
    fn amount_is_monotonic() {
        let mut last = f32::NEG_INFINITY;
        for amount in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let level = db(magnitude(&settled_output(amount, 60.0, 0.2), 300.0));
            println!("amount {amount:.2} -> 300 Hz at {level:.1} dBFS");
            assert!(
                level > last + 1.0,
                "amount {amount} did not raise the harmonic level ({level:.1} vs {last:.1})"
            );
            last = level;
        }
    }

    /// Same bug class the EQ had: one filter chain shared between channels sees an
    /// interleaved stream at twice the sample rate and detunes every filter.
    ///
    /// The tolerance is not slack — [`crate::biquad`] injects a 1e-15 anti-denormal
    /// term into every section, so a silent channel carries that residue and never
    /// comes out bit-exactly zero.
    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let mut vb = VirtualBass::new(SR, 2);
        vb.set_amount(1.0);
        let mut worst = 0.0f32;
        for i in 0..FRAMES {
            let x = 0.2 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / SR).sin();
            let mut frame = [x, 0.0];
            vb.process_frame(&mut frame);
            worst = worst.max(frame[1].abs());
        }
        assert!(worst < 1e-6, "tone bled into the silent channel at {worst}");
    }

    /// Changing the amount must not reset the filters, or every adjustment clicks.
    #[test]
    fn changing_amount_keeps_filter_state() {
        let mut vb = VirtualBass::new(SR, 2);
        vb.set_amount(0.5);
        run(&mut vb, 60.0, 0.2);
        vb.set_amount(0.9);

        // A reset would show up as a discontinuity in the very next block.
        for i in 0..480 {
            let x = 0.2 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / SR).sin();
            let mut frame = [x, 0.0];
            vb.process_frame(&mut frame);
            assert!(
                frame[0].abs() < 1.0,
                "discontinuity at sample {i}: {}",
                frame[0]
            );
        }
    }

    /// The level table is the UI's contract with the DSP. A step that produces
    /// nothing, or that sits below the step before it, is a knob that lies.
    #[test]
    fn every_offered_level_does_something_and_they_ascend() {
        assert_eq!(LEVELS[0], 0.0, "the first level has to be a true bypass");
        let mut last = f32::NEG_INFINITY;
        for (i, &amount) in LEVELS.iter().enumerate() {
            let level = db(magnitude(&settled_output(amount, 60.0, 0.2), 300.0));
            println!("level {i} (amount {amount}) -> 300 Hz at {level:.1} dBFS");
            assert!(level > last, "level {i} is not above the one before it");
            if amount > 0.0 {
                assert!(
                    level > -45.0,
                    "level {i} (amount {amount}) is too faint to hear: {level:.1} dBFS"
                );
            }
            last = level;
        }
        assert_eq!(level_amount(99), LEVELS[LEVELS.len() - 1]);
    }

    /// Not an assertion — a measurement you can read.
    ///
    /// `cargo test --manifest-path rust/Cargo.toml print_harmonic_table -- --nocapture`
    #[test]
    fn print_harmonic_table() {
        let amp = 0.2;
        println!(
            "\ninput amp {amp}  ({:.1} dBFS)   odd harmonics, dBFS",
            db(amp)
        );
        print!("{:>7}", "in Hz");
        for n in [1, 3, 5, 7, 9, 11, 15] {
            print!("{:>9}", format!("{n}f"));
        }
        println!();
        for f0 in [40.0, 60.0, 80.0, 100.0, 120.0] {
            print!("{f0:>7.0}");
            for n in [1, 3, 5, 7, 9, 11, 15] {
                let h = f0 * n as f32;
                if h >= 20_000.0 {
                    print!("{:>9}", "-");
                    continue;
                }
                let mut vb = VirtualBass::new(SR, 2);
                vb.set_amount(1.0);
                let out = run(&mut vb, f0, amp);
                print!("{:>9.1}", db(magnitude(settled(&out), h)));
            }
            println!();
        }
        println!();
    }
}
