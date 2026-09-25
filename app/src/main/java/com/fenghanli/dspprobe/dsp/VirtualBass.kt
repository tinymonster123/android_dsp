package com.fenghanli.dspprobe.dsp

/**
 * Display names for the virtual-bass steps implemented in `rust/src/virtual_bass.rs`.
 *
 * The step *values* are not here, and neither is the mechanism. All of that is on
 * the Rust side with the DSP, where a test can sweep a tone through the engine and
 * check the claim each step's description makes. This file carries a name and a
 * one-line intent, which is all the UI needs.
 *
 * [namesMatchNative] guards the two tables against drifting apart, exactly as the
 * EQ preset table does.
 */
object VirtualBass {

    data class Info(val name: String, val intent: String)

    /** Order matters — the index is the contract with the native side. */
    val ALL = listOf(
        Info("关", "直通：喇叭放不出的低频就是不放出来"),
        Info("弱", "谐波最少，最不容易听出加工痕迹"),
        Info("中", "把 40–120Hz 的能量搬到 200–1800Hz，也就是喇叭真正推得动的地方"),
        Info("强", "谐波最多，低频最足；峰值高时限幅器会更用力，整体会稍微变小声"),
    )

    fun count(): Int = ALL.size

    /** False when Kotlin's name table and the Rust step table have diverged. */
    fun namesMatchNative(): Boolean = runCatching {
        ALL.size == DspEngine.virtualBassLevelCount
    }.getOrDefault(false)

    fun infoAt(index: Int): Info = ALL[index.coerceIn(0, ALL.indices.last)]
}
