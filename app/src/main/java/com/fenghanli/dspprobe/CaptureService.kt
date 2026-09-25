package com.fenghanli.dspprobe

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.util.Log
import com.fenghanli.dspprobe.dsp.DspEngine
import com.fenghanli.dspprobe.dsp.Presets
import com.fenghanli.dspprobe.dsp.VirtualBass
import kotlin.concurrent.thread
import kotlin.math.abs
import kotlin.math.log10
import kotlin.math.sqrt

/**
 * Captures other apps' audio via AudioPlaybackCapture and (optionally) plays it straight back
 * out. The straight-back-out part is the whole point: it lets a human standing in front of the
 * phone hear whether the original audio is still being played by the source app underneath us.
 */
class CaptureService : Service() {

    companion object {
        private const val TAG = "DspProbe"

        /** Marker for "the UI thread has not asked for a change". */
        private const val NO_PENDING = -1

        private const val ACTION_START = "com.fenghanli.dspprobe.START"
        private const val ACTION_STOP = "com.fenghanli.dspprobe.STOP"
        private const val EXTRA_RESULT_CODE = "resultCode"
        private const val EXTRA_RESULT_DATA = "resultData"

        private const val CHANNEL_ID = "dsp-probe"
        private const val NOTIF_ID = 41

        @Volatile
        var instance: CaptureService? = null

        fun start(ctx: Context, resultCode: Int, data: Intent) {
            val i = Intent(ctx, CaptureService::class.java).apply {
                action = ACTION_START
                putExtra(EXTRA_RESULT_CODE, resultCode)
                putExtra(EXTRA_RESULT_DATA, data)
            }
            ctx.startForegroundService(i)
        }

        fun stop(ctx: Context) {
            ctx.startService(Intent(ctx, CaptureService::class.java).apply { action = ACTION_STOP })
        }
    }

    private val sampleRate = 48000

    // Small on purpose. This is a capacity, and any audio sitting in it is latency we added.
    // 4096 bytes = 1024 frames stereo = ~21 ms worst case.
    private val bufferBytes = 4096

    private var projection: MediaProjection? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var record: AudioRecord? = null
    private var track: AudioTrack? = null
    private var dsp: DspEngine? = null

    /**
     * Reused by the audio thread to read the output device's presentation clock.
     * AudioTrack#getLatency is not public API; getTimestamp is the supported way to learn
     * how far behind real time our output actually is.
     */
    private val outputTimestamp = AudioTimestamp()

    private var savedMusicVolume = -1

    /**
     * Original volume per output route, -1 meaning "not captured".
     *
     * Fixed-size and allocated up front because route changes can be realised on
     * the audio thread, where allocating is not allowed.
     */
    private val savedRouteVolume = IntArray(3) { -1 }

    @Volatile
    private var loopRunning = false
    private var worker: Thread? = null
    private var tearingDown = false

    /**
     * Requests from the UI thread, realised by the audio thread.
     *
     * These exist because the track used to be paused, released and rebuilt from
     * the UI thread while the capture thread was midway through
     * `AudioTrack.write`. That is a use-after-release: the write throws on a
     * non-main thread and takes the whole process down. Widening the window
     * (by calling into the DSP between taking the track and writing to it) is
     * what turned it from rare into reliable.
     *
     * Handing ownership to the one thread that actually uses the track removes
     * the race rather than narrowing it. The cost is that rebuilding an
     * AudioTrack allocates on the audio thread, once per button press — a
     * one-off glitch on an explicit user action, which is the right trade
     * against an intermittently fatal race.
     */
    @Volatile
    private var pendingRoute: Int = NO_PENDING

    @Volatile
    private var pendingPlayback: Int = NO_PENDING

    private val handler = Handler(Looper.getMainLooper())

    private val projectionCallback = object : MediaProjection.Callback() {
        override fun onStop() {
            ProbeState.note = "MediaProjection 被系统停止（常见于切换应用/超时）"
            teardown()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> teardown()
            ACTION_START -> {
                startAsForeground()
                val code = intent.getIntExtra(EXTRA_RESULT_CODE, Activity.RESULT_CANCELED)
                val data = resultData(intent)
                if (data == null) {
                    ProbeState.note = "没有拿到投影授权数据"
                    teardown()
                } else {
                    begin(code, data)
                }
            }
        }
        return START_NOT_STICKY
    }

    @Suppress("DEPRECATION")
    private fun resultData(intent: Intent): Intent? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
        } else {
            intent.getParcelableExtra(EXTRA_RESULT_DATA)
        }

    /**
     * Android 14+ requires the foreground service to already be running with the
     * mediaProjection type *before* we ask for a MediaProjection, so this goes first.
     */
    private fun startAsForeground() {
        val nm = getSystemService(NotificationManager::class.java)
        if (nm.getNotificationChannel(CHANNEL_ID) == null) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "DSP Probe", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val notif = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("DSP Probe 运行中")
            .setContentText("正在抓取系统播放的音频")
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setOngoing(true)
            .build()

        val type = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION or
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        startForeground(NOTIF_ID, notif, type)
    }

    private fun begin(resultCode: Int, data: Intent) {
        val mpm = getSystemService(MediaProjectionManager::class.java)
        val proj = mpm.getMediaProjection(resultCode, data)
        if (proj == null) {
            ProbeState.note = "getMediaProjection 返回 null"
            teardown()
            return
        }
        projection = proj
        proj.registerCallback(projectionCallback, handler)

        // A projection with no active capture can be reaped. We do not need pixels, so mirror
        // into nothing at a token size just to keep the session legitimate.
        try {
            virtualDisplay = proj.createVirtualDisplay(
                "dsp-probe", 16, 16, 1,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                null, null, null
            )
        } catch (t: Throwable) {
            ProbeState.note = "createVirtualDisplay 失败: ${t.javaClass.simpleName}"
        }

        ProbeState.deviceInfo =
            "${Build.MANUFACTURER} ${Build.MODEL} / Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})"

        val am = getSystemService(AudioManager::class.java)
        ProbeState.nativeSampleRate =
            am.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE)?.toIntOrNull() ?: 0
        ProbeState.nativeFramesPerBuffer =
            am.getProperty(AudioManager.PROPERTY_OUTPUT_FRAMES_PER_BUFFER)?.toIntOrNull() ?: 0
        if (ProbeState.nativeSampleRate > 0) {
            ProbeState.nativeBurstMs =
                ProbeState.nativeFramesPerBuffer * 1000f / ProbeState.nativeSampleRate
        }
        ProbeState.musicVolume = am.getStreamVolume(AudioManager.STREAM_MUSIC)
        ProbeState.routeVolume = am.getStreamVolume(streamForRoute(ProbeState.outputRoute))

        if (!openInput(proj)) return
        openOutput()
        startLoop()
    }

    private fun openInput(proj: MediaProjection): Boolean {
        val config = AudioPlaybackCaptureConfiguration.Builder(proj)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .build()

        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(sampleRate)
            .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
            .build()

        val rec = try {
            AudioRecord.Builder()
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferBytes)
                .setAudioPlaybackCaptureConfig(config)
                .build()
        } catch (t: Throwable) {
            ProbeState.note = "AudioRecord 构造失败: ${t.javaClass.simpleName} ${t.message}"
            teardown()
            return false
        }

        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            ProbeState.note = "AudioRecord 未初始化 (state=${rec.state})"
            rec.release()
            teardown()
            return false
        }

        record = rec
        // The system may hand back a different format than we asked for; report what we got.
        ProbeState.captureSampleRate = rec.sampleRate
        ProbeState.captureChannels = rec.channelCount

        // The DSP core is tied to the capture format rather than the output
        // track, so it is rebuilt here whenever a fresh record is opened.
        dsp?.close()
        val engine = DspEngine(rec.sampleRate, rec.channelCount)
        dsp = engine
        if (engine.isValid) {
            applyPreset(engine, ProbeState.presetIndex, rec.sampleRate)
            engine.setVirtualBassLevel(ProbeState.virtualBassLevel)
            // The curve tables live in Rust and the name tables in Kotlin; this is
            // the only place both are reachable at once.
            if (!Presets.namesMatchNative()) {
                ProbeState.note =
                    "预设表不一致：Kotlin ${Presets.count()} 条 vs native ${DspEngine.presetCount} 条"
            }
            if (!VirtualBass.namesMatchNative()) {
                ProbeState.note =
                    "虚拟低音表不一致：Kotlin ${VirtualBass.count()} 档 vs native " +
                        "${DspEngine.virtualBassLevelCount} 档"
            }
        } else {
            ProbeState.note = "DSP 引擎创建失败（libdsp.so 没加载上？）"
        }
        return true
    }

    /**
     * Hands the curve index to the native engine and publishes what the UI shows.
     *
     * Index 0 is a true bypass on the Rust side — no EQ *and* no limiter latency —
     * so an A/B against it compares the DSP to nothing at all, rather than to a
     * re-levelled signal.
     */
    private fun applyPreset(engine: DspEngine, index: Int, sampleRate: Int) {
        val idx = index.coerceIn(0, Presets.count() - 1)
        engine.applyPreset(idx)
        val info = Presets.infoAt(idx)
        ProbeState.presetIndex = idx
        ProbeState.presetName = info.name
        ProbeState.presetIntent = info.intent
        ProbeState.dspLatencyMs = if (idx == 0) 0f else engine.latencyFrames * 1000f / sampleRate
        ProbeState.dspReductionDb = 0f
    }

    /** Called from the UI thread. */
    fun setPreset(index: Int) {
        val idx = index.coerceIn(0, Presets.count() - 1)
        val engine = dsp
        if (engine == null) {
            // No capture running, so no engine and no sample rate. Record the
            // choice; openInput() will apply it when a record is opened.
            val info = Presets.infoAt(idx)
            ProbeState.presetIndex = idx
            ProbeState.presetName = info.name
            ProbeState.presetIntent = info.intent
            return
        }
        applyPreset(engine, idx, record?.sampleRate ?: 48000)
        ProbeState.note = "DSP 预设：${Presets.infoAt(idx).name}"
    }

    /**
     * Called from the UI thread.
     *
     * Deliberately orthogonal to the EQ preset: the harmonic layer and the curve
     * answer different questions, and wanting more low end should not mean giving
     * up the curve that was just chosen. The two only interact at preset 0, which
     * is a true bypass of the whole chain including this module.
     */
    fun setVirtualBassLevel(index: Int) {
        val idx = index.coerceIn(0, VirtualBass.count() - 1)
        val info = VirtualBass.infoAt(idx)
        ProbeState.virtualBassLevel = idx
        ProbeState.virtualBassName = info.name
        ProbeState.virtualBassIntent = info.intent
        dsp?.setVirtualBassLevel(idx)
        ProbeState.note = "虚拟低音：${info.name}"
    }

    /**
     * [ProbeState.outputRoute] picks the sink.
     *
     * USAGE_MEDIA lands on STREAM_MUSIC — the same stream the source app plays on, and one
     * of the usages we capture, so our own output feeds straight back into the capture.
     * USAGE_ALARM lands on STREAM_ALARM: independent of STREAM_MUSIC, so muting the media
     * stream does not silence us, and not in the captured usage set, so we cannot hear
     * ourselves. USAGE_ASSISTANCE_SONIFICATION (STREAM_SYSTEM) is the same idea but this
     * device ships with that stream at volume 0.
     */
    private fun openOutput() {
        val rec = record ?: return
        val route = ProbeState.outputRoute
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(rec.sampleRate)
            .setChannelMask(
                if (rec.channelCount >= 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
            )
            .build()

        val usage = when (route) {
            1 -> AudioAttributes.USAGE_ALARM
            2 -> AudioAttributes.USAGE_ASSISTANCE_SONIFICATION
            else -> AudioAttributes.USAGE_MEDIA
        }
        val contentType = if (route == 0) {
            AudioAttributes.CONTENT_TYPE_MUSIC
        } else {
            AudioAttributes.CONTENT_TYPE_SONIFICATION
        }

        val attrs = AudioAttributes.Builder()
            .setUsage(usage)
            .setContentType(contentType)
            // Without this our own output is capturable and we would feed ourselves in a
            // loop, which sounds like an endless echo rather than music.
            .setAllowedCapturePolicy(AudioAttributes.ALLOW_CAPTURE_BY_NONE)
            .build()

        val t = try {
            AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferBytes * 2)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .build()
        } catch (e: Throwable) {
            ProbeState.note = "AudioTrack 构造抛异常: ${e.javaClass.simpleName} ${e.message}"
            null
        }

        // Builder returning is not the same as the track being usable.
        if (t != null && t.state != AudioTrack.STATE_INITIALIZED) {
            ProbeState.note = "AudioTrack 未初始化 state=${t.state} (${ProbeState.routeName(route)})"
            t.release()
            track = null
            ProbeState.hasTrack = false
            ProbeState.outputPolicy = -1
            return
        }

        track = t
        ProbeState.hasTrack = t != null
        ProbeState.outputPolicy = t?.audioAttributes?.allowedCapturePolicy ?: -1
        preferBluetoothSink(t)
    }

    /**
     * Asks the track to prefer the Bluetooth sink.
     *
     * Stream routing policy decides which streams *may* play; this decides which
     * device *this track* goes to. On this ROM `USAGE_ALARM` is routed to the
     * phone speaker and A2DP at once — which makes headphone listening impossible —
     * and setPreferredDevice is the supported way to say "this track, that device".
     * It is a request, not a guarantee, which is why the actual result is read
     * back from the track and shown in the readout.
     */
    private fun preferBluetoothSink(t: AudioTrack?) {
        if (t == null) {
            ProbeState.preferredDevice = "-"
            return
        }
        val am = getSystemService(AudioManager::class.java)
        val bt = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            .firstOrNull { it.type == AudioDeviceInfo.TYPE_BLUETOOTH_A2DP }
        if (bt == null) {
            ProbeState.preferredDevice = "(没有蓝牙输出设备)"
            return
        }
        // Called as a method, not via the property: the property setter throws
        // away setPreferredDevice's boolean, and that boolean is the whole answer.
        @Suppress("UsePropertyAccessSyntax")
        val accepted = try {
            t.setPreferredDevice(bt)
        } catch (e: Throwable) {
            ProbeState.preferredDevice = "异常 ${e.javaClass.simpleName}"
            return
        }
        ProbeState.preferredDevice =
            if (accepted) "→ ${bt.productName}" else "被拒绝 (${bt.productName})"
    }

    /** Human-readable name for an [AudioDeviceInfo] type. */
    private fun deviceTypeName(type: Int): String = when (type) {
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "speaker"
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> "earpiece"
        AudioDeviceInfo.TYPE_BLUETOOTH_A2DP -> "bt_a2dp"
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO -> "bt_sco"
        AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> "wired"
        AudioDeviceInfo.TYPE_USB_HEADSET -> "usb"
        else -> "type$type"
    }

    private fun releaseTrack() {
        try {
            track?.pause()
            track?.flush()
        } catch (_: Throwable) {
        }
        track?.release()
        track = null
        ProbeState.hasTrack = false
        ProbeState.outputPolicy = -1
    }

    /** Called from the UI thread. Only records the intent; see [pendingPlayback]. */
    fun setPlayback(on: Boolean) {
        ProbeState.playing = on
        if (loopRunning) {
            pendingPlayback = if (on) 1 else 0
        } else {
            // No capture thread to race with.
            realizePlayback(on)
        }
    }

    /** Called from the UI thread. Only records the intent; see [pendingRoute]. */
    fun cycleOutputRoute() {
        val next = (ProbeState.outputRoute + 1) % 3
        ProbeState.outputRoute = next
        // Volume bookkeeping is a binder call, so it stays on this thread: the
        // audio thread must not block or allocate.
        ensureRouteAudible(next)
        if (loopRunning) {
            pendingRoute = next
        } else {
            realizeRoute(next)
        }
    }

    /** Which system stream a route's output lands on. */
    private fun streamForRoute(route: Int): Int = when (route) {
        1 -> AudioManager.STREAM_ALARM
        2 -> AudioManager.STREAM_SYSTEM
        else -> AudioManager.STREAM_MUSIC
    }

    /**
     * Makes sure the stream our output lands on is actually audible.
     *
     * The threshold is deliberately not zero. Our output track is the active one
     * on its stream, so the hardware volume keys adjust *that* stream — a user
     * reaching for "the volume" silently turns our own output down to 1 and then
     * reports "no sound". Treating anything below a third of full scale as a
     * silent route turns a confusing dead end into a self-correcting one.
     */
    private fun ensureRouteAudible(route: Int) {
        val stream = streamForRoute(route)
        val am = getSystemService(AudioManager::class.java)
        val current = am.getStreamVolume(stream)
        val max = am.getStreamMaxVolume(stream)
        ProbeState.routeVolume = current

        val floor = (max * 0.3f).toInt().coerceAtLeast(1)
        if (current >= floor) return

        if (savedRouteVolume[route] < 0) savedRouteVolume[route] = current
        val target = (max * 0.7f).toInt().coerceAtLeast(1)
        try {
            am.setStreamVolume(stream, target, 0)
            ProbeState.routeVolume = am.getStreamVolume(stream)
            ProbeState.note =
                "${ProbeState.routeName(route)} 音量 $current/$max 太低，已提到 ${ProbeState.routeVolume}"
        } catch (e: Throwable) {
            ProbeState.note = "提 ${ProbeState.routeName(route)} 音量被拒: ${e.javaClass.simpleName}"
        }
    }

    /** Puts back every stream volume this service moved. */
    private fun restoreRouteVolumes() {
        for (route in savedRouteVolume.indices) {
            val original = savedRouteVolume[route]
            if (original < 0) continue
            try {
                getSystemService(AudioManager::class.java)
                    .setStreamVolume(streamForRoute(route), original, 0)
            } catch (_: Throwable) {
            }
            savedRouteVolume[route] = -1
        }
    }

    /** Audio thread (or a stopped service): actually start/stop the output track. */
    private fun realizePlayback(on: Boolean) {
        val t = track ?: return
        try {
            if (on) {
                if (t.playState != AudioTrack.PLAYSTATE_PLAYING) t.play()
            } else {
                if (t.playState == AudioTrack.PLAYSTATE_PLAYING) t.pause()
                t.flush()
            }
        } catch (e: Throwable) {
            ProbeState.note = "切换回放失败: ${e.javaClass.simpleName}"
        }
    }

    /** Audio thread (or a stopped service): rebuild the output track on a new route. */
    private fun realizeRoute(route: Int) {
        val wasPlaying = ProbeState.playing
        releaseTrack()
        // Blank the note so openOutput's failure message, if any, is not lost.
        ProbeState.note = ""
        openOutput()
        val failure = ProbeState.note
        if (failure.isNotEmpty()) {
            ProbeState.note = failure
            return
        }
        if (wasPlaying) realizePlayback(true)
        ProbeState.note = "输出已切到 ${ProbeState.routeName(route)}"
    }

    /**
     * Applies anything the UI thread asked for. Runs at the top of every capture
     * block, before the track is touched, so the track is never swapped out from
     * under an in-flight write.
     */
    private fun applyPendingControls() {
        val route = pendingRoute
        if (route != NO_PENDING) {
            pendingRoute = NO_PENDING
            realizeRoute(route)
        }
        val playback = pendingPlayback
        if (playback != NO_PENDING) {
            pendingPlayback = NO_PENDING
            realizePlayback(playback == 1)
        }
    }

    /**
     * Mute (or restore) STREAM_MUSIC, i.e. the source app's own output.
     *
     * Two things are being tested at once: whether a normal app is allowed to move this
     * stream's volume at all, and — if it is — whether the capture tap sits before or after
     * volume scaling. If the capture goes silent too, this whole approach is a dead end.
     */
    fun setMuteOriginal(on: Boolean) {
        val am = getSystemService(AudioManager::class.java)
        try {
            if (on) {
                val cur = am.getStreamVolume(AudioManager.STREAM_MUSIC)
                if (cur > 0) savedMusicVolume = cur
                am.setStreamVolume(AudioManager.STREAM_MUSIC, 0, 0)
                ProbeState.muteOriginal = true
            } else {
                am.setStreamVolume(
                    AudioManager.STREAM_MUSIC,
                    if (savedMusicVolume > 0) savedMusicVolume else 7,
                    0
                )
                ProbeState.muteOriginal = false
            }
            ProbeState.musicVolume = am.getStreamVolume(AudioManager.STREAM_MUSIC)
            ProbeState.note = "STREAM_MUSIC 现在 vol=${ProbeState.musicVolume}"
        } catch (t: Throwable) {
            ProbeState.note = "改音量被拒: ${t.javaClass.simpleName} ${t.message}"
        }
    }

    private fun startLoop() {
        if (loopRunning) return
        val rec = record ?: return
        loopRunning = true
        ProbeState.reset()
        ProbeState.running = true

        try {
            rec.startRecording()
        } catch (t: Throwable) {
            ProbeState.note = "startRecording 失败: ${t.javaClass.simpleName}"
            teardown()
            return
        }

        // Read one device burst at a time. read() blocks until it has the full request, so
        // asking for 40 ms of audio means 40 ms of added delay all by itself.
        val framesPerRead = if (ProbeState.nativeFramesPerBuffer > 0) {
            ProbeState.nativeFramesPerBuffer
        } else {
            rec.sampleRate / 250 // ~4 ms
        }

        worker = thread(name = "dsp-capture") {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            val buf = ShortArray(framesPerRead * rec.channelCount)
            var frames = 0L
            var chunks = 0L
            var silent = 0L
            var iteration = 0

            while (loopRunning) {
                // Before anything else: the audio thread owns the track, so this
                // is the only place it may be swapped.
                applyPendingControls()

                val n = rec.read(buf, 0, buf.size)
                if (n <= 0) {
                    ProbeState.readErrors++
                    ProbeState.lastReadError = n
                    // DEAD_OBJECT / INVALID_OPERATION mean the record is gone; stop rather than spin.
                    if (n == AudioRecord.ERROR_DEAD_OBJECT || n == AudioRecord.ERROR_INVALID_OPERATION) break
                    continue
                }

                chunks++
                // read() returns SHORTS, not frames. Dividing by sample rate alone would
                // overstate the chunk duration by the channel count.
                val chunkFrames = n / rec.channelCount
                frames += chunkFrames

                var sumSq = 0.0
                var peak = 0
                for (i in 0 until n) {
                    val v = buf[i].toInt()
                    sumSq += v.toDouble() * v
                    val a = abs(v)
                    if (a > peak) peak = a
                }
                val rms = sqrt(sumSq / n) / 32768.0
                val rmsDb = 20 * log10(rms + 1e-12)
                val peakDb = 20 * log10(peak / 32768.0 + 1e-12)
                if (peakDb < -80.0) silent++

                ProbeState.framesCaptured = frames
                ProbeState.totalChunks = chunks
                ProbeState.silentChunks = silent
                ProbeState.rmsDbfs = rmsDb.toFloat()
                ProbeState.peakDbfs = peakDb.toFloat()
                ProbeState.chunkMs = chunkFrames * 1000f / rec.sampleRate

                // Roughly every two seconds, put the numbers somewhere readable without
                // looking at the phone — the probe has to stay measurable while the app
                // under test (Apple Music, Bilibili) is in the foreground.
                if (iteration % 500 == 0) {
                    Log.i(
                        TAG,
                        "frames=$frames chunks=$chunks silent=$silent " +
                            "rms=%.1f peak=%.1f dBFS playing=${ProbeState.playing} " +
                            "chunk=%.1fms outLag=${ProbeState.trackLatencyMs}ms " +
                            "policy=${ProbeState.outputPolicy} vol=${ProbeState.musicVolume} " +
                            "err=${ProbeState.readErrors}".format(rmsDb, peakDb, ProbeState.chunkMs)
                    )
                }

                val t = track
                if (t != null && ProbeState.playing) {
                    if (t.playState != AudioTrack.PLAYSTATE_PLAYING) t.play()
                    // Process in place, then hand the very same array to the track:
                    // no allocation, no second copy, on the audio thread.
                    dsp?.process(buf, n)
                    val written = t.write(buf, 0, n)
                    if (written < 0) ProbeState.trackUnderruns++
                    if (iteration % 20 == 0) {
                        ProbeState.dspReductionDb = dsp?.reductionDb ?: 0f
                        // Read back where the track actually went; setPreferredDevice
                        // is only a request, so this is the real answer.
                        ProbeState.routedDevice =
                            t.routedDevice?.let { deviceTypeName(it.type) } ?: "-"
                        if (t.getTimestamp(outputTimestamp)) {
                            // nanoTime is on the same monotonic clock as System.nanoTime(),
                            // so this is how long ago the presented frame reached the device.
                            ProbeState.trackLatencyMs =
                                ((System.nanoTime() - outputTimestamp.nanoTime) / 1_000_000L).toInt()
                        }
                    }
                }
                iteration++
            }
        }
    }

    private fun teardown() {
        if (tearingDown) return
        tearingDown = true

        loopRunning = false
        ProbeState.running = false
        ProbeState.playing = false

        // Never leave the user's phone with a stream left muted or cranked.
        if (ProbeState.muteOriginal) {
            try {
                val am = getSystemService(AudioManager::class.java)
                am.setStreamVolume(
                    AudioManager.STREAM_MUSIC,
                    if (savedMusicVolume > 0) savedMusicVolume else 7,
                    0
                )
                ProbeState.musicVolume = am.getStreamVolume(AudioManager.STREAM_MUSIC)
            } catch (_: Throwable) {
            }
            ProbeState.muteOriginal = false
        }
        restoreRouteVolumes()

        // Stopping the record unblocks a read() that the worker is parked in, so this must
        // happen before we join it.
        try {
            record?.stop()
        } catch (_: Throwable) {
        }
        try {
            worker?.join(1000)
        } catch (_: InterruptedException) {
        }
        worker = null

        releaseTrack()

        dsp?.close()
        dsp = null
        ProbeState.dspReductionDb = 0f

        record?.release()
        record = null

        try {
            virtualDisplay?.release()
        } catch (_: Throwable) {
        }
        virtualDisplay = null

        try {
            projection?.unregisterCallback(projectionCallback)
        } catch (_: Throwable) {
        }
        try {
            projection?.stop()
        } catch (_: Throwable) {
        }
        projection = null

        try {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } catch (_: Throwable) {
        }
        stopSelf()
    }

    override fun onDestroy() {
        instance = null
        teardown()
        super.onDestroy()
    }
}
