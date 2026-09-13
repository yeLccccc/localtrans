# 安卓中继支持设计文档(第二批)

日期:2026-08-24
状态:已确认(用户批准分两步走,本批为第二步)
前置依赖:2026-08-24-device-identity-and-relay.md(第一批 v0.8.3)的 A 项
(RelayClientConfig.device_name)与共用化重构(merge_devices 挪 core)

## 背景与问题

安卓端(FFI + Kotlin)的中继支持是半成品,实测确认三处断链:

1. **FFI `devices()` 只读本地发现表**(ffi/lib.rs:521-541):中结名册
   (relay_roster)完全未接入,手机上永远看不到中继远程设备——
   `via_relay` 字段硬编码 false(lib.rs:535)。
2. **FFI 无 RelayClient 生命周期**:start() 不读 config 启动中继连接,
   save_settings 改中继配置后无重连动作。桌面壳 main.rs:562-600 的启动
   自动重连在 FFI 层没有对应物。
3. **FFI 无 connect 路由**:桌面壳 connect 命令的「本地优先,名册兜底」
   路由(commands.rs:82-118)在 FFI 层没有对应物,安卓 connect 只会走本地。

结果:用户在 PC + 阿里云中继的真实部署里,手机端完全无法使用中继
(设置页能填配置,但设备页永远看不到远程设备、无法连接)。

## 目标

安卓 App 完整支持中继:配置 → 连接 → 名册可见 → 连接远程设备 → 互传,
体验与桌面端一致。手机在 4G/WiFi(非同局域网)下可发现并连接家里/办公室的 PC。

## 架构

FFI 层镜像桌面壳的中继接线,核心逻辑全部复用 core:

```
Kotlin SettingsScreen     FFI save_settings / relay_status
        ↓                          ↓
  config.relay_* 字段 ──→ FFI start(): RelayClient::connect(含 device_name)
        ↓                          ↓
Kotlin DevicesScreen  ←── FFI devices(): core::merge_devices(本地+名册+connected+别名)
        ↓                          ↓
  connectDevice(fp)  ──→ FFI connect(fp): 本地优先 → 名册 connect_peer + adopt_as_initiator
```

## 详细设计

### 1. FFI AppState 扩展(state.rs)

```rust
pub struct AppState {
    // ...现有字段...
    /// 中继客户端(启用中继时存在)
    pub relay: Arc<tokio::sync::Mutex<Option<Arc<RelayClient>>>>,
    /// 中结名册快照
    pub relay_roster: Arc<std::sync::Mutex<Vec<RemoteDevice>>>,
    /// 中继状态(供 UI 查询:Disconnected/Connecting/Connected/Error(String))
    pub relay_status: Arc<std::sync::Mutex<RelayUiStatus>>,
}

pub enum RelayUiStatus { Disconnected, Connecting, Connected, Error(String) }
```

### 2. start() 内启动中继(lib.rs)

镜像桌面壳 main.rs:562-600:

- 读 config:`relay_enabled && !relay_server.is_empty()`;
- 先跑 `validate_relay_config`(第一批 D 项),失败 → relay_status = Error(原因),
  打 WARN,不起连接任务;
- 成功 → spawn 任务:`RelayClient::connect(cfg, identity)`(cfg 含 device_name),
  成功后进入事件循环消费 `event_rx`:
  - `RosterUpdated(roster)` → 写 relay_roster + 发 `AppEvent::DevicesChanged`(合批:
    名册变化只触发一次设备列表重发);
  - `StatusChanged(s)` → 映射 relay_status;Connected 时发一次 DevicesChanged;
  - `PunchIncoming { from_fp, session_addr }` → 对端主动连我(被动方向):
    `relay.accept_peer(session_addr)` → `sm.adopt_connection(conn)`
    (被动方语义,镜像桌面壳 main.rs 的 PunchIncoming 处理;信任校验在
    adopt_connection 内部,未配对对端被拒)。
    **漏掉此分支则 PC 主动连手机的中继方向不通——手机只能发起、不能被连。**
    (核对补充 2026-08-24:桌面壳确实处理了此事件,原稿遗漏)
- 连接失败 → relay_status = Error + backoff 重试(复用 core 内部 backoff,
  FFI 层不再叠一层)。

### 3. devices() 改为合并(lib.rs)

```rust
pub fn devices(&self) -> Vec<DeviceDto> {
    // core::merge_devices(本地发现, 名册, connected, 别名) → DeviceDto
}
```

- 复用第一批挪到 core 的 `merge_devices`(排序/别称/去重行为与桌面完全一致);
- `alias_map` 同样从 core 取(信任表在 FFI state 已有);
- DeviceDto 已有 via_relay 字段,Kotlin 侧 DevicesUiModel.viaRelay 已存在,
  设备卡片「远程」徽章逻辑已就位(DevicesScreen.kt:221),无需改 UI。

### 4. connect 路由(lib.rs)

镜像桌面壳 commands.rs:82-118:

```
按指纹查本地发现表:
  命中 → sm.connect(addr)
  未命中 → 查名册:
    命中 → relay_client.connect_peer(fp) → sm.adopt_as_initiator(conn)
    未命中 → Err("未找到设备")
```

注意:安卓侧 sm 的连接原语与桌面同源(SessionManager),adopt 语义一致
(主动方开 bi 流),v0.4.0 已修过的"互相 accept_bi 死锁"不会复现。

### 5. save_settings 中继感知(lib.rs)

- relay 配置字段有变化(enabled/server/psk 任一)时:
  shutdown 旧 RelayClient(若在)→ 按"start() 内启动"同款逻辑重建;
- relay 配置无变化 → 不动连接。

### 6. FFI 新增 API

```rust
/// 查询中继状态(设置页显示)
pub fn relay_status(&self) -> RelayStatusDto
// RelayStatusDto { enabled: bool, connected: bool, status: String, error: String }
```

`validate_relay`(第一批 D 项)在本批继续使用。

### 7. Kotlin 侧

- `SettingsRepo`:补 `relayStatus()` 桥接;
- `SettingsScreen` 中继卡片:状态行显示 未启用/连接中/已连接/配置错误: {原因},
  轮询或事件驱动(现有 SettingsViewModel 刷新时机:onResume + 保存后);
- `DevicesViewModel`/`DevicesScreen`:无改动(DevicesChanged 事件已驱动刷新,
  viaRelay 徽章已有);
- EventRouter:DevicesChanged 已路由,无新增事件类型。

## 测试策略

| 层 | 测试 |
|---|---|
| FFI 单测(handler 级) | devices() 合并:本地+名册同指纹去重(本地优先)、仅名册设备 via_relay=true、排序规则 |
| FFI 单测 | connect 路由:本地命中走 sm.connect、名册命中走 connect_peer(桩)、双未命中报错 |
| FFI 单测 | relay_status 状态机:配置无效 → Error;有效 → Connecting/Connected |
| 装机冒烟 | 模拟器装 APK → 配真实中继(用户阿里云 203.0.113.10:9443)→ 设备页出现 PC(远程徽标、真实设备名)→ 连接 → 配对 → 互传一个文件 |
| 回归 | cargo test(ffi/core/tauri);gradle unitTest;PC ⇄ PC 中继互传不回归 |

## 非目标

- 活跃会话通路迁移(连接建立后局域网/中继自动切换)——下期独立 spec
- 域名中继地址
- 中继服务器多选/故障切换

## 验收清单

1. 手机(4G/异网)配好中继 → 设备页出现家里 PC,带「远程」徽标,显示真实设备名
2. 点连接 → 走同意门+配对码(与局域网一致)→ 配对成功可互传
3. 断中继(关掉服务器/改错 PSK)→ 状态显示配置错误/断开,本地局域网功能不受影响
4. PC 端同时在线:手机设备页 PC 只有一条(本地+名册合并去重)
5. cargo test + gradle test 全绿
