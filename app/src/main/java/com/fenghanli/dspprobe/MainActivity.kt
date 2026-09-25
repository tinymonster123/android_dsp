package com.fenghanli.dspprobe

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import com.fenghanli.dspprobe.dsp.Presets

class MainActivity : Activity() {

    private companion object {
        const val REQ_PERMS = 100
        const val REQ_PROJECTION = 101
    }

    private lateinit var statusText: TextView
    private lateinit var detailText: TextView
    private lateinit var startBtn: Button
    private lateinit var stopBtn: Button
    private lateinit var playbackBtn: Button
    private lateinit var routeBtn: Button
    private lateinit var muteBtn: Button
    private lateinit var presetBtn: Button
    private lateinit var presetInfoText: TextView

    private val ui = Handler(Looper.getMainLooper())
    private val tick = object : Runnable {
        override fun run() {
            render()
            ui.postDelayed(this, 250)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildUi())
    }

    override fun onResume() {
        super.onResume()
        ui.post(tick)
    }

    override fun onPause() {
        super.onPause()
        ui.removeCallbacks(tick)
    }

    private fun dp(v: Int): Int = TypedValue
        .applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics)
        .toInt()

    private fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(24), dp(20), dp(16))
        }

        root.addView(TextView(this).apply {
            text = "DSP Probe"
            textSize = 22f
            setTypeface(typeface, Typeface.BOLD)
        })

        root.addView(TextView(this).apply {
            text = "先用 Apple Music 或 B 站放一首歌，再点开始。"
            textSize = 12f
        })

        statusText = TextView(this).apply {
            textSize = 26f
            setTypeface(typeface, Typeface.BOLD)
            gravity = Gravity.CENTER
            setPadding(0, dp(12), 0, dp(12))
        }
        root.addView(statusText)

        startBtn = Button(this).apply {
            text = "1. 授权并开始抓取"
            setOnClickListener { requestPermissionsThenProjection() }
        }
        root.addView(startBtn, matchWidth())

        // A plain Button rather than a ToggleButton: ToggleButton's textOn/textOff were
        // ignored on this ROM, leaving the label stuck on the default "关闭".
        playbackBtn = Button(this).apply {
            setOnClickListener { service()?.setPlayback(!ProbeState.playing) }
        }
        root.addView(playbackBtn, matchWidth())

        routeBtn = Button(this).apply {
            setOnClickListener { service()?.cycleOutputRoute() }
        }
        root.addView(routeBtn, matchWidth())

        muteBtn = Button(this).apply {
            setOnClickListener { service()?.setMuteOriginal(!ProbeState.muteOriginal) }
        }
        root.addView(muteBtn, matchWidth())

        presetBtn = Button(this).apply {
            setOnClickListener {
                service()?.setPreset((ProbeState.presetIndex + 1) % Presets.count())
            }
        }
        root.addView(presetBtn, matchWidth())

        // Shown under the buttons rather than buried in the readout: during an A/B
        // you need to know what the current curve is *supposed* to sound like.
        presetInfoText = TextView(this).apply {
            textSize = 12f
            gravity = Gravity.CENTER
            setTextColor(Color.parseColor("#555555"))
            setPadding(0, 0, 0, dp(6))
        }
        root.addView(presetInfoText, matchWidth())

        stopBtn = Button(this).apply {
            text = "停止"
            setOnClickListener {
                CaptureService.stop(this@MainActivity)
                ProbeState.playing = false
            }
        }
        root.addView(stopBtn, matchWidth())

        detailText = TextView(this).apply {
            textSize = 10f
            typeface = Typeface.MONOSPACE
            setTextIsSelectable(true)
            setPadding(0, dp(12), 0, 0)
        }
        root.addView(ScrollView(this).apply {
            addView(detailText)
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f)
        })

        return root
    }

    private fun matchWidth(): LinearLayout.LayoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT
    )

    private fun service(): CaptureService? {
        val s = CaptureService.instance
        if (s == null) ProbeState.note = "服务未运行，先点开始抓取"
        return s
    }

    private fun render() {
        startBtn.isEnabled = !ProbeState.running
        stopBtn.isEnabled = ProbeState.running
        playbackBtn.isEnabled = ProbeState.running
        routeBtn.isEnabled = ProbeState.running
        muteBtn.isEnabled = ProbeState.running
        presetBtn.isEnabled = ProbeState.running

        playbackBtn.text =
            if (ProbeState.playing) "2. 回放：开（听有没有回声）" else "2. 回放：关（听原声还在不在）"
        routeBtn.text = "3. 输出：${ProbeState.routeName(ProbeState.outputRoute)}（点一下换一条）"
        muteBtn.text =
            if (ProbeState.muteOriginal) "4. 静音原声：开（只剩我们一路）" else "4. 静音原声：关"
        presetBtn.text = "5. DSP：${ProbeState.presetName}（点一下换）"
        presetInfoText.text = ProbeState.presetIntent

        when (ProbeState.verdict()) {
            ProbeState.Verdict.IDLE -> {
                statusText.text = "空闲"
                statusText.setTextColor(Color.GRAY)
            }
            ProbeState.Verdict.WAITING -> {
                statusText.text = "等待音频…"
                statusText.setTextColor(Color.parseColor("#EF6C00"))
            }
            ProbeState.Verdict.SILENT -> {
                statusText.text = "静音 · 抓不到"
                statusText.setTextColor(Color.parseColor("#C62828"))
            }
            ProbeState.Verdict.SIGNAL -> {
                statusText.text = "有信号 %.0f dBFS".format(ProbeState.rmsDbfs)
                statusText.setTextColor(Color.parseColor("#2E7D32"))
            }
        }

        detailText.text = ProbeState.dump()
    }

    private fun requestPermissionsThenProjection() {
        val needed = mutableListOf(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            needed += Manifest.permission.POST_NOTIFICATIONS
        }
        val missing = needed.filter {
            checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED
        }
        if (missing.isEmpty()) {
            requestProjection()
        } else {
            requestPermissions(missing.toTypedArray(), REQ_PERMS)
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQ_PERMS) return
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            requestProjection()
        } else {
            ProbeState.note = "麦克风权限被拒绝 — 没有它就抓不了音频"
        }
    }

    private fun requestProjection() {
        val mpm = getSystemService(MediaProjectionManager::class.java)
        @Suppress("DEPRECATION")
        startActivityForResult(mpm.createScreenCaptureIntent(), REQ_PROJECTION)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_PROJECTION) return
        if (resultCode == RESULT_OK && data != null) {
            CaptureService.start(this, resultCode, data)
        } else {
            ProbeState.note = "投影授权被取消"
        }
    }
}
