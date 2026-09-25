# android_dsp

给 Android 手机做一个 Boom 2 式的音质增强。当前状态：**可行性已验证，尚未开始写 DSP。**

## 已验证的事实（Honor Magic5 Pro / PGT-AN00 / Android 16 / MagicOS 10）

全部由 `app/` 里的 probe 在真机上实测得出，不是推断：

| 结论 | 证据 |
|---|---|
| Apple Music 的音频**可捕获** | RMS -22 dBFS，48kHz 立体声，19313 个 4ms 块里只有 833 个静音，零读取错误 |
| 额外延迟**可以压到 4.0 ms** | `chunk 4.0ms`，`out lag 0ms`。一个设备 burst（192 帧 / 48kHz） |
| 源 app 的外放**不会被自动静音** | 用户实听：鼓机瞬态重叠、整体浑浊 → 双声 + 梳状滤波 |
| **捕获点在音量缩放之前** | `STREAM_MUSIC vol=0` 时 `verdict` 仍是 SIGNAL，RMS -28 dBFS。**这是整个方案成立的前提** |
| `ALLOW_CAPTURE_BY_NONE` **确实生效** | `out track OK policy=NONE`，无自捕获反馈 |
| 静音媒体流 + 输出走 ALARM 流 = **只剩一路，听感干净** | 用户实听确认 |
| **Root 路线在荣耀上不可行** | 荣耀无官方 BL 解锁（见下） |

### 延迟是怎么从 42.7ms 降到 4.0ms 的

`AudioRecord.read()` 会阻塞到凑满请求的数量才返回。原来一次请求 4096 shorts = 42.7ms，
这 42.7ms 是**我们自己加的延迟**，不是系统限制。改成按设备 burst（192 帧）读取后只剩 4ms。
注意 `read()` 返回的是 **short 数不是帧数**，立体声下除以采样率会放大 2 倍。

### 产品化的四个硬问题（尚未解决）

1. `STREAM_ALARM` 只是**测试落脚点**。它是这台机器上唯一"独立于 STREAM_MUSIC + 未被静音 +
   不在捕获 usage 列表"的流。走它意味着绕过勿扰、独立音量曲线、语义错误。真产品要重新设计。
2. 必须**长期压制 `STREAM_MUSIC` 到 0**，但用户按一次音量键就前功尽弃，双声立刻回来。
3. **抓不到的 app（Spotify / DRM / 部分游戏）会变哑巴** —— 媒体流被静音而它们又不进我们的处理链。
   需要自动检测连续静音并解除静音，或维护排除列表。
4. **MediaProjection 被 MagicOS 主动杀过一次**（`onStop` 回调），稳定性需专门处理。

### 为什么 root 路线在荣耀上不可行

要 root 必须先解 BL，而荣耀从 2018 年起关闭官方通道（[Bootloader 解锁耻辱榜](https://www.appinn.com/bootloader-unlock-list/)、
[Uotan Wiki](https://wiki.uotan.cn/index.php?title=%E8%A7%A3%E9%94%81Bootloader) 均列为"不支持解锁"）。
第三方途径存在但条件苛刻（可能需降级、可能触发熔断变砖、部分要拆机），且无资料证实 PGT-AN00 可行。
**不要在主力机上赌这个。**

## 为什么需要先做 probe

Boom 2 在 macOS 上能"系统级"工作，是因为 macOS 允许用户态安装虚拟音频设备、改 CoreAudio 输出链路。
**Android 没有任何用户可安装的 audio HAL**，所以不存在一条直路，只有三条各有代价的绕路：

| 路线 | 做法 | 覆盖范围 | 代价 |
|---|---|---|---|
| A 自带播放器 | DSP 内嵌在播放管线里 | 只有本 app 播的音频 | 无。但管不到 Apple Music / B 站 |
| B 内录重放 | `AudioPlaybackCapture` → 处理 → `AudioTrack` | 其他 app 的音频 | 屏蔽捕获的 app 抓不到；有额外延迟；需常驻前台服务 |
| C Root | Magisk + AudioFlinger effect | 全部，且零延迟 | 要 root，上不了 Play Store |

目标场景是 **Apple Music + B 站**，属于"其他 app 的音频"，只有 B / C 能覆盖。据公开报告 B 路线是可行的
（Apple Music 可用；Spotify/Chrome/SoundCloud 因 DRM 屏蔽捕获；B 站未被列入屏蔽名单、多机型实测可内录），
但以下三点**必须在本机实测**，因为机型与 ROM 差异极大：

1. 抓取时源 app 是否仍在往外放 → 决定会不会双声（`AudioPlaybackCapture` 的语义是"复制"）
2. 实际额外延迟 → 决定 B 站这类**视频**场景能不能用
3. Apple Music / B 站 在你这台机器上的真实捕获结果

这三点研究不出来，只能跑。所以先造这个抛掉也不心疼的 probe。

## 它做什么

`CaptureService` 用 `AudioPlaybackCapture` 抓 `USAGE_MEDIA|GAME|UNKNOWN` 的音频，
在音频线程上算 RMS/峰值，并可选择把抓到的 PCM **原样**写回 `AudioTrack`。
"原样写回"是刻意的——它让站在手机前的人直接用耳朵判断源音频有没有被静音、有没有双声。

界面只有三个控件，对应三个实验：

- **1. 授权并开始抓取** — 大字状态显示判定结果
- **2. 回放开关** — 直通回放，用来听回声
- **停止**

## 判读表

先开 Apple Music / B 站**播放音乐**，再点开始抓取。

| 观察 | 结论 |
|---|---|
| 大字绿色 `有信号 -xx dBFS` | 抓到了，路线成立 |
| 大字红色 `静音 · 抓不到`，frames 在涨但 silent 接近 100% | 该 app 屏蔽了捕获 → 它在这个方案里用不了 |
| 回放**关**着，你仍然能听到音乐 | 源音频没被静音 → 开回放必然双声 |
| 回放**关**着，音乐停了/明显变小 | 系统把源音频让给了捕获流（最理想） |
| 回放开着，听到回声/加倍/梳状滤波 | 双声确认，需要额外的"静音原声"处理 |
| `added delay` 数值 | 低于 ~50ms 可忽略；超过 ~100ms 视频口型会不同步 |

底部详情区里 `out lag` 由 `AudioTrack.getTimestamp()` 算出，是输出缓冲落后真实时间的量。

## 构建

工具链装在 Homebrew 的 cask 里，不在默认位置：

```sh
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export JAVA_HOME=/Library/Java/JavaVirtualMachines/jdk-21.jdk/Contents/Home
export PATH="$ANDROID_HOME/platform-tools:$PATH"

./gradlew :app:assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

`local.properties` 里的 `sdk.dir` 指向同一个 SDK。

## 已知的坑（都是实际踩过的）

- **AGP 9 内置 Kotlin**，不能再套 `org.jetbrains.kotlin.android` 插件，否则构建脚本直接报错。
- **`AudioTrack.getLatency()` 不是公开 API**，API 36 的 `android.jar` 里根本没有这个方法。
  用 `getTimestamp(AudioTimestamp)` 代替。
- **`AudioTrack` 必须设 `ALLOW_CAPTURE_BY_NONE`**，否则它会被自己捕获，形成无限回声循环
  （RootlessJamesDSP 的 issue #109 就是这个）。
- Android 14+ 要求 **先** `startForeground()` 带 `mediaProjection` 类型，**再** `getMediaProjection()`。
- Android 14+ 必须先 `registerCallback()`，否则 `MediaProjection` 会抛异常。
- `MediaProjection` 没有活跃捕获时可能被回收，所以建了个 16×16 的 `VirtualDisplay` 吊着它。

## 下一步（取决于 probe 结果）

- 抓不到 → 只剩 root 路线（Magisk + AudioFlinger effect），整个项目形态要重估
- 有双声 → 研究"静音原声"的绕法（不能用 `STREAM_MUSIC` 静音，会连自己的输出一起杀掉）
- 延迟过高 → B 站视频场景放弃，只做纯音乐场景
- 都通过 → 进入 `/to-spec` → `/to-tickets` → `/implement`，开始写 Rust DSP 内核

DSP 内核的目标清单（对标 Boom 的各项卖点）：

| 模块 | 原理 | 对标 |
|---|---|---|
| 参量均衡 | biquad IIR，RBJ cookbook 系数 | 31 段 EQ |
| 虚拟低音 | 缺失基频重建（谐波生成） | Bass boost |
| 立体声展宽 | mid/side 处理 | Stereo widening |
| 交叉馈送/HRTF | 串扰 + 头部传递函数 | 3D Surround |
| 压缩 + 限幅 | 提升音量后防削顶 | Volume Boost |
| 响度归一化 | ITU-R BS.1770 / ReplayGain | — |
| 卷积混响 | IR 卷积 | Ambience / Night Mode |
