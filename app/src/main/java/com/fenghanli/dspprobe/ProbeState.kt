package com.fenghanli.dspprobe

/**
 * Single source of truth shared between the capture service (writer, on the audio thread)
 * and the activity (reader, polling on the UI thread).
 *
 * Deliberately a plain singleton with @Volatile scalars: no binder, no Flow, no AndroidX.
 * The probe must be trivial to reason about — every field here exists to answer one question.
 */
object ProbeState {

    /** True while the capture loop is alive and holding an AudioRecord. */
    @Volatile var running = false

    /** Whether the user asked for the captured audio to be played back (bypass mode). */
    @Volatile var playing = false

    /** Which sink our output goes to: 0=STREAM_MUSIC, 1=STREAM_ALARM, 2=STREAM_SYSTEM. */
    @Volatile var outputRoute = 0

    /** Whether the output AudioTrack actually reached STATE_INITIALIZED. */
    @Volatile var hasTrack = false

    fun routeName(route: Int): String = when (route) {
        1 -> "STREAM_ALARM"
        2 -> "STREAM_SYSTEM"
        else -> "STREAM_MUSIC"
    }

    /** Whether we asked the system to mute STREAM_MUSIC, i.e. the source app's own output. */
    @Volatile var muteOriginal = false

    @Volatile var framesCaptured = 0L
    @Volatile var rmsDbfs = -120f
    @Volatile var peakDbfs = -120f

    /** Chunks we read that were essentially digital silence — the signature of a blocked capture. */
    @Volatile var silentChunks = 0L
    @Volatile var totalChunks = 0L

    /** What the AudioRecord actually gave us, which may differ from what we asked for. */
    @Volatile var captureSampleRate = 0
    @Volatile var captureChannels = 0

    /** What the device's own mixer runs at, for comparison. */
    @Volatile var nativeSampleRate = 0
    @Volatile var nativeFramesPerBuffer = 0
    @Volatile var nativeBurstMs = 0f

    @Volatile var readErrors = 0L
    @Volatile var lastReadError = 0

    /** Output side, only meaningful once playback has been switched on. */
    @Volatile var trackLatencyMs = 0
    @Volatile var trackUnderruns = 0

    /** Read back from the live AudioTrack: 1=ALL, 2=SYSTEM, 3=NONE. -1 when no track. */
    @Volatile var outputPolicy = -1

    /** STREAM_MUSIC volume as the system reports it right now. */
    @Volatile var musicVolume = -1

    /** Duration of one capture chunk, i.e. the pipeline's base quantum. */
    @Volatile var chunkMs = 0f

    /** Total latency we are adding on top of what the source app already had. */
    val addedLatencyMsLo: Float get() = chunkMs
    val addedLatencyMsHi: Float get() = chunkMs + trackLatencyMs

    /** Free-form line for ad-hoc diagnostics. */
    @Volatile var note = ""

    @Volatile var deviceInfo = ""

    fun reset() {
        framesCaptured = 0L
        rmsDbfs = -120f
        peakDbfs = -120f
        silentChunks = 0L
        totalChunks = 0L
        readErrors = 0L
        lastReadError = 0
        trackUnderruns = 0
        outputPolicy = -1
        note = ""
    }

    /** The verdict, in one word. This is the thing the whole probe exists to produce. */
    enum class Verdict { IDLE, WAITING, SIGNAL, SILENT }

    fun verdict(): Verdict = when {
        !running -> Verdict.IDLE
        framesCaptured == 0L -> Verdict.WAITING
        // If essentially every chunk was digital silence, the source app is blocking us.
        totalChunks > 8 && silentChunks * 100 / totalChunks > 95 -> Verdict.SILENT
        else -> Verdict.SIGNAL
    }

    private fun policyName(p: Int): String = when (p) {
        1 -> "ALL"
        2 -> "SYSTEM"
        3 -> "NONE"
        else -> "-"
    }

    fun dump(): String = buildString {
        appendLine("device      $deviceInfo")
        appendLine()
        appendLine("verdict     ${verdict()}")
        appendLine("frames      $framesCaptured  (chunks $totalChunks, silent $silentChunks)")
        appendLine("rms/peak    %.1f / %.1f dBFS".format(rmsDbfs, peakDbfs))
        appendLine()
        appendLine("capture     ${captureSampleRate}Hz x${captureChannels}ch")
        appendLine("device mix  ${nativeSampleRate}Hz, ${nativeFramesPerBuffer} frames/burst (%.1f ms)".format(nativeBurstMs))
        appendLine("chunk       %.1f ms   <- our read granularity".format(chunkMs))
        appendLine()
        appendLine("playback    ${if (playing) "ON" else "OFF"}  → ${routeName(outputRoute)}")
        appendLine("out track   ${if (hasTrack) "OK" else "MISSING"}   policy=${policyName(outputPolicy)} (1=ALL 2=SYSTEM 3=NONE)")
        appendLine("out lag     ${trackLatencyMs} ms  (AudioTrack getTimestamp)")
        appendLine("added delay %.1f - %.1f ms".format(addedLatencyMsLo, addedLatencyMsHi))
        appendLine("underruns   $trackUnderruns")
        appendLine()
        appendLine("mute orig   ${if (muteOriginal) "ON" else "OFF"}   STREAM_MUSIC vol=$musicVolume")
        appendLine("read errors $readErrors (last $lastReadError)")
        if (note.isNotEmpty()) appendLine("note        $note")
    }
}
