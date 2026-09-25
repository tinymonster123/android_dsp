# android_dsp

给 Android 手机做一个 Boom 2 式的音质增强。当前状态：**可行性已验证，完整音频链已打通，DSP 有 10 段参数 EQ + 前瞻限幅 + 虚拟低音。**

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
                          ├─ 输出增益（一阶平滑，避免 zipper noise）
                          ├─ 10 段 biquad EQ（RBJ cookbook）
                          ├─ 虚拟低音（缺失基频重建）
                          └─ 2ms 前瞻限幅器 + 软削波
```

七个控件：

| # | 控件 | 作用 |
|---|---|---|
| 1 | 授权并开始抓取 | 申请 MediaProjection + RECORD_AUDIO，起前台服务 |
| 2 | 回放 | 把处理后的音频送出去 |
| 3 | 输出 | 在 `STREAM_MUSIC` / `STREAM_ALARM` / `STREAM_SYSTEM` 之间循环 |
| 4 | 静音原声 | 把 `STREAM_MUSIC` 压到 0，掐掉源 app 的外放 |
| 5 | DSP | 在 5 条曲线之间循环，按钮下方显示当前曲线的意图 |
| 6 | 虚拟低音 | 在 4 档之间循环。与 5 正交，可以任意组合 |
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

### 虚拟低音（缺失基频重建）

手机喇叭发不出 60Hz。但 300Hz 和 420Hz 它能放，而**大脑是从谐波的间距推断音高的，不是从基频本身**。
所以这一模块做的事是：把 40–120Hz 的能量搬到喇叭真正推得动的地方，让耳朵自己把缺失的基频补回来。

**关键在谐波必须是奇次的，写错了也"能响"，只是低音会高一个八度：**

| 生成的谐波 | 成分 | 波形的最小周期 | 听到的音高 |
|---|---|---|---|
| 只偶次（120/240/360） | 全是 120 的倍数 | 120Hz | **高八度** ❌ |
| 奇次（180/300/420） | 除了 60 没有公因子 | 60Hz | 60Hz ✅ |

**全波整流——最顺手的那个非线性——产出的恰好是「DC + 只偶次谐波」**，也就是上表第一行。
（单簧管几乎只有奇次谐波，所以它听起来是记谱音高而不是高八度；同一个道理。）

所以这里的谐波发生器是**奇对称**的（`f(-x) == -f(x)`），用一个对称饱和器实现，而不是整流器。
两个附带好处：谐波幅度按 1/n 衰减（整流器是 1/n²），高次谐波够多，能填满喇叭能放的那一段；
输出有界，不会给限幅器制造意外。

**不做过采样。** 削波必然产生超过 Nyquist 的谐波，折回来就是非谐波垃圾，通常的解法是在非线性器
外面套过采样。这里不需要，理由值得写下来：饱和器**前面**那道 LR4 低通已经把它的输入限制在 120Hz
以内，所以它看到的是一条单音的谐波、按 1/n 衰减，折回来的分量比真谐波低 33dB 以上
（`folding_stays_below_the_noise_floor` 实测，不是断言）。

```sh
cargo test --manifest-path rust/Cargo.toml print_harmonic_table -- --nocapture
```

| 输入 | 3f | 5f | 7f | 9f | 11f | 15f |
|---|---|---|---|---|---|---|
| 40 | −35.5 | −28.9 | −29.0 | −30.6 | −32.3 | −35.3 |
| 60 | −25.9 | −25.6 | −28.0 | −30.3 | −32.3 | −35.8 |
| 80 | −22.3 | −24.9 | −27.9 | −30.5 | −32.7 | −37.1 |
| 100 | −21.0 | −24.9 | −28.2 | −31.1 | −33.7 | −39.4 |
| 120 | −20.7 | −25.2 | −28.8 | −32.1 | −35.5 | −43.0 |

（输入幅度 0.2 = −14 dBFS，amount = 1.0，单位 dBFS。）

两条**测出来的**行为，都不是调出来的：

- **饱和器把电平压住了。** 输入涨 20dB，谐波只涨 2.8dB。所以谐波层是一层音色，
  不是低音的第二份拷贝——它不会跟着音乐一起变响。
- **安静段落自动关断。** 输入 −54dBFS 时谐波是 −129dBFS，也就是完全没有。驱动量定的就是这个
  门槛：太浅则安静段落低音消失，太深则几个 dB 的底噪就会在安静段落里变成持续的嗡嗡声。
  44dB 的驱动量把门槛放在了「−30dBFS 以上恒定量、−55dBFS 以下淡出」。

**驱动量是 44dB，不是 30dB——这是测出来的，不是拍的。** `soft_clip` 收敛得很慢
（`1 − 0.25/|x|`），要到 `|x| ≈ 25` 才接近方波；30dB 时安静的贝斯根本没进饱和区，
谐波电平几乎正比于输入（实测 20dB 输入差 → 26dB 输出差，比线性还陡）。
把它抬到 44dB 才拿到上面那两条性质。这个错误的说法在代码注释里存在过一次，
是被 `the_layer_compresses_on_loud_material_and_gates_on_quiet_material` 打掉的。

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
- **虚拟低音的三个已知边界。** 一是它的天花板由喇叭决定：我们把能量搬进 200–1800Hz，
  这一段手机喇叭也不是满效率，只是比 60Hz 好得多；二是饱和器**没有记忆**，同时发声的低频
  （比如底鼓叠贝斯）会产生互调产物，只是被前面那道低通限制在很低的量级；三是效果强度
  依赖低音自身的电平——−30dBFS 以上是恒定的，往下会淡出。第三条在真实音乐里通常不成问题
  （流行乐的贝斯轨本身压得很紧），但如果听感上出现"安静段落低音消失"，解法是在饱和器前面
  加一级包络归一化，而不是继续加驱动量。

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
- **测滤波器增益不能用峰值。** 48kHz 下 8kHz 只剩 6 个采样点/周期，采到的"峰值"取决于
  滤波器相移把采样点落在哪，跟真实增益没关系（一个 0.99 的高通被测成 0.94）。
  要么用 RMS，要么把测点放在采样点足够密的频段。这个坑是写 `low_pass_has_a_butterworth_corner`
  时踩到的。

## 下一步

已完成：可行性验证、完整音频链、10 段 EQ + 限幅器、虚拟低音。

按 Boom 的卖点清单，还没做：

| 模块 | 原理 | 对标 | 状态 |
|---|---|---|---|
| 参量均衡 | biquad IIR，RBJ cookbook | 31 段 EQ | ✅ 10 段 |
| 压缩 + 限幅 | 提升音量后防削顶 | Volume Boost | ⚠️ 限幅有，但没有补偿增益 |
| 响度归一化 | ITU-R BS.1770 / ReplayGain | — | ❌ |
| **动态 EQ / 多段压缩** | 按频段做向上压缩 | 人声突出 | ❌ 静态 EQ 的天花板在这 |
| 虚拟低音 | 缺失基频重建（奇次谐波生成） | Bass boost | ✅ 4 档 |
| 立体声展宽 | mid/side 处理 | Stereo widening | ❌ |
| 交叉馈送 / HRTF | 串扰 + 头部传递函数 | 3D Surround | ❌ |
| 卷积混响 | IR 卷积 | Ambience / Night Mode | ❌ |

以及那些「产品化的四个硬问题」——尤其自动检测连续静音来解除媒体流静音（否则屏蔽捕获的
app 会变成哑巴）。
