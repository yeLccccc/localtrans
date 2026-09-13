# 安卓中继支持 v0.9.0 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 安卓端完整接入中继:启动自动连接、设备列表合并(本地+名册)、connect 双路由(本地优先/名册兜底)、被动方向 PunchIncoming 接听、设置页状态显示——体验与桌面端一致。

**Architecture:** FFI 层镜像桌面壳(src-tauri)的中继接线,核心逻辑全部复用 core 的 `RelayClient`/`merge_devices`/`validate_relay_config`。Kotlin 侧仅设置页加一个状态行(设备页 `viaRelay` 徽章与 `DevicesChanged` 刷新链路已就位,无需改动)。

**Tech Stack:** Rust(uniFFI 0.28)+ Kotlin(JNA 绑定)+ Compose。中继协议复用 core 既有实现,无新协议。

**Spec:** `docs/superpowers/specs/2026-08-24-android-relay.md`

## Global Constraints

- **测试命令**:per-crate 运行 `cargo test -p localtrans-ffi -- --test-threads=1`(FFI 测试独占 47600/47601 端口,并行会假失败;运行前确认无残留 localtrans 进程)
- **构建链**:Rust FFI 改动后必须 ①`cargo ndk` 编译(注意 `-o` 输出到 `android/app/src/main/jniLibs`,target/android 滞留旧 so 会假成功)②Gradle `genUniffi` 任务重生成绑定 ③**重生成后手工补 AppException 补丁**(3131-3148 行附近,`message` 字段改名 `errorMessage` 并 override `message`;重生成会覆盖丢失,需重做)
- **Kotlin 绑定手工移植五段**(新增 FFI 方法时,genUniffi 后核对差异面并补齐):①JNA fun 声明 ②checksum fun 声明 ③checksum 校验值(以生成产物为准,不要照抄本文档中的数字)④接口方法声明 ⑤实现体
- **安全红线**:PSK 不进日志;明文文件名/路径不进 tracing 日志(只打指纹/数量);配对码只在被连接方屏幕显示
- **提交规范**:提交信息中文前缀(feat:/fix:/test:/docs:/chore:),空行后 `Co-Authored-By: Claude <noreply@anthropic.com>`
- **版本号**:本批收尾统一升 `0.9.0`(Cargo.toml workspace version / src-tauri/tauri.conf.json / android versionName"0.9.0"+versionCode=9),中间任务不动版本
- **复用优先**:合并/排序/验证逻辑一律调 core(`localtrans_core::device_merge::merge_devices`/`alias_map`、`localtrans_core::relay::validate_relay_config`),FFI 层不重复实现

## 桌面壳参照物(实现时对照阅读)

| 功能 | 桌面位置 | 说明 |
|---|---|---|
| 启动自动重连 | `src-tauri/src/main.rs:562-660` | 读 config → validate → spawn 连接+事件桥 |
| set_relay_config | `src-tauri/src/commands.rs:1806-1940` | 验证→落盘→关旧→建新 |
| connect 双路由 | `src-tauri/src/commands.rs:82-118` | 本地优先→名册 connect_peer+adopt_as_initiator |
| relay_status | `src-tauri/src/commands.rs:1948-1975` | enabled/connected/server/devices/error |
| PunchIncoming 接听 | `src-tauri/src/main.rs:615-637` | accept_peer→adopt_connection(被动) |

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/localtrans-ffi/src/state.rs` | 修改 | AppState 加 relay/relay_roster/relay_status 字段 |
| `crates/localtrans-ffi/src/relay_state.rs` | 新建 | RelayUiStatus 枚举 + 纯函数重连接辑(可测) |
| `crates/localtrans-ffi/src/lib.rs` | 修改 | start() 启中继、devices() 合并、connect 路由、save_settings 重连、relay_status API |
| `android/.../uniffi/localtrans_ffi/localtrans_ffi.kt` | 修改 | genUniffi 重生成 + AppException 手工补丁 + relayStatus 绑定 |
| `android/.../data/SettingsRepo.kt` | 修改 | 接口加 relayStatus() |
| `android/.../ui/settings/SettingsViewModel.kt` | 修改 | 加载 relayStatus + 保存后刷新 |
| `android/.../ui/settings/SettingsScreen.kt` | 修改 | 中继卡片加状态行 |
| `CHANGELOG.md` / 版本文件 | 修改 | v0.9.0 收尾 |

---

### Task 1: FFI AppState 扩展 relay 字段

**Files:**
- Modify: `crates/localtrans-ffi/src/state.rs`(结构体字段 + new() 初始化)
- Test: `cargo test -p localtrans-ffi -- --test-threads=1`(既有测试回归)

**Interfaces:**
- Consumes: 无(纯字段扩展)
- Produces(Task 2/3/5 依赖):
  ```rust
  pub relay: Arc<tokio::sync::Mutex<Option<Arc<localtrans_core::relay::client::RelayClient>>>>,
  pub relay_roster: Arc<std::sync::Mutex<Vec<localtrans_core::relay::proto::RemoteDevice>>>,
  pub relay_status: Arc<std::sync::Mutex<crate::relay_state::RelayUiStatus>>,
  pub relay_event_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
  ```

- [ ] **Step 1: 新建 relay_state.rs(枚举先落,Task 2 的重连函数在 Task 2 加)**

在 `crates/localtrans-ffi/src/relay_state.rs`:

```rust
//! 中继 UI 状态(FFI 层)。桌面壳用 emit("relay-state") 推 JSON,
//! 安卓侧无事件通道查状态,改为拉模式:relay_status() 按需读。

/// 中继连接的 UI 可见状态
#[derive(Clone, Debug, PartialEq)]
pub enum RelayUiStatus {
    /// 未启用(config.relay_enabled = false)
    Disabled,
    /// 已启用,连接建立中
    Connecting,
    /// 已注册到中继(名册可用)
    Connected,
    /// 配置无效或连接失败(带原因,设置页显示「配置错误: 原因」)
    Error(String),
}

impl Default for RelayUiStatus {
    fn default() -> Self {
        RelayUiStatus::Disabled
    }
}
```

在 `crates/localtrans-ffi/src/lib.rs` 头部(`mod dto; mod state;` 旁)加:

```rust
mod relay_state;
```

- [ ] **Step 2: AppState 加四个字段**

`state.rs` 结构体尾部(sender_jobs 之后)加:

```rust
    // ===== 中继(v0.9.0):镜像桌面壳 AppState 的 relay 四件套 =====
    /// 中继客户端(启用且连接成功时存在)
    pub relay: Arc<tokio::sync::Mutex<Option<Arc<localtrans_core::relay::client::RelayClient>>>>,
    /// 中结名册快照(RosterUpdated 事件写入,devices() 合并用)
    pub relay_roster: Arc<Mutex<Vec<localtrans_core::relay::proto::RemoteDevice>>>,
    /// 中继 UI 状态(拉模式,relay_status() 读)
    pub relay_status: Arc<Mutex<crate::relay_state::RelayUiStatus>>,
    /// 中继事件桥任务句柄(重连/禁用时 abort)
    pub relay_event_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
```

注意 `state.rs` 顶部已有 `use tokio::sync::{RwLock, Mutex as TokioMutex, ...}`,`tokio::sync::Mutex` 需写全路径或加别名 import——照抄上面的全路径写法最稳。

`new()` 的 `Self { ... }` 尾部加初始化:

```rust
            relay: Arc::new(tokio::sync::Mutex::new(None)),
            relay_roster: Arc::new(Mutex::new(Vec::new())),
            relay_status: Arc::new(Mutex::new(crate::relay_state::RelayUiStatus::Disabled)),
            relay_event_task: Arc::new(tokio::sync::Mutex::new(None)),
```

- [ ] **Step 3: 编译 + 既有测试回归**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 12 passed(纯字段扩展,无行为变化)

- [ ] **Step 4: Commit**

```bash
git add crates/localtrans-ffi/src/state.rs crates/localtrans-ffi/src/relay_state.rs crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): AppState 扩展中继四字段(client/名册/状态/事件任务)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: start() 启动中继 + 事件桥(含 PunchIncoming)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(start() 内,QUIC listener 启动之后、RPC Router 之前插入)
- Modify: `crates/localtrans-ffi/src/relay_state.rs`(加 spawn_relay 纯编排函数)
- Test: `cargo test -p localtrans-ffi -- --test-threads=1` 新增 3 测试

**Interfaces:**
- Consumes: Task 1 的四字段;core 的 `RelayClient::connect(RelayClientConfig, Arc<Identity>) -> Result<(Arc<RelayClient>, mpsc::Receiver<RelayEvent>), String>`;`RelayEvent::{RosterUpdated, StatusChanged, PunchIncoming}`;`sm.adopt_connection(quinn::Connection) -> Result<Fingerprint, SessionError>`;`relay.accept_peer(SocketAddr) -> Result<Connection, String>`
- Produces(Task 3/5 依赖):
  ```rust
  // relay_state.rs
  pub async fn spawn_relay(
      state: &AppState,           // FFI AppState
      callback: &Arc<Box<dyn LocalTransCallback>>,
      runtime: &tokio::runtime::Runtime,  // 不需要——内部 spawn
  )
  // 行为:读 config → 未启用直接返回;启用则 validate(失败置 Error 返回);
  // 否则关旧连接/任务 → RelayClient::connect → 事件桥任务
  // (RosterUpdated→写名册+DevicesChanged;StatusChanged→写 relay_status;
  //  PunchIncoming→accept_peer+adopt_connection)
  ```

- [ ] **Step 1: 写失败测试(relay_state.rs 测试模块)**

`relay_state.rs` 尾部加:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_yields_disabled_status() {
        // relay_enabled=false:spawn_relay 不起连接,状态保持 Disabled
        // (集成级验证在 lib.rs tests 里做;此处验证枚举语义)
        assert_eq!(RelayUiStatus::Disabled, RelayUiStatus::default());
    }
}
```

`lib.rs` tests 模块加 2 个集成测试:

```rust
    #[test]
    fn relay_invalid_config_marks_error_not_connecting() {
        // 坏配置(缺端口)启动:start() 后 relay_status 应为 Error 而非 Connecting
        let dir = tempfile::tempdir().unwrap();
        // 预写坏配置:启用中继但地址缺端口
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"relay_enabled":true,"relay_server":"10.255.255.1","relay_psk":"0123456789abcdef"}"#,
        ).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb));
        app.start();

        let status = app.relay_status();
        assert!(status.enabled, "配置已启用");
        assert!(!status.connected, "不应建立连接");
        assert!(status.error.contains("缺少端口"), "坏配置应给出具体错误,实际: {}", status.error);

        app.shutdown();
    }

    #[test]
    fn relay_valid_unreachable_marks_connecting_then_error() {
        // 好配置但服务器不可达:状态先 Connecting,连接失败后 Error
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"relay_enabled":true,"relay_server":"127.0.0.1:19443","relay_psk":"0123456789abcdef"}"#,
        ).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb));
        app.start();

        let status = app.relay_status();
        assert!(status.enabled);
        // 注意:core RelayClient::connect 内部有 backoff 重试,连接失败
        // 后 status 停留在 Connecting/Reconnecting 而非 Error——这与桌面行为
        // 一致(UI 显示「连接中」)。此测试只断言「不会连接成功且不 panic」。
        assert!(!status.connected, "不可达服务器不应连接成功");
        app.shutdown();
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi -- --test-threads=1 relay_`
Expected: FAIL —— `relay_status` 方法不存在(编译错误)

- [ ] **Step 3: 实现 RelayStatusDto + relay_status() + spawn_relay**

`dto.rs` 加(注意 uniffi 导出宏与既有 DTO 一致——文件里其他 DTO 用 `#[derive(uniffi::Record)]`):

```rust
/// 中继状态(设置页显示;拉模式)
#[derive(uniffi::Record)]
pub struct RelayStatusDto {
    pub enabled: bool,
    pub connected: bool,
    pub status: String,
    pub error: String,
}
```

`relay_state.rs` 加 spawn_relay(事件桥镜像桌面 main.rs:595-660,差异:emit 换成 callback.on_event):

```rust
use std::sync::Arc;
use crate::state::AppState;
use crate::{AppEvent, LocalTransCallback};

/// 按 config 启动/重启中继连接(启动自动重连与 save_settings 变更重连共用)。
/// 行为镜像桌面壳 set_relay_config 的连接建立段:
/// 未启用→不动;启用→validate(失败置 Error 返回)→关旧→建新+事件桥。
pub async fn spawn_relay(
    state: &AppState,
    callback: &Arc<Box<dyn LocalTransCallback>>,
) {
    let (enabled, server, psk, device_name) = {
        let config = state.config.read().await;
        (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone(), config.device_name.clone())
    };
    if !enabled {
        *state.relay_status.lock().unwrap() = crate::relay_state::RelayUiStatus::Disabled;
        return;
    }

    // 配置验证(失败置 Error,不起连接——与桌面 main.rs 启动重连的 validate 门一致)
    if let Err(reason) = localtrans_core::relay::validate_relay_config(true, &server, &psk) {
        tracing::warn!("[relay] 配置无效,不启动中继: {}", reason);
        *state.relay_status.lock().unwrap() = RelayUiStatus::Error(reason);
        return;
    }

    // 关旧连接与事件任务(set_relay_config 变更重连路径会走到这里)
    {
        let mut relay_guard = state.relay.lock().await;
        if let Some(client) = relay_guard.as_ref() {
            client.shutdown().await;
            *relay_guard = None;
        }
    }
    if let Some(h) = state.relay_event_task.lock().await.take() {
        h.abort();
    }
    *state.relay_roster.lock().unwrap() = Vec::new();
    *state.relay_status.lock().unwrap() = RelayUiStatus::Connecting;

    let server_addr: std::net::SocketAddr = match server.parse() {
        Ok(a) => a,
        Err(e) => {
            // validate 已确保可解析,此处兜底(防回归)
            *state.relay_status.lock().unwrap() = RelayUiStatus::Error(format!("无效服务器地址: {}", e));
            return;
        }
    };

    let relay_config = localtrans_core::relay::client::RelayClientConfig {
        server_addr,
        psk,
        device_name,
    };
    let identity = state.identity.clone();
    let relay_state_slot = state.relay.clone();
    let roster_slot = state.relay_roster.clone();
    let status_slot = state.relay_status.clone();
    let sm_for_punch = state.sm.clone();
    let callback_for_bridge = callback.clone();
    let event_task_slot = state.relay_event_task.clone();

    let connect_task = tokio::spawn(async move {
        match localtrans_core::relay::client::RelayClient::connect(relay_config, identity).await {
            Ok((client, mut event_rx)) => {
                tracing::info!("[relay] 中继连接成功");
                *relay_state_slot.lock().await = Some(client.clone());

                let relay_for_punch = client.clone();
                let event_task = tokio::spawn(async move {
                    while let Some(event) = event_rx.recv().await {
                        match event {
                            localtrans_core::relay::client::RelayEvent::RosterUpdated(roster) => {
                                *roster_slot.lock().unwrap() = roster;
                                callback_for_bridge.on_event(AppEvent::DevicesChanged);
                            }
                            localtrans_core::relay::client::RelayEvent::StatusChanged(s) => {
                                // Registered 才算 Connected;Connecting/Reconnecting 映射 Connecting
                                let ui = match s {
                                    localtrans_core::relay::client::RelayClientStatus::Registered =>
                                        crate::relay_state::RelayUiStatus::Connected,
                                    _ => crate::relay_state::RelayUiStatus::Connecting,
                                };
                                *status_slot.lock().unwrap() = ui;
                            }
                            localtrans_core::relay::client::RelayEvent::PunchIncoming { from_fp: _, session_addr } => {
                                // 对端主动连我(被动方向):accept+adopt(镜像桌面 main.rs:615)
                                // 信任校验在 adopt_connection 内部,未配对对端被拒
                                let sm = sm_for_punch.clone();
                                if let Ok(conn) = relay_for_punch.accept_peer(session_addr).await {
                                    if let Err(e) = sm.adopt_connection(conn).await {
                                        tracing::warn!("[relay] 中继入站 adopt 失败: {}", e);
                                    }
                                }
                            }
                        }
                    }
                });
                *event_task_slot.lock().await = Some(event_task);
                *status_slot.lock().unwrap() = crate::relay_state::RelayUiStatus::Connected;
            }
            Err(e) => {
                tracing::warn!("[relay] 中继连接失败: {}", e);
                // 连接失败保留 Connecting(core 内部 backoff 持续重试,
                // 与桌面行为一致:UI 显示「连接中」)
                *status_slot.lock().unwrap() = crate::relay_state::RelayUiStatus::Connecting;
            }
        }
    });
    state.register_event_task(connect_task);
}
```

`lib.rs` 加 relay_status 方法(放 validate_relay 旁):

```rust
    /// 查询中继状态(设置页拉模式显示)
    pub fn relay_status(&self) -> dto::RelayStatusDto {
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                let config = self.runtime.block_on(async { state.config.read().await.clone() });
                let connected = self.runtime.block_on(async { state.relay.lock().await.is_some() });
                let roster_len = state.relay_roster.lock().unwrap().len();
                let status_ui = state.relay_status.lock().unwrap().clone();
                let (status, error) = match status_ui {
                    crate::relay_state::RelayUiStatus::Disabled => ("disabled".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Connecting => ("connecting".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Connected => ("connected".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Error(reason) => ("error".to_string(), reason),
                };
                dto::RelayStatusDto { enabled: config.relay_enabled, connected, status, error: error.clone(), }
            }
            None => dto::RelayStatusDto { enabled: false, connected: false, status: "disabled".into(), error: String::new() },
        }
    }
```

(RelayStatusDto 若需 `#[derive(uniffi::Record)]` + `Clone`,以 dto.rs 既有 DTO 风格为准。)

start() 内插入启动调用。**实现者注意**:start() 的初始化在 `self.runtime.block_on(async move { ... })` 闭包内,`state`(AppState)在闭包内构造。插入位置:闭包内 `AppState::new(...)` 完成后、闭包返回 state 之前。参照闭包内既有代码的位置感——QUIC listener 与 RPC Router 都在 AppState 构造**之前**(它们先拿各 Arc),而 spawn_relay 需要 AppState 整体,所以放最后:

```rust
            // ===== 中继启动(镜像桌面壳 main.rs 10.5):配置启用则自动连接 =====
            // state:闭包内刚构造的 AppState;callback:闭包开头已 clone 的回调 Arc
            crate::relay_state::spawn_relay(&state, &callback).await;

            state
```

(若闭包是 `async move` 且 callback 已被 move,直接用闭包内变量名;以编译器为准。spawn_relay 内部把连接任务经 `state.register_event_task` 登记,shutdown 可 abort。)

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 15 passed(12 既有 + 3 新增;`relay_valid_unreachable` 里 RelayClient 连 127.0.0.1:19443 会失败但不 panic)

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-ffi/src/
git commit -m "feat(ffi): start() 自动连接中继 + 事件桥(名册/状态/PunchIncoming 接听)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: devices() 改为合并(core::merge_devices)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:521-541`(devices 方法)
- Test: `cargo test -p localtrans-ffi -- --test-threads=1` 新增 1 测试

**Interfaces:**
- Consumes: `localtrans_core::device_merge::merge_devices(local: &[DeviceInfo], roster: &[RemoteDevice], connected: &HashSet<String>, aliases: &HashMap<String, String>) -> Vec<MergedDevice>`;`localtrans_core::device_merge::alias_map(&TrustStore) -> HashMap<String, String>`;Task 1 的 relay_roster
- Produces: `devices() -> Vec<DeviceDto>`(via_relay 真实值;Task 4 的 connect 路由依赖 devices 合并后的同一 roster)

- [ ] **Step 1: 写失败测试**

`lib.rs` tests 模块加:

```rust
    #[test]
    fn devices_merges_roster_with_local_priority() {
        // 手工喂名册:core merge_devices 已单测覆盖排序/去重,
        // 此处验证 FFI devices() 真正接了名册(via_relay 不再恒 false)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb));
        app.start();

        // 直接向 state.relay_roster 写入一台测试设备(绕过网络)
        {
            let state_guard = app.state_for_test();
            let state = state_guard.lock().unwrap();
            let st = state.as_ref().unwrap();
            let mut fp = [0u8; 32];
            fp[0] = 0xAA;
            let roster = vec![localtrans_core::relay::proto::RemoteDevice {
                fingerprint: fp,
                name: "中继远程设备".into(),
                lease_addr: "10.0.0.1:47601".parse().unwrap(),
                relayed_ephemeral: false,
            }];
            *st.relay_roster.lock().unwrap() = roster;
        }

        let devices = app.devices();
        let remote = devices.iter().find(|d| d.via_relay).expect("名册设备应出现在列表");
        assert_eq!(remote.name, "中继远程设备");
        assert!(remote.online);

        app.shutdown();
    }
```

同时需要给 LocalTransApp 加测试后门(仅 #[cfg(test)] 可见,放 impl 块):

```rust
    #[cfg(test)]
    pub fn state_for_test(&self) -> &std::sync::Mutex<Option<state::AppState>> {
        &self.state
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi -- --test-threads=1 devices_merges`
Expected: FAIL —— via_relay 恒 false,`expect("名册设备应出现在列表")` panic

- [ ] **Step 3: 实现 devices() 合并**

替换 lib.rs:521-541 的 devices 方法体:

```rust
    /// Get list of discovered devices(本地发现 + 中结名册合并,排序/去重与桌面一致)
    pub fn devices(&self) -> Vec<DeviceDto> {
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                let devices = state.devices.lock().unwrap().clone();
                let roster = state.relay_roster.lock().unwrap().clone();
                let connected = state.connected_fps.lock().unwrap().clone();
                let aliases = self.runtime.block_on(async {
                    localtrans_core::device_merge::alias_map(&*state.trust.lock().await)
                });
                let merged = localtrans_core::device_merge::merge_devices(&devices, &roster, &connected, &aliases);
                merged.into_iter().map(|m| DeviceDto {
                    fingerprint: m.fingerprint,
                    name: m.name,
                    addr: m.addr,
                    online: m.online,
                    connected: m.connected,
                    via_relay: m.via_relay,
                }).collect()
            }
            None => vec![],
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 16 passed

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): devices() 合并本地发现与中结名册(core::merge_devices)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: connect_device 双路由(本地优先/名册兜底)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:556-581`(connect_device 方法)
- Test: `cargo test -p localtrans-ffi -- --test-threads=1` 新增 1 测试

**Interfaces:**
- Consumes: core `RelayClient::connect_peer([u8;32]) -> Result<quinn::Connection, String>`;`sm.adopt_as_initiator(quinn::Connection) -> Result<Fingerprint, SessionError>`;Task 1 的 relay/relay_roster
- Produces: connect 路由行为(桌面 commands.rs:82-118 的 FFI 对应物)

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn connect_routes_to_roster_when_not_local() {
        // 名册有、本地无的设备:应走 relay.connect_peer(无中继连接时报
        // 「中继未连接」而非「Device not found」——证明路由进了名册分支)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb));
        app.start();

        let mut fp = [0u8; 32];
        fp[0] = 0xBB;
        let fp_hex = hex::encode(fp);
        {
            let state_guard = app.state_for_test();
            let state = state_guard.lock().unwrap();
            let st = state.as_ref().unwrap();
            *st.relay_roster.lock().unwrap() = vec![localtrans_core::relay::proto::RemoteDevice {
                fingerprint: fp,
                name: "远程设备".into(),
                lease_addr: "10.0.0.2:47601".parse().unwrap(),
                relayed_ephemeral: false,
            }];
        }

        let err = app.connect_device(fp_hex).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("中继未连接"), "名册设备应走路由分支,实际报错: {}", msg);

        app.shutdown();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi -- --test-threads=1 connect_routes`
Expected: FAIL —— 现状报 "Device not found"

- [ ] **Step 3: 实现 connect 路由**

替换 lib.rs:556-581 connect_device 方法体(镜像桌面 commands.rs:82-118):

```rust
    /// Connect to a device by fingerprint(路由:本地优先,名册兜底)
    pub fn connect_device(&self, fingerprint: String) -> Result<(), AppException> {
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                let fp_bytes = hex::decode(&fingerprint).map_err(|e| AppException::Io { message: e.to_string() })?;
                let mut fp = [0u8; 32];
                fp.copy_from_slice(&fp_bytes);

                // 路由逻辑:优先本地发现,其次中结名册(镜像桌面壳 connect 命令)
                let local_addr = {
                    let devices = state.devices.lock().unwrap();
                    devices.iter().find(|d| d.fingerprint == fp).map(|d| d.addr)
                };

                if let Some(addr) = local_addr {
                    let sm = state.sm.clone();
                    ffi_guard(self, Box::pin(async move {
                        sm.connect(addr).await.map_err(|e| AppException::Internal { message: e.to_string() })?;
                        Ok::<(), AppException>(())
                    }))
                } else {
                    // 查中结名册
                    let in_roster = {
                        let roster = state.relay_roster.lock().unwrap();
                        roster.iter().any(|d| d.fingerprint == fp)
                    };
                    if !in_roster {
                        return Err(AppException::Internal { message: format!("未找到设备: {}", fingerprint) });
                    }

                    let relay_client = self.runtime.block_on(async { state.relay.lock().await.clone() });
                    let Some(client) = relay_client else {
                        return Err(AppException::Internal { message: "中继未连接".to_string() });
                    };
                    let sm = state.sm.clone();
                    ffi_guard(self, Box::pin(async move {
                        let conn = client.connect_peer(fp).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        // 主动方语义:开 bi 流(与对端 adopt_connection 配对,防死锁)
                        sm.adopt_as_initiator(conn).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        Ok::<(), AppException>(())
                    }))
                }
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 17 passed

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): connect 双路由——本地优先,名册兜底 connect_peer+adopt_as_initiator

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: save_settings 中继感知(配置变更重连)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(save_settings 方法,lib.rs:684-716)
- Test: `cargo test -p localtrans-ffi -- --test-threads=1` 新增 1 测试

**Interfaces:**
- Consumes: Task 2 的 `spawn_relay(&AppState, &Arc<Box<dyn LocalTransCallback>>)`;现有 save_settings 的 config 写入
- Produces: save_settings 副作用(中继配置变更 → 重启中继连接;设备名变更 → 名册刷新)

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn save_settings_relay_change_triggers_status_update() {
        // 启用中继+坏地址保存:relay_status 应从 Disabled 变为 Error
        // (证明 save_settings 真的触发了重连评估)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb));
        app.start();

        assert_eq!(app.relay_status().status, "disabled");

        let mut s = app.settings();
        s.relay_enabled = true;
        s.relay_addr = "10.255.255.1".into();   // 缺端口→validate 失败
        s.relay_psk = "0123456789abcdef".into();
        app.save_settings(s);

        let status = app.relay_status();
        assert_eq!(status.status, "error");
        assert!(status.error.contains("缺少端口"));

        app.shutdown();
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi -- --test-threads=1 save_settings_relay`
Expected: FAIL —— 现状 status 仍 "disabled"(save_settings 不触发重连)

- [ ] **Step 3: 实现 save_settings 重连**

save_settings 方法内,`drop(config);` 之后、`if name_changed` 之前加变更检测与重连:

```rust
            // 中继配置变更检测(v0.9.0):enabled/server/psk 任一变化须重启
            // 中继连接——否则新配置不生效(或旧连接残留)。设备名变化也走
            // 重连(Register 上报 device_name,名册端名字靠重连刷新)。
            let relay_changed = old_relay != (settings.relay_enabled, settings.relay_addr.clone(), settings.relay_psk.clone())
                || name_changed;
            drop(config);

            if relay_changed {
                let state_ptr: &crate::state::AppState = state;
                let cb = self.callback.clone();
                self.runtime.block_on(async move {
                    crate::relay_state::spawn_relay(state_ptr, &cb).await;
                });
            }
```

其中 `old_relay` 在写 config 前捕获(在 `let name_changed = ...` 旁):

```rust
            let old_relay = (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone());
```

**实现者注意**:save_settings 当前持有 `self.state.lock().unwrap()` 的 guard 且在 `self.runtime.block_on` 里跑 spawn_relay——spawn_relay 内部会 `state.relay.lock().await`(tokio Mutex,不同锁,无死锁)与 `state.config.read().await`(RwLock 读,与已 drop 的写锁无冲突,前提是 `drop(config)` 在前)。务必保持 `drop(config)` 先于 block_on。若借用检查报错(state 引用跨 await),把 spawn_relay 调用改为先 clone 各 Arc 再进异步块——以编译器为准,语义不变。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 18 passed

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): save_settings 中继感知——配置/改名变更触发重连

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: Kotlin 绑定重建 + 手工补丁

**Files:**
- Modify: `android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt`(genUniffi 重生成 + 补丁)
- Modify: `crates/localtrans-ffi/src/lib.rs`(若 dto.rs 导出需调整)

**Interfaces:**
- Consumes: Task 1-5 的 Rust 侧 API(relay_status -> RelayStatusDto)
- Produces(Task 7 依赖): Kotlin 侧 `LocalTransAppInterface` 含 `fun relayStatus(): RelayStatusDto`;`RelayStatusDto` data class 含 `enabled/connected/status/error` 四字段

- [ ] **Step 1: 编译 so + 重生成绑定**

```bash
# 多 ABI(照项目既有构建脚本;关键是 -o 落到 jniLibs,target/android 不算数)
cargo ndk -t arm64-v8a -t armeabi-v7a -o android/app/src/main/jniLibs build -p localtrans-ffi --release
cd android && ./gradlew genUniffi   # Windows: gradlew.bat genUniffi
```

Expected: 绑定文件重新生成,含 `relayStatus` 方法与 `RelayStatusDto` 类

- [ ] **Step 2: 核对并手工补丁 AppException**

重生成会覆盖 3131-3148 行的手工补丁。检查 `AppException` 两个子类(`Io`/`Internal`):

```kotlin
        // 手工修复:字段改名 errorMessage 并保留 override message;重生成后需重做。
        val errorMessage: kotlin.String = `message`
        override val message: kotlin.String get() = errorMessage
```

字段名 `message` 与 Throwable.message 冲突,不改会编译失败。同时确认构造调用处(`AppException.Io`/`AppException.Internal` 的调用点)用的是命名参数 `message = ...`(与errorMessage 补丁兼容)。

- [ ] **Step 3: 五段核对(若 genUniffi 生成不完整则手工补)**

核对 `relayStatus` 在绑定文件五处齐全(参照 validateRelay 样板):
1. JNA fun 声明:`uniffi_localtrans_ffi_fn_method_localtransapp_relay_status`
2. checksum fun 声明:`uniffi_localtrans_ffi_checksum_method_localtransapp_relay_status`
3. checksum 校验值(以生成产物为准——从 Rust 侧 `cargo build` 输出或生成文件里抄,**不要照抄计划文档里的数字**)
4. 接口声明(LocalTransAppInterface 内):`fun \`relayStatus\`(): RelayStatusDto`
5. 实现体(callWithPointer + uniffiRustCall)

以及 `RelayStatusDto` 的 Record 生成(data class + FfiConverter)。

- [ ] **Step 4: 编译验证**

```bash
cd android && ./gradlew :app:compileDebugKotlin   # Windows: gradlew.bat
```

Expected: BUILD SUCCESSFUL

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/uniffi/
git commit -m "chore(android): 重生成 uniFFI 绑定(relayStatus)+ AppException 手工补丁

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: Kotlin 设置页状态行 + Repo/ViewModel 接线

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/data/SettingsRepo.kt`(接口 + Ffi 实现 + Fake)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/settings/SettingsViewModel.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/settings/SettingsScreen.kt`(中继卡片状态行)
- Test: `android/app/src/test/java/com/localtrans/app/ui/settings/SettingsViewModelTest.kt`(新增)

**Interfaces:**
- Consumes: Task 6 的 Kotlin 绑定(`app.relayStatus(): RelayStatusDto`)
- Produces: UI 状态行(用户可见)——「未启用/连接中/已连接(名册 N 台)/配置错误: 原因」

- [ ] **Step 1: 写失败测试**

`SettingsViewModelTest.kt` 加(以文件既有 Fake 模式为准):

```kotlin
    @Test
    `settingsViewModel relay status reflects fake repo`() = runTest {
        // FakeSettingsRepo 加 relayStatus 覆写后:ViewModel 暴露状态文本
        val fake = FakeSettingsRepo().apply {
            relayStatusOverride = RelayStatusDto(
                enabled = true, connected = false, status = "error",
                error = "服务器地址缺少端口"
            )
        }
        val vm = SettingsViewModel(fake)
        vm.loadRelayStatus()
        assertEquals("配置错误: 服务器地址缺少端口", vm.uiState.value.relayStatusText)
    }
```

(Fake 的 `relayStatusOverride` 属性与 `loadRelayStatus()`/`relayStatusText` 由本任务实现;测试先写,kotlin.test 断言风格照文件内既有测试。)

- [ ] **Step 2: Repo 层接线**

`SettingsRepo.kt`:

```kotlin
interface SettingsRepo {
    fun settings(): SettingsDto
    fun saveSettings(settings: SettingsDto)
    fun setHidden(hidden: Boolean)
    fun relayStatus(): uniffi.localtrans_ffi.RelayStatusDto    // 新增
    override val events: SharedFlow<AppEvent>
}
```

`FfiSettingsRepo`:

```kotlin
    override fun relayStatus(): RelayStatusDto = app.`relayStatus`()
```

`FakeSettingsRepo`:

```kotlin
    var relayStatusOverride: RelayStatusDto? = null
    override fun relayStatus(): RelayStatusDto =
        relayStatusOverride ?: RelayStatusDto(enabled = false, connected = false, status = "disabled", error = "")
```

(RelayStatusDto 若是 uniffi 生成类,测试里直接构造需要全参构造——以上四参即全部字段。)

- [ ] **Step 3: ViewModel 加状态**

`SettingsUiState` 加字段:

```kotlin
data class SettingsUiState(
    val isLoading: Boolean = false,
    val isSaving: Boolean = false,
    val savedTick: Int = 0,
    val error: String? = null,
    val relayStatusText: String = "未启用"    // 新增
)
```

`SettingsViewModel` 加:

```kotlin
    fun loadRelayStatus() {
        viewModelScope.launch {
            try {
                val st = repo.relayStatus()
                val text = when {
                    !st.enabled -> "未启用"
                    st.connected -> "已连接"
                    st.status == "error" -> "配置错误: ${st.error}"
                    else -> "连接中"
                }
                _uiState.update { it.copy(relayStatusText = text) }
            } catch (e: Exception) {
                _uiState.update { it.copy(relayStatusText = "状态未知") }
            }
        }
    }
```

调用时机:①`loadSettings()` 完成后 ②`saveSettings()` 成功后(中继配置可能刚变)——两处各加一行 `loadRelayStatus()`。

- [ ] **Step 4: 设置页状态行**

`SettingsScreen.kt` 中继卡片内(`if (form.relayEnabled)` 块的 PSK 输入框之后)加:

```kotlin
                        // 状态行(v0.9.0):拉模式,进入页面/保存后刷新
                        Text(
                            text = "状态: ${uiState.relayStatusText}",
                            style = MaterialTheme.typography.bodySmall,
                            color = when {
                                uiState.relayStatusText.startsWith("已连接") -> MaterialTheme.colorScheme.primary
                                uiState.relayStatusText.startsWith("配置错误") -> MaterialTheme.colorScheme.error
                                else -> MaterialTheme.colorScheme.onSurfaceVariant
                            }
                        )
```

(具体 Compose 语法以 SettingsScreen.kt 既有代码风格为准——已有 SettingsCard/OutlinedTextField 模式可参照。)

- [ ] **Step 5: 跑单测 + 编译**

```bash
cd android && ./gradlew :app:testDebugUnitTest :app:compileDebugKotlin
```

Expected: 全绿

- [ ] **Step 6: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ android/app/src/test/
git commit -m "feat(android): 设置页中继状态行(未启用/连接中/已连接/配置错误)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 8: 装机 E2E + 回归 + v0.9.0 收尾

**Files:**
- Modify: `Cargo.toml`(workspace version → 0.9.0)
- Modify: `src-tauri/tauri.conf.json`(version → 0.9.0)
- Modify: `android/app/build.gradle.kts`(versionName "0.9.0", versionCode 9)
- Modify: `CHANGELOG.md`
- Test: 全量回归

**Interfaces:**
- Consumes: Task 1-7 全部
- Produces: v0.9.0 发布

- [ ] **Step 1: Rust 全量回归**

```bash
cargo test -p localtrans-core && cargo test -p localtrans-ffi -- --test-threads=1 && cargo test -p localtrans-tauri
```

Expected: 全绿(注意运行前确认无残留 localtrans.exe 占端口)

- [ ] **Step 2: 桌面端回归(PC⇄PC 中继互传不回归)**

```bash
cargo build --release -p localtrans-tauri
```

装机验证:PC 配置真实中继(203.0.113.10:9443)→ 设备页远程互传一个文件。

- [ ] **Step 3: 安卓装机 E2E(模拟器或真机)**

```bash
cd android && ./gradlew :app:assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

验证清单(spec 验收):
1. 手机(模拟器用 cellular 模拟或与 PC 不同网段)配中继(203.0.113.10:9443 + PSK)→ 设置页显示「已连接」
2. 设备页出现 PC,带「远程」徽标,显示真实设备名(不再是 LocalTrans)
3. 手机点连接 PC → 同意门+配对码 → 互传一个文件
4. **反向**:PC 设备页出现手机(真实设备名)→ PC 点连接手机 → 配对 → 互传(验证 PunchIncoming 被动接听)
5. 断中继(改错 PSK 保存)→ 状态「配置错误: ...」,局域网功能不受影响
6. PC+手机同局域网同时在线:设备页互相只有一条(合并去重)

- [ ] **Step 4: 版本号 0.9.0**

三处版本对齐:
- `Cargo.toml:7` → `version = "0.9.0"`
- `src-tauri/tauri.conf.json` → `"version": "0.9.0"`
- `android/app/build.gradle.kts` → `versionCode = 9` / `versionName = "0.9.0"`

- [ ] **Step 5: CHANGELOG**

`CHANGELOG.md` 头部插入:

```markdown
## v0.9.0 — 安卓中继支持 - 2026-08-24

### 新增
- **安卓完整中继支持**:启动自动重连、设备列表合并(本地+名册,指纹去重本地优先)、连接双路由(本地优先/名册兜底)、被动方向 PunchIncoming 接听——手机在 4G/异网可发现并连接家里/办公室 PC
- 设置页中继状态行:未启用/连接中/已连接/配置错误(带具体原因)
- save_settings 中继感知:配置或设备名变更自动重启中继连接(名册名字即时刷新)

### 修复
- 安卓 devices() 此前只读本地发现表,via_relay 恒 false——远程设备永不可见
- 安卓 connect 只走本地,名册设备报「Device not found」
```

- [ ] **Step 6: Commit + tag**

```bash
git add Cargo.toml src-tauri/tauri.conf.json android/app/build.gradle.kts CHANGELOG.md
git commit -m "chore: v0.9.0 版本收尾(安卓中继支持)

Co-Authored-By: Claude <noreply@anthropic.com>"
git tag v0.9.0
```
