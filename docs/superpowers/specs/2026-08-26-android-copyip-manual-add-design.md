# Android 复制 IP + 手动添加设备 设计文档

日期:2026-08-26
版本目标:v0.10.3(versionCode 14;PC/relay 不动,纯 Android+FFI 版)
前置:v0.10.2(74248c6)
性质:功能补充——Android 与 PC 对齐的两项入口能力

## 需求

1. Android 能把本机 IP 复制给其他设备,对方(在 PC 或另一台手机)手动添加后即可发现自己。
2. Android 有手动添加设备入口:输入对方 IP → 主动探测 → 对方出现在设备列表。

## 现状(已实锤)

- PC 链路完整可镜像:`firewall.rs:128 primary_local_ip()`(UDP connect 8.8.8.8 本地选路不发包)→ 设备页 IP 徽章点击复制;`commands.rs:59 add_manual_device` → `DiscoveryCmd::ProbeAddr`(仅 IP 补默认端口 47600)→ 5s 后回查设备表按 IP 比对 → emit `manual-probe-result{target,found}` → toast。
- FFI 层无 `local_ip`、无 `probe_addr`(全量 pub fn 清单核实)。
- Android DevicesScreen 顶部已有 `MyFingerprintSection`(指纹+隐身开关),无本机 IP、无添加入口。
- `AppState.discovery: Arc<DiscoveryHandle>` 持有 cmd 通道,FFI 可直接发 `DiscoveryCmd::ProbeAddr`。
- v0.10.2 隐身语义:本机隐身时 `ProbeNow/ProbeAddr` 被门控跳过(不发包)——手动探测在隐身下自然静默失败,提示文案需涵盖。

## 方案(已选定 A:最小镜像)

**取舍**:A(FFI 只加两个方法,5s 回查在 Kotlin 侧 delay+devices() 重查)vs B(FFI 发 ManualProbeResult 事件,完全对称 PC)。选 A:PC 的事件本质就是"5 秒后查一次设备表",Kotlin 做同样干净;B 需膨胀 AppEvent 枚举+bridge 分支,且两条路都要重生成 uniffi 绑定(必重做 `AppException.message→errorMessage` 手工补丁,B 增量成本不小。

### 1. FFI(crates/localtrans-ffi/src/lib.rs)

```rust
/// 本机主网络 IP(UDP connect 仅本地选路不发包;镜像桌面壳 firewall::primary_local_ip)
pub fn local_ip(&self) -> Option<String>;

/// 手动探测指定地址:仅 IP 补默认发现端口 47600;IP:端口 直用;坏格式 AppException
/// 隐身时发现层门控自动跳过(不发包)——调用方提示文案涵盖
pub fn probe_addr(&self, addr: String) -> Result<(), AppException>;
```

- probe_addr 实现:解析地址 → 克隆 cmd 通道 → 释放 state 锁 → `runtime.block_on(send(ProbeAddr))`。
- 日志合规:目标 IP 不进 tracing 日志。

### 2. 绑定重生成(android)

`gradle genUniffi` 后**必须重做** `localtrans_ffi.kt` 两处 `AppException.message→errorMessage` 手工补丁(:3228/:3237 附近);`.so` 需 `cargo ndk` 双 ABI 重编(ANDROID_NDK_HOME=C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973)。

### 3. Kotlin 数据层(data/DevicesRepo.kt)

接口 + Ffi + Fake 三处同步加:
```kotlin
suspend fun localIp(): String?          // Ffi: withContext(IO){ app.localIp() }
suspend fun probeAddr(addr: String)     // Ffi: withContext(IO){ app.probeAddr(addr) }
```
Fake:`localIp` 可配置(默认 "192.168.1.105");`probeAddr` 记录 `lastProbedAddr`、可注 `probeError`。

### 4. ViewModel(ui/devices)

- `DevicesUiState` 加 `localIp: String? = null`、`deviceName: String = ""`(loadSettings 顺带)、`manualProbe: ManualProbeUiState?`(null=无探测会话)。
- `ManualProbeUiState(state: ProbeState, target: String, message: String)`,`ProbeState{PROBING,FOUND,NOT_FOUND,ERROR}`。
- `loadLocalIp()`:init 一次性拉取。
- `probeDevice(addr)`:置 PROBING → `repo.probeAddr(addr)` → `delay(5000)` → `repo.devices()` 按目标 IP 前缀比对(`substringBefore(':')`,IPv4 假设)→ FOUND 时刷新设备列表 / NOT_FOUND 给原因文案("对方可能不在线、已开启隐身,或本机隐身中暂停了探测")/ catch 写 ERROR。持 `probeJob` 先 cancel 防叠协程(同 v0.10.2 pairingWatcher 模式)。
- `clearManualProbe()`:弹窗关闭时清状态。

### 5. UI(ui/devices/DevicesScreen.kt)

- 本机信息卡(置于 MyFingerprintSection 之下,视觉同构):本机设备名 + IP 行 + [复制] + [+];IP 为空显示"获取中…"。
- **复制**:`LocalClipboardManager.setText` + `Toast` "已复制 IP,发给对方手动添加即可"(Toast 零状态,不复用 error Snackbar)。
- **蜂窝网灰显**:非私网段(192.168.*/10.*/172.16-31.*)时 IP 行附注"移动网络下 IP 可能无法直连"——纯函数 `isPrivateLanIp(ip): Boolean` 放 DevicesUiModel.kt,单测覆盖。
- **ManualAddDialog**:输入框(placeholder "IP 或 IP:端口")格式预校验(空/明显非法禁用按钮)+ [探测] 按钮 → PROBING 转圈 → FOUND/NOT_FOUND/ERROR 结果文案 + [重试];弹窗关闭调 `clearManualProbe()`。

### 6. 版本收尾

v0.10.3 / versionCode 14 / versionName "0.10.3" / CHANGELOG(新增:Android 本机 IP 复制+手动添加设备入口)。

## 测试

- **Rust**:probe_addr 地址解析三路(仅 IP 补 47600/IP:端口/坏格式 AppException)——解析逻辑抽纯函数可测;local_ip 不 panic 断言。
- **Kotlin 单测**:probe 状态机三路(FOUND/NOT_FOUND/ERROR,Fake repo + 注入短 delay 或 advanceTimeBy);isPrivateLanIp 四例(192.168/10/172.16-31/公网)。
- **回归**:cargo test -p localtrans-ffi -- --test-threads=1;gradle testDebugUnitTest 全量。
- **装机双机**:手机 WiFi 下复制 IP → PC 手动添加互测发现;手机手动添加 PC IP;蜂窝网灰显视检;本机隐身时探测提示 NOT_FOUND 文案。

## 明确不做(YAGNI)

- PC 端任何改动(IP/手动添加 PC 已有)
- QR 码/二维码分享 IP(将来可加,本轮复制文本够用)
- IPv6 手动添加的完整解析(IPv4 假设,IPv6 留 backlog)
- FFI ManualProbeResult 事件(方案 B)

## 全局约束

- 版本对齐 0.10.3 / versionCode 14 / CHANGELOG 三处
- 安全规约:目标 IP/指纹不进日志;PSK/配对码规约沿用
- 绑定重生成后必须重做 AppException.errorMessage 手工补丁(v0.8.0 起已知事项)
- 提交信息:中文前缀 + 空行 + Co-Authored-By: Claude <noreply@anthropic.com>
- 测试 per-crate `-- --test-threads=1`;main 分支直接工作;SDD 双审
