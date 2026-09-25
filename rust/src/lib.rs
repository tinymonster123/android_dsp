//! JNI surface for the DSP core.
//!
//! Deliberately thin. Everything interesting lives in [`engine`]; this file only
//! converts values and manages the opaque handle Kotlin holds.
//!
//! ## Handle model
//!
//! `nativeCreate` boxes a [`Handle`] and hands back its raw address as a `jlong`.
//! Kotlin must call `nativeDestroy` exactly once. The handle is **not** internally
//! synchronised — assume a single UI thread calling the setters and a single audio
//! thread calling `nativeProcess`. That is precisely why the setters in `engine`
//! go through atomics.
//!
//! ## Allocation
//!
//! `nativeProcess` runs on the audio thread and must not allocate. The scratch
//! buffer it needs is therefore sized once in `nativeCreate`. If a caller ever
//! passes a larger array than the scratch can hold we drop the block rather than
//! allocate — a dropped 4 ms block is inaudible, an allocation is not.

pub mod biquad;
pub mod engine;
pub mod limiter;
pub mod presets;
pub mod virtual_bass;

use jni::objects::{JClass, JShortArray};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::JNIEnv;

use engine::{Engine, MAX_CHANNELS};

/// Room for 4096 frames. The Android side feeds 192-frame blocks, so this is
/// ~20x headroom; it exists to make the "no allocation on the audio thread"
/// promise hold rather than to be tight.
const SCRATCH_FRAMES: usize = 4096;

struct Handle {
    engine: Engine,
    scratch: Vec<i16>,
}

impl Handle {
    fn new(sample_rate: f32, channels: usize) -> Self {
        Handle {
            scratch: vec![0i16; SCRATCH_FRAMES * channels],
            engine: Engine::new(sample_rate, channels),
        }
    }
}

/// # Safety
/// `handle` must be a pointer previously returned by `nativeCreate` and not yet
/// passed to `nativeDestroy`.
#[inline]
unsafe fn handle_mut<'a>(handle: jlong) -> Option<&'a mut Handle> {
    if handle == 0 {
        None
    } else {
        Some(&mut *(handle as *mut Handle))
    }
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeCreate(
    _env: JNIEnv,
    _class: JClass,
    sample_rate: jint,
    channels: jint,
) -> jlong {
    let ch = (channels as usize).clamp(1, MAX_CHANNELS);
    let h = Box::new(Handle::new(sample_rate as f32, ch));
    Box::into_raw(h) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // Reclaiming the Box drops the engine and its buffers.
        unsafe { drop(Box::from_raw(handle as *mut Handle)) };
    }
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeSetBand(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    kind: jint,
    freq_hz: jfloat,
    gain_db: jfloat,
    q: jfloat,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        h.engine
            .params
            .set_band(index as usize, kind as u32, freq_hz, gain_db, q);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeSetOutputGainDb(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    db: jfloat,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        h.engine.params.set_output_gain_db(db);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeSetThresholdDb(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    db: jfloat,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        h.engine.params.set_threshold_db(db);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeSetEnabled(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    enabled: jboolean,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        h.engine.params.set_enabled(enabled != 0);
    }
}

/// Processes interleaved 16-bit PCM in place.
///
/// `length` is how many of `buf`'s elements are valid. `AudioRecord.read` returns
/// a short count, so the caller's buffer is routinely longer than the audio in
/// it; processing the whole array would run stale samples through the chain.
#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeProcess(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    buf: JShortArray,
    length: jint,
) {
    let Some(h) = (unsafe { handle_mut(handle) }) else {
        return;
    };

    let cap = match env.get_array_length(&buf) {
        Ok(n) => n as usize,
        Err(_) => return,
    };
    let len = (length.max(0) as usize).min(cap);
    if len == 0 || len > h.scratch.len() {
        // Never allocate here. Dropping an oversized block beats stalling audio.
        return;
    }

    if env
        .get_short_array_region(&buf, 0, &mut h.scratch[..len])
        .is_err()
    {
        return;
    }

    h.engine.process_i16(&mut h.scratch[..len]);

    let _ = env.set_short_array_region(&buf, 0, &h.scratch[..len]);
}

/// Applies curve `index`. Presets live in Rust so their frequency response can be
/// measured by tests instead of asserted in a comment.
#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeApplyPreset(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        presets::apply(&h.engine.params, index.max(0) as usize);
    }
}

/// Applies virtual-bass step `index`. The step's `amount` lives in
/// [`virtual_bass::LEVELS`], so Kotlin never handles the number.
#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeSetVirtualBassLevel(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) {
    if let Some(h) = unsafe { handle_mut(handle) } {
        let amount = virtual_bass::level_amount(index.max(0) as usize);
        h.engine.params.set_virtual_bass(amount);
    }
}

/// Number of virtual-bass steps. Same contract as `nativePresetCount`: Kotlin
/// keeps the names and checks its table against this.
#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeVirtualBassLevelCount(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    virtual_bass::level_count() as jint
}

/// Number of curves the native side knows about. Kotlin keeps the display names
/// and checks its own table against this, so the two cannot silently drift.
#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativePresetCount(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    presets::preset_count() as jint
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeReductionDb(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jfloat {
    unsafe { handle_mut(handle) }
        .map(|h| h.engine.reduction_db())
        .unwrap_or(0.0)
}

#[no_mangle]
pub extern "system" fn Java_com_fenghanli_dspprobe_dsp_DspEngine_nativeLatencyFrames(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    unsafe { handle_mut(handle) }
        .map(|h| h.engine.latency_frames() as jint)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_round_trip() {
        let h = Handle::new(48000.0, 2);
        assert_eq!(h.scratch.len(), SCRATCH_FRAMES * 2);
        assert_eq!(h.engine.channels(), 2);
    }

    #[test]
    fn channels_are_clamped() {
        let h = Handle::new(48000.0, 99);
        assert_eq!(h.engine.channels(), MAX_CHANNELS);
        let h = Handle::new(48000.0, 0);
        assert_eq!(h.engine.channels(), 1);
    }
}
