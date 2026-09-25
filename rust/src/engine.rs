//! The processing chain and the parameter plumbing around it.
//!
//! Threading model: [`Params`] lives behind atomics and is written from the UI
//! thread; [`Engine::process_i16`] runs on the audio thread and only *reads* it.
//! There is no lock and no allocation on the audio path — every buffer the
//! engine needs is sized in `new()`.
//!
//! Coefficient recomputation is gated on a version counter rather than done per
//! block: `sin`/`cos` are cheap but not free, and audio blocks arrive ~250 times
//! a second forever.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::biquad::Biquad;
use crate::limiter::Limiter;
use crate::virtual_bass::VirtualBass;

pub const MAX_BANDS: usize = 10;
pub const MAX_CHANNELS: usize = 2;

pub const KIND_PEAKING: u32 = 0;
pub const KIND_LOW_SHELF: u32 = 1;
pub const KIND_HIGH_SHELF: u32 = 2;

/// Frequency below which the gain-smoothing one-pole settles. Long enough to
/// avoid zipper noise, short enough to feel instant when the user drags a slider.
const GAIN_SMOOTH_HZ: f32 = 20.0;

struct BandParams {
    kind: AtomicU32,
    freq_hz: AtomicU32,
    gain_db: AtomicU32,
    q: AtomicU32,
}

impl BandParams {
    fn new(kind: u32, freq_hz: f32, gain_db: f32, q: f32) -> Self {
        BandParams {
            kind: AtomicU32::new(kind),
            freq_hz: AtomicU32::new(freq_hz.to_bits()),
            gain_db: AtomicU32::new(gain_db.to_bits()),
            q: AtomicU32::new(q.to_bits()),
        }
    }
}

/// Written by the UI thread, read by the audio thread. Every field is atomic;
/// `version` is bumped on any change so the audio thread knows to recompute.
pub struct Params {
    version: AtomicU32,
    bands: [BandParams; MAX_BANDS],
    output_gain_db: AtomicU32,
    threshold_db: AtomicU32,
    virtual_bass: AtomicU32,
    /// Whether the EQ is doing anything at all. Cleared by the flat preset.
    ///
    /// Not a master switch — the virtual bass has its own, and the chain runs
    /// while either of them has work to do. See [`Engine::process_i16`].
    eq_enabled: AtomicBool,
}

impl Params {
    pub fn new() -> Self {
        let bands = std::array::from_fn(|_| BandParams::new(KIND_PEAKING, 1000.0, 0.0, 1.0));
        Params {
            version: AtomicU32::new(0),
            bands,
            output_gain_db: AtomicU32::new(0.0f32.to_bits()),
            // Parenthesised on purpose: `-1.0f32.to_bits()` parses as
            // `-(1.0f32.to_bits())`, i.e. unary minus on a u32.
            threshold_db: AtomicU32::new((-1.0f32).to_bits()),
            virtual_bass: AtomicU32::new(0.0f32.to_bits()),
            eq_enabled: AtomicBool::new(true),
        }
    }

    fn bump(&self) {
        // Release so the field writes above are visible to the acquiring audio thread.
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn set_band(&self, index: usize, kind: u32, freq_hz: f32, gain_db: f32, q: f32) {
        if index >= MAX_BANDS {
            return;
        }
        let b = &self.bands[index];
        b.kind.store(kind, Ordering::Relaxed);
        b.freq_hz.store(freq_hz.to_bits(), Ordering::Relaxed);
        b.gain_db.store(gain_db.to_bits(), Ordering::Relaxed);
        b.q.store(q.to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn set_output_gain_db(&self, db: f32) {
        self.output_gain_db.store(db.to_bits(), Ordering::Relaxed);
        self.bump();
    }

    pub fn set_threshold_db(&self, db: f32) {
        self.threshold_db.store(db.to_bits(), Ordering::Relaxed);
        self.bump();
    }

    /// 0..1, where 0 bypasses the virtual-bass module entirely.
    ///
    /// Deliberately separate from the preset table: the harmonic layer and the EQ
    /// curve answer different questions, and a user who wants a bass lift should
    /// not have to give up the curve they picked.
    pub fn set_virtual_bass(&self, amount: f32) {
        self.virtual_bass.store(amount.to_bits(), Ordering::Relaxed);
        self.bump();
    }

    /// Turns the EQ off without touching the virtual bass.
    ///
    /// Callers clearing this are expected to have flattened the bands already —
    /// [`crate::presets::apply`] does. [`Engine::refresh`] flattens them anyway, so
    /// that "the EQ is disabled" means the EQ is transparent rather than meaning it
    /// happens to be holding a flat curve.
    pub fn set_eq_enabled(&self, on: bool) {
        self.eq_enabled.store(on, Ordering::Relaxed);
        self.bump();
    }

    fn eq_enabled(&self) -> bool {
        self.eq_enabled.load(Ordering::Relaxed)
    }

    fn virtual_bass_amount(&self) -> f32 {
        f32::from_bits(self.virtual_bass.load(Ordering::Relaxed))
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Engine {
    sample_rate: f32,
    channels: usize,
    pub params: Params,

    /// One filter chain **per channel**. Sharing a single chain and feeding it
    /// interleaved L/R/R would run every section at twice the real sample rate,
    /// which detunes the whole EQ — a filter centred on 1 kHz lands on 500 Hz.
    filters: [[Biquad; MAX_BANDS]; MAX_CHANNELS],
    virtual_bass: VirtualBass,
    limiter: Limiter,

    cached_version: u32,
    /// Gain actually applied last frame, stepped toward the target to avoid zipper noise.
    smooth_gain: f32,
    target_gain: f32,
    gain_coef: f32,
}

impl Engine {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        let filters = [[Biquad::identity(); MAX_BANDS]; MAX_CHANNELS];
        Engine {
            sample_rate,
            channels,
            params: Params::new(),
            filters,
            virtual_bass: VirtualBass::new(sample_rate, channels),
            limiter: Limiter::new(sample_rate, channels, -1.0, 2.0),
            // u32::MAX can never be a real version, so the first process() always refreshes.
            cached_version: u32::MAX,
            smooth_gain: 1.0,
            target_gain: 1.0,
            gain_coef: (-2.0 * std::f32::consts::PI * GAIN_SMOOTH_HZ / sample_rate).exp(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Whether the chain has anything to do.
    ///
    /// Read from the parameters rather than a refreshed cache so that
    /// [`Engine::latency_frames`] is right even before the first block, and so the
    /// bypass and the reported latency are decided by the same expression and
    /// cannot drift apart.
    fn chain_is_active(&self) -> bool {
        self.params.eq_enabled() || self.params.virtual_bass_amount() > 0.0
    }

    /// Latency the chain adds, in frames. Zero when nothing is being processed.
    pub fn latency_frames(&self) -> usize {
        if self.chain_is_active() {
            self.limiter.latency_frames()
        } else {
            0
        }
    }

    pub fn reduction_db(&self) -> f32 {
        self.limiter.reduction_db()
    }

    /// Recomputes filter coefficients if the UI thread changed anything.
    fn refresh(&mut self) {
        let v = self.params.version.load(Ordering::Acquire);
        if v == self.cached_version {
            return;
        }

        let eq_enabled = self.params.eq_enabled();

        for i in 0..MAX_BANDS {
            let b = &self.params.bands[i];
            let kind = b.kind.load(Ordering::Relaxed);
            let freq = f32::from_bits(b.freq_hz.load(Ordering::Relaxed));
            let gain = f32::from_bits(b.gain_db.load(Ordering::Relaxed));
            let q = f32::from_bits(b.q.load(Ordering::Relaxed));

            // Flattening on disable is belt-and-braces: `presets::apply` already
            // writes a unity curve for its flat preset, but making it happen here
            // means a caller who clears the flag on its own still gets a bypass,
            // rather than a chain quietly filtering with whatever curve was left.
            let new = if eq_enabled {
                match kind {
                    KIND_LOW_SHELF => Biquad::low_shelf(self.sample_rate, freq, gain, q),
                    KIND_HIGH_SHELF => Biquad::high_shelf(self.sample_rate, freq, gain, q),
                    _ => Biquad::peaking(self.sample_rate, freq, gain, q),
                }
            } else {
                Biquad::identity()
            };
            // Copy coefficients into every channel but keep each one's z1/z2:
            // swapping a filter out from under its own state is what makes
            // parameter changes click.
            for c in 0..MAX_CHANNELS {
                self.filters[c][i].replace_coefficients(&new);
            }
        }

        self.target_gain =
            10f32.powf(f32::from_bits(self.params.output_gain_db.load(Ordering::Relaxed)) / 20.0);
        self.limiter.set_threshold_db(f32::from_bits(
            self.params.threshold_db.load(Ordering::Relaxed),
        ));
        // Cheap enough to do on every parameter change: the module's filters are
        // fixed at construction, so this is one clamp and a multiply.
        self.virtual_bass
            .set_amount(self.params.virtual_bass_amount());

        self.cached_version = v;
    }

    /// Processes interleaved i16 in place.
    pub fn process_i16(&mut self, buf: &mut [i16]) {
        self.refresh();

        // Bypass means "nothing is being processed", which is what makes an A/B
        // against it honest — no EQ, and the limiter's look-ahead drops out too, so
        // the comparison is against *nothing* rather than against a re-levelled and
        // delayed copy.
        //
        // Note this is a property of the whole chain, not of the EQ preset. The flat
        // preset on its own is still a true bypass, but the flat preset with the
        // virtual bass switched on is not: the bass *is* the processing. Gating on
        // the preset number instead would make that combination silently inert, and
        // it is the only way to hear the module by itself.
        if !self.chain_is_active() {
            return;
        }

        let ch = self.channels;
        let frames = buf.len() / ch;
        if frames == 0 {
            return;
        }

        for f in 0..frames {
            let base = f * ch;
            let mut frame = [0.0f32; MAX_CHANNELS];

            for c in 0..ch {
                frame[c] = buf[base + c] as f32 * (1.0 / 32768.0);
            }

            // One-pole step toward the target gain, per sample.
            self.smooth_gain += (self.target_gain - self.smooth_gain) * self.gain_coef;
            let g = self.smooth_gain;

            for c in 0..ch {
                let mut x = frame[c] * g;
                for filt in self.filters[c].iter_mut() {
                    x = filt.process(x);
                }
                frame[c] = x;
            }

            // After the EQ so a user's bass cut also calms the harmonic generator
            // (one control, two effects, in the direction they asked for), and
            // before the limiter so the layer's extra peak is accounted for.
            self.virtual_bass.process_frame(&mut frame[..ch]);
            self.limiter.process_frame(&mut frame[..ch]);

            for c in 0..ch {
                let v = (frame[c] * 32768.0).clamp(-32768.0, 32767.0);
                buf[base + c] = v as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(buf: &[i16]) -> f32 {
        if buf.is_empty() {
            return 0.0;
        }
        let sum: f64 = buf.iter().map(|v| (*v as f64) * (*v as f64)).sum();
        (sum / buf.len() as f64).sqrt() as f32
    }

    fn tone(frames: usize, freq: f32, amp: f32, sr: f32) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let x = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin();
                let v = (x * 32767.0) as i16;
                [v, v]
            })
            .collect()
    }

    /// Level of `freq` in channel 0, in dBFS, over the second half of `buf`.
    ///
    /// A single-bin DFT, as in the virtual-bass tests: half a second of any
    /// multiple of 2 Hz is a whole number of periods, so there is no leakage.
    fn level_db(buf: &[i16], freq: f32) -> f32 {
        let frames = buf.len() / 2;
        let start = frames / 2;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for i in start..frames {
            let x = buf[i * 2] as f64 / 32768.0;
            let w = 2.0 * std::f64::consts::PI * freq as f64 * (i - start) as f64 / 48000.0;
            re += x * w.cos();
            im -= x * w.sin();
        }
        let n = (frames - start) as f64;
        let amp = (2.0 * (re * re + im * im).sqrt() / n) as f32;
        20.0 * amp.max(1e-9).log10()
    }

    #[test]
    fn disabled_engine_is_identity() {
        let mut e = Engine::new(48000.0, 2);
        e.params.set_eq_enabled(false);
        let mut buf = tone(4800, 440.0, 0.5, 48000.0);
        let before = buf.clone();
        e.process_i16(&mut buf);
        assert_eq!(buf, before, "bypass altered the signal");
    }

    #[test]
    fn boost_raises_level() {
        let mut e = Engine::new(48000.0, 2);
        e.params.set_band(0, KIND_PEAKING, 1000.0, 12.0, 1.0);
        e.params.set_threshold_db(0.0);
        let mut buf = tone(48000, 1000.0, 0.2, 48000.0);
        e.process_i16(&mut buf);
        let out = rms(&buf[1000..]);
        assert!(
            out > 0.2 * 32767.0 * 1.5,
            "boost did not raise level: {out}"
        );
    }

    /// Even a crazy boost must not clip once the limiter is in the chain.
    #[test]
    fn limiter_contains_extreme_boost() {
        let mut e = Engine::new(48000.0, 2);
        for i in 0..MAX_BANDS {
            e.params.set_band(i, KIND_PEAKING, 1000.0, 24.0, 1.0);
        }
        e.params.set_output_gain_db(24.0);
        e.params.set_threshold_db(-1.0);
        let mut buf = tone(48000, 1000.0, 0.5, 48000.0);
        e.process_i16(&mut buf);
        let peak = buf[5000..].iter().map(|v| (*v as i32).abs()).max().unwrap();
        assert!(peak <= 32767, "clipped at {peak}");
    }

    /// Regression test for shared filter state. Feeding interleaved stereo
    /// through one filter chain both detunes the EQ and lets the loud channel
    /// ring into the silent one.
    #[test]
    fn stereo_channels_are_independent() {
        let mut e = Engine::new(48000.0, 2);
        e.params.set_band(0, KIND_PEAKING, 1000.0, 12.0, 1.0);

        let frames = 4800usize;
        let mut buf = vec![0i16; frames * 2];
        for f in 0..frames {
            let x = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * f as f32 / 48000.0).sin();
            buf[f * 2] = (x * 32767.0) as i16;
            buf[f * 2 + 1] = 0;
        }
        e.process_i16(&mut buf);

        let right_peak = buf[2000..]
            .iter()
            .skip(1)
            .step_by(2)
            .map(|v| (*v as i32).abs())
            .max()
            .unwrap();
        assert_eq!(
            right_peak, 0,
            "tone bled into the silent channel: {right_peak}"
        );
    }

    /// The module measures itself in isolation; this checks the thing those tests
    /// cannot — that it is actually plugged into the chain, and that it still
    /// works after the EQ and into the limiter.
    #[test]
    fn virtual_bass_is_wired_into_the_chain() {
        let with = {
            let mut e = Engine::new(48000.0, 2);
            e.params.set_virtual_bass(1.0);
            let mut buf = tone(48_000, 60.0, 0.2, 48000.0);
            e.process_i16(&mut buf);
            level_db(&buf, 300.0)
        };
        let without = {
            let mut e = Engine::new(48000.0, 2);
            let mut buf = tone(48_000, 60.0, 0.2, 48000.0);
            e.process_i16(&mut buf);
            level_db(&buf, 300.0)
        };
        println!("300 Hz: {without:.1} dBFS without, {with:.1} dBFS with");
        assert!(
            with > -45.0,
            "no usable harmonic reached the output: {with:.1} dBFS"
        );
        assert!(
            with > without + 30.0,
            "the module is present but doing nothing: {without:.1} -> {with:.1} dBFS"
        );
    }

    /// The module is minimum-phase — filters and a memoryless shaper, no delay
    /// line — so it adds no latency of its own.
    ///
    /// It does need the limiter to run, because the layer adds peak. What it must
    /// not do is delay anything *beyond* that: the UI's A/B compares treated and
    /// untreated audio, and a latency that grew with the amount would be heard as
    /// an echo rather than as a tone change.
    #[test]
    fn virtual_bass_adds_no_latency_of_its_own() {
        let e = Engine::new(48000.0, 2);
        e.params.set_eq_enabled(false);
        assert_eq!(e.latency_frames(), 0, "nothing running, nothing delayed");

        e.params.set_virtual_bass(0.35);
        let with_limiter = e.latency_frames();
        assert!(
            with_limiter > 0,
            "the bass layer adds peak, so the limiter has to run"
        );

        e.params.set_virtual_bass(1.0);
        assert_eq!(
            e.latency_frames(),
            with_limiter,
            "latency moved with the amount, so it is not coming from the limiter alone"
        );
    }

    /// Preset 0 is still the honest control for the EQ: with the bass off it is a
    /// true bypass — no filtering, no limiter, bit-exact.
    ///
    /// What it must *not* be is a master switch. Gating the whole chain on the
    /// preset number made "flat EQ + virtual bass" silently do nothing, and that
    /// combination is the only way to hear the module on its own.
    #[test]
    fn the_flat_preset_is_a_true_bypass_but_not_a_master_switch() {
        use crate::presets;

        let mut e = Engine::new(48000.0, 2);
        presets::apply(&e.params, 0);
        let mut buf = tone(4800, 60.0, 0.2, 48000.0);
        let before = buf.clone();
        e.process_i16(&mut buf);
        assert_eq!(buf, before, "flat preset with the bass off is not a bypass");
        assert_eq!(e.latency_frames(), 0);

        // Switching the bass on has to bring the module back, and only the module.
        e.params.set_virtual_bass(1.0);
        let mut buf = tone(48_000, 60.0, 0.2, 48000.0);
        e.process_i16(&mut buf);
        let harmonics = level_db(&buf, 300.0);
        println!("flat preset + bass on: 300 Hz at {harmonics:.1} dBFS");
        assert!(
            harmonics > -45.0,
            "the bass module stayed inert: {harmonics:.1} dBFS"
        );
    }

    /// With the bass on, the EQ filters still run — they have to, the harmonic
    /// layer is added after them. So disabling the EQ has to make those filters
    /// transparent, not merely make the preset number say they are.
    #[test]
    fn disabling_the_eq_leaves_no_curve_behind() {
        let mut e = Engine::new(48000.0, 2);
        e.params.set_band(0, KIND_PEAKING, 1000.0, 12.0, 1.0);
        e.params.set_virtual_bass(1.0);

        let mut buf = tone(48_000, 1000.0, 0.05, 48000.0);
        e.process_i16(&mut buf);
        let boosted = level_db(&buf, 1000.0);

        e.params.set_eq_enabled(false);
        let mut buf = tone(48_000, 1000.0, 0.05, 48000.0);
        e.process_i16(&mut buf);
        let flat = level_db(&buf, 1000.0);

        println!("1 kHz: {boosted:.1} dBFS boosted, {flat:.1} dBFS after disabling the EQ");
        assert!(
            boosted - flat > 3.0,
            "the boost survived disabling the EQ: {boosted:.1} -> {flat:.1} dBFS"
        );
    }

    #[test]
    fn parameter_change_keeps_filter_state() {
        let mut e = Engine::new(48000.0, 2);
        e.params.set_band(0, KIND_PEAKING, 1000.0, 6.0, 1.0);
        let mut buf = tone(4800, 1000.0, 0.3, 48000.0);
        e.process_i16(&mut buf);
        // Changing a parameter must not reset the chain; a reset would show up as
        // a discontinuity in the next block.
        e.params.set_band(0, KIND_PEAKING, 1000.0, 7.0, 1.0);
        let mut next = tone(480, 1000.0, 0.3, 48000.0);
        e.process_i16(&mut next);
        let first = next[0].abs() as f32;
        assert!(
            first < 12000.0,
            "discontinuity after parameter change: {first}"
        );
    }
}
