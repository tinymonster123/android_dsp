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
经过 Rust DSP，再写回 `AudioTrack`。整条链现在是可以出声的完整方案：

```
AudioPlaybackCapture ──► Engine::process_i16 ──► AudioTrack(STREAM_ALARM)
                          ├─ 10 段 biquad EQ（RBJ cookbook）
                          ├─ 输出增益（一阶平滑，避免 zipper noise）
                          └─ 2ms 前瞻限幅器 + 软削波
```

六个控件：

| # | 控件 | 作用 |
|---|---|---|
| 1 | 授权并开始抓取 | 申请 MediaProjection + RECORD_AUDIO，起前台服务 |
| 2 | 回放 | 把处理后的音频送出去 |
| 3 | 输出 | 在 `STREAM_MUSIC` / `STREAM_ALARM` / `STREAM_SYSTEM` 之间循环 |
| 4 | 静音原声 | 把 `STREAM_MUSIC` 压到 0，掐掉源 app 的外放 |
| 5 | DSP | 在 5 条曲线之间循环，按钮下方显示当前曲线的意图 |
| — | 停止 | 结束并**强制恢复媒体音量**，不留后患 |

大字状态：绿色 `有信号 -xx dBFS` = 抓到了；红色 `静音 · 抓不到` = 该 app 屏蔽了捕获。

## 为什么输出必须走 STREAM_ALARM

`AudioPlaybackCapture` 的语义是**复制**——源 app 照常播到扬声器，我们额外拿到一份。
不处理就是双声 + 梳状滤波（实测：43ms 时鼓点明显错开；4ms 时变成发闷、鼓点被增强）。

解法是掐掉源外放。**捕获点在音量缩放之前**，所以把 `STREAM_MUSIC` 压到 0 只影响外放、
不影响我们拿到的数据（实测 `vol=0` 时捕获仍是 −28 dBFS，`verdict` 仍是 SIGNAL）。

但这样我们自己的输出也会被一起静音——它默认也在 `STREAM_MUSIC` 上。所以输出改走
`STREAM_ALARM`：它是这台机器上唯一「独立于 `STREAM_MUSIC` + 未被静音 + 不在捕获
usage 列表里」的流。**这是测试落脚点，不是产品方案**（见上文「产品化的四个硬问题」）。

## DSP 内核

Rust 写的实时音频处理，通过 JNI 挂在抓取和重放之间。**曲线定义、滤波器、限幅器全在 Rust**，
Kotlin 只保留 UI 需要的名字和说明——把它们放在 DSP 旁边，是为了能用测试量出真实频响，
而不是在注释里声称。

### RT 安全的做法

- **音频路径上零分配、零锁。** 参数存在原子量里（UI 线程写、音频线程读），系数只在版本号
  变化时重算，且重算时保留滤波器状态——否则每次调参都会爆一声。
- **`nativeProcess` 带长度参数。** `AudioRecord.read()` 返回的是实际读到的数量，缓冲区通常
  比里面的音频长。不带长度就会把陈旧样本也送进处理链；而在音频线程上 `copyOf` 分配内存
  同样不可接受，所以只有这一个办法。
- **`panic = "abort"`。** 跨 FFI 边界 unwind 是未定义行为，音频插件宁可确定性崩溃。
- **`AudioTrack` 由音频线程独占。** 详见「已知的坑」里那条竞态。

### 频响（实测，不是声称）

```sh
cargo test --manifest-path rust/Cargo.toml print_response_table -- --nocapture
```

| Hz | 关 | 突出人声 | 突出背景 | 温和 | 强 |
|---|---|---|---|---|---|
| 60 | 0.0 | −2.9 | **+6.0** | +2.6 | +6.0 |
| 250 | 0.0 | −2.8 | +0.5 | −0.4 | −1.4 |
| 1000 | 0.0 | +2.6 | **−6.0** | +0.1 | +0.1 |
| 1600 | 0.0 | **+5.3** | **−4.8** | +1.0 | +2.4 |
| 2500 | 0.0 | **+5.8** | −0.8 | +2.0 | +5.1 |
| 14000 | 0.0 | +1.9 | +5.1 | +1.8 | +4.6 |

**所有预设的输出增益统一为 0 dB。** 早先的版本一条 +2 dB、一条 +5 dB，结果唯一能听出来的
只有音量差——更响几乎自动被读成"更好"，把真正要测的音色变化盖掉了。这也让"温和 vs 强"
这种同形状只差程度的对比变得几乎无意义。

### 测试抓到的真 bug

1. **立体声共用滤波器状态。** 每帧用同一条 biquad 链处理左右声道时，滤波器看到的是
   96 kHz 的 L,R,L,R 交错流，按 48 kHz 设计的 1000 Hz 中心频率实际落在 500 Hz 上——
   整个 EQ 失谐（+12 dB 的提升只测到 +4 dB）。`stereo_channels_are_independent` 锁死它。
2. **限幅器延迟差一个采样。** 延迟线 `la` 个槽位时最老的可用样本是 `input[i-la+1]`，
   实际延迟是 `la-1`，而 `latency_frames()` 报的是 `la`。听不出来，但报了就该报准。
3. **参数变化重置滤波器状态**导致爆音——改为 `replace_coefficients`：只换系数，保留 z1/z2。

这三个都不是调出来的，是测试逼出来的。

### 已知限制

- **静态 EQ 无法分离人声和伴奏。** 它们在同一频段，滤波器不知道哪个是哪个。"突出人声"
  的真实含义是"中频前倾"。要真正让人声压过伴奏需要动态处理（1–4 kHz 的向上压缩或动态
  EQ），那是下一个模块，不是调参数能解决的。
- **没有响度归一化。** 源素材峰值约 −11 dBFS，还有约 10 dB 余量，所以限幅器几乎不介入
  （GR 读数常为 0.0 dB）。这意味着 Boom 的 "Volume Boost" 卖点我们一点都没实现。

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
- **`AudioTrack` 不能在 UI 线程释放。** 采集线程手里握着 `track` 引用、正要 `write`，UI 线程
  把它 `release` 掉，就是一次 use-after-release——`play()` 会抛
  `IllegalStateException: Unable to retrieve AudioTrack pointer`。在非主线程抛异常直接杀进程。
  这个窗口本来就有，但把 DSP 调用插进"取引用"和"写"之间以后，它从极难触发变成了几乎必中。
  修法是让**音频线程独占 `track`**：UI 只投递请求（`pendingRoute` / `pendingPlayback`），
  音频线程在每块开头执行。
- **AGP 9 的 Gradle daemon 找不到 `cargo`。** daemon 继承的是它启动时的 PATH，几乎不含
  `~/.cargo/bin`。要按路径解析 cargo，并把它的目录加进 `PATH`——`cargo ndk` 是靠搜索 PATH
  找 `cargo-ndk` 子命令的。
- **Gradle Kotlin 脚本里的 `java` 不是包名**，是 Java 插件扩展。`java.util.Properties` 会报
  "Unresolved reference 'util'"，得 `import java.util.Properties`。

## 下一步

已完成：可行性验证、完整音频链、基础 DSP（10 段 EQ + 限幅器）。

按 Boom 的卖点清单，还没做：

| 模块 | 原理 | 对标 | 状态 |
|---|---|---|---|
| 参量均衡 | biquad IIR，RBJ cookbook | 31 段 EQ | ✅ 10 段 |
| 压缩 + 限幅 | 提升音量后防削顶 | Volume Boost | ⚠️ 限幅有，但没有补偿增益 |
| 响度归一化 | ITU-R BS.1770 / ReplayGain | — | ❌ |
| **动态 EQ / 多段压缩** | 按频段做向上压缩 | 人声突出 | ❌ 静态 EQ 的天花板在这 |
| 虚拟低音 | 缺失基频重建（谐波生成） | Bass boost | ❌ |
| 立体声展宽 | mid/side 处理 | Stereo widening | ❌ |
| 交叉馈送 / HRTF | 串扰 + 头部传递函数 | 3D Surround | ❌ |
| 卷积混响 | IR 卷积 | Ambience / Night Mode | ❌ |

以及那些「产品化的四个硬问题」——尤其自动检测连续静音来解除媒体流静音（否则屏蔽捕获的
app 会变成哑巴）。
