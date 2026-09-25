package com.fenghanli.dspprobe.dsp

/**
 * Display names for the EQ curves that live in `rust/src/presets.rs`.
 *
 * The curves are deliberately **not** here. They used to be, and the only way to
 * find out what a curve actually did was to listen and guess — which produced a
 * "vocals forward" setting that mostly just made everything thinner. Sitting in
 * Rust, each curve is swept by a test that checks its response against the claim
 * in its comment.
 *
 * This file carries only what the UI needs: a name and a one-line description.
 * [namesMatchNative] guards the two tables against drifting apart.
 */
object Presets {

    data class Info(val name: String, val intent: String)

    /** Order matters — the index is the contract with the native side. */
    val ALL = listOf(
        Info("关", "直通，对照组"),
        Info("突出人声", "中频前倾：1.6k / 3.2k 各 +4dB，低频只微降"),
        Info("突出背景", "V 字型：挖掉 1.2k，低频 +7dB、9k 空气感 +6dB"),
        Info("温和", "均衡微调：去闷、抬清晰度，量都很小"),
        Info("强", "同温和但每项翻倍，峰值高时限幅器会介入"),
    )

    fun count(): Int = ALL.size

    /** False when Kotlin's name table and the Rust curve table have diverged. */
    fun namesMatchNative(): Boolean = runCatching {
        ALL.size == DspEngine.presetCount
    }.getOrDefault(false)

    fun infoAt(index: Int): Info = ALL[index.coerceIn(0, ALL.indices.last)]
}
