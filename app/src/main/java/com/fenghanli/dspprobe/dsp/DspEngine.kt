package com.fenghanli.dspprobe.dsp

/**
 * Kotlin side of the Rust DSP core.
 *
 * Threading contract, and it matters:
 *  - the setters are called from the UI thread;
 *  - [process] is called from the audio thread and must not allocate or block.
 *
 * The Rust side keeps its parameters in atomics precisely so those two threads
 * never need a lock. `process` borrows the caller's array and hands the same
 * array back — no per-block garbage is produced.
 */
class DspEngine(sampleRate: Int, channels: Int) : AutoCloseable {

    private var handle: Long = nativeCreate(sampleRate, channels)

    val isValid: Boolean get() = handle != 0L

    /** Frames of delay the limiter's look-ahead adds. */
    val latencyFrames: Int get() = if (handle != 0L) nativeLatencyFrames(handle) else 0

    /** Current gain reduction in dB, for metering. 0 means the limiter is idle. */
    val reductionDb: Float get() = if (handle != 0L) nativeReductionDb(handle) else 0f

    fun setBand(index: Int, kind: Int, freqHz: Float, gainDb: Float, q: Float) {
        if (handle != 0L) nativeSetBand(handle, index, kind, freqHz, gainDb, q)
    }

    /** Applies a curve defined in Rust. See `rust/src/presets.rs` for what each one does. */
    fun applyPreset(index: Int) {
        if (handle != 0L) nativeApplyPreset(handle, index)
    }

    fun setOutputGainDb(db: Float) {
        if (handle != 0L) nativeSetOutputGainDb(handle, db)
    }

    fun setThresholdDb(db: Float) {
        if (handle != 0L) nativeSetThresholdDb(handle, db)
    }

    fun setEnabled(on: Boolean) {
        if (handle != 0L) nativeSetEnabled(handle, on)
    }

    /**
     * Processes the first [length] elements of [buf] in place.
     *
     * [length] exists because `AudioRecord.read` returns a short count: the array
     * is usually longer than the audio in it.
     */
    fun process(buf: ShortArray, length: Int) {
        if (handle != 0L) nativeProcess(handle, buf, length)
    }

    override fun close() {
        val h = handle
        if (h != 0L) {
            handle = 0L
            nativeDestroy(h)
        }
    }

    companion object {
        const val KIND_PEAKING = 0
        const val KIND_LOW_SHELF = 1
        const val KIND_HIGH_SHELF = 2

        /** How many curves the native side has. Use this rather than a literal. */
        val presetCount: Int get() = nativePresetCount()

        init {
            System.loadLibrary("dsp")
        }

        @JvmStatic private external fun nativeCreate(sampleRate: Int, channels: Int): Long
        @JvmStatic private external fun nativeDestroy(handle: Long)
        @JvmStatic private external fun nativeApplyPreset(handle: Long, index: Int)
        @JvmStatic private external fun nativePresetCount(): Int
        @JvmStatic private external fun nativeSetBand(
            handle: Long, index: Int, kind: Int, freqHz: Float, gainDb: Float, q: Float
        )
        @JvmStatic private external fun nativeSetOutputGainDb(handle: Long, db: Float)
        @JvmStatic private external fun nativeSetThresholdDb(handle: Long, db: Float)
        @JvmStatic private external fun nativeSetEnabled(handle: Long, enabled: Boolean)
        @JvmStatic private external fun nativeProcess(handle: Long, buf: ShortArray, length: Int)
        @JvmStatic private external fun nativeReductionDb(handle: Long): Float
        @JvmStatic private external fun nativeLatencyFrames(handle: Long): Int
    }
}
