# 设备体验修复批 v0.8.3 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复中结名册假名「LocalTrans」、设备列表顺序跳动、中继配置错误无提示三个问题(spec: docs/superpowers/specs/2026-08-24-device-identity-and-relay.md)。

**Architecture:** core 层三处新增(RelayClientConfig.device_name / validate_relay_config 纯函数 / merge_devices+排序挪到 core),桌面壳与 FFI/Kotlin 壳消费。协议零变更。

**Tech Stack:** Rust(tokio/quinn)+ Tauri 2 + Vue 3 + uniFFI 0.28 + Kotlin Compose

## Global Constraints

- 协议零变更:不改任何 wire 格式(Register 本就带 name 字段,只是客户端填错值)
- 错误提示全部中文,含具体原因与示例格式 `203.0.113.10:9443`
- PSK 最短 16 字符(与服务端 config.rs load() 拒启阈值一致)
- 排序规则固定全序:`connected desc → online desc → name asc → fingerprint asc`
- 日志不含文件名/明文 PSK(既有规约)
- 测试命令一律 per-crate `--test-threads=1`(workspace 并行有 UDP 端口竞争 flaky)
- 提交信息中文前缀(fix:/feat:/test:/chore:/docs:)+ 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`
- 构建链坑:uniffi-bindgen 必须用 `android/app/src/main/jniLibs/x86_64/liblocaltrans_ffi.so`;out-dir 尾部含 "uniffi" 会嵌套生成;重生成绑定后 AppException 的 message 冲突需手工补丁(字段改 errorMessage)

---

### Task 1: core 新增 validate_relay_config 纯函数

**Files:**
- Modify: `crates/localtrans-core/src/relay/mod.rs`(加 pub fn + 测试模块)

**Interfaces:**
- Produces: `localtrans_core::relay::validate_relay_config(enabled: bool, server: &str, psk: &str) -> Result<(), String>`(Task 5/7 消费)

- [ ] **Step 1: 写失败测试**

在 `crates/localtrans-core/src/relay/mod.rs` 末尾追加:

```rust
#[cfg(test)]
mod validate_tests {
    use super::*;

    const OK_ADDR: &str = "203.0.113.10:9443";
    const OK_PSK: &str = "0123456789abcdef";

    #[test]
    fn disabled_passes_even_with_empty_fields() {
        assert!(validate_relay_config(false, "", "").is_ok());
    }

    #[test]
    fn enabled_empty_server_rejected() {
        let e = validate_relay_config(true, "", OK_PSK).unwrap_err();
        assert!(e.contains("服务器地址和密钥"), "实际: {e}");
    }

    #[test]
    fn enabled_empty_psk_rejected() {
        let e = validate_relay_config(true, OK_ADDR, "").unwrap_err();
        assert!(e.contains("服务器地址和密钥"), "实际: {e}");
    }

    #[test]
    fn missing_port_rejected() {
        let e = validate_relay_config(true, "203.0.113.10", OK_PSK).unwrap_err();
        assert!(e.contains("缺少端口"), "实际: {e}");
    }

    #[test]
    fn double_colon_rejected() {
        let e = validate_relay_config(true, "203.0.113.10::9443", OK_PSK).unwrap_err();
        assert!(e.contains("多了一个冒号"), "实际: {e}");
    }

    #[test]
    fn valid_ipv4_passes() {
        assert!(validate_relay_config(true, OK_ADDR, OK_PSK).is_ok());
    }

    #[test]
    fn valid_ipv6_with_port_passes() {
        assert!(validate_relay_config(true, "[::1]:9443", OK_PSK).is_ok());
    }

    #[test]
    fn garbage_addr_rejected() {
        let e = validate_relay_config(true, "not an addr:9443", OK_PSK).unwrap_err();
        assert!(e.contains("地址格式无效"), "实际: {e}");
    }

    #[test]
    fn short_psk_rejected_and_16_passes() {
        let e = validate_relay_config(true, OK_ADDR, "0123456789abcde").unwrap_err();
        assert!(e.contains("密钥过短"), "实际: {e}");
        assert!(validate_relay_config(true, OK_ADDR, "0123456789abcdef").is_ok());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```
cargo test -p localtrans-core --lib relay::validate_tests -- --test-threads=1
```
预期:编译失败,`cannot find function validate_relay_config`

- [ ] **Step 3: 实现**

在 `crates/localtrans-core/src/relay/mod.rs`(模块声明区之后)加:

```rust
/// 校验中继客户端配置(壳层保存前调用;规则与服务端拒启阈值对齐)
///
/// 返回 Err(中文原因) 时不得落盘、不得发起连接。
pub fn validate_relay_config(enabled: bool, server: &str, psk: &str) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    if server.trim().is_empty() || psk.is_empty() {
        return Err("启用中继需要填写服务器地址和密钥".into());
    }
    if server.parse::<std::net::SocketAddr>().is_err() {
        // 错误细分:双冒号(IPv6 误写)、缺端口是最常见的两种输入错误
        if server.contains("::") {
            return Err(format!(
                "地址格式无效: 疑似多了一个冒号,应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
            ));
        }
        if !server.contains(':') {
            return Err(format!(
                "地址格式无效: 缺少端口,应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
            ));
        }
        return Err(format!(
            "地址格式无效: 应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
        ));
    }
    if psk.len() < 16 {
        return Err(format!(
            "密钥过短: 至少 16 字符(与服务端要求一致),当前 {} 字符",
            psk.len()
        ));
    }
    Ok(())
}
```

注:`[::1]:9443` 能被 SocketAddr::parse 接受故自然通过——先 parse 后判 "::",顺序不可换。

- [ ] **Step 4: 跑测试确认通过**

```
cargo test -p localtrans-core --lib relay::validate_tests -- --test-threads=1
```
预期:9 passed

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/relay/mod.rs
git commit -m "feat(core): validate_relay_config 纯函数(缺端口/双冒号/短 PSK 中文报错)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: merge_devices/alias_map 挪到 core + 稳定排序

**Files:**
- Create: `crates/localtrans-core/src/device_merge.rs`
- Modify: `crates/localtrans-core/src/lib.rs`(加 `pub mod device_merge;`)
- Modify: `src-tauri/src/commands.rs`(原函数改为薄包装;测试迁移)
- Modify: `src-tauri/src/main.rs`(若调用路径变化,保持 `crate::commands::merge_devices` 可用则不动)

**Interfaces:**
- Produces:
  - `localtrans_core::device_merge::MergedDevice { fingerprint: String, name: String, addr: String, online: bool, connected: bool, via_relay: bool }`
  - `localtrans_core::device_merge::merge_devices(local: &[discovery::DeviceInfo], roster: &[relay::proto::RemoteDevice], connected: &HashSet<String>, aliases: &HashMap<String,String>) -> Vec<MergedDevice>`
  - `localtrans_core::device_merge::alias_map(trust: &identity::TrustStore) -> HashMap<String,String>`
  - 桌面壳 `crate::commands::merge_devices` 签名不变(返回 `Vec<crate::DeviceDto>`,内部转发 core)
- Consumes: 无新依赖

- [ ] **Step 1: 创建 core 模块(含迁移测试 + 新排序测试)**

`crates/localtrans-core/src/device_merge.rs` 全文:

```rust
//! 设备列表合并(本地发现 + 中结名册)——桌面壳与 FFI 壳共用。
//!
//! 规则:
//! 1. 本地发现的设备优先(via_relay=false,地址用发现层地址)
//! 2. 仅中继名册中的设备标记 via_relay=true,地址用 lease_addr
//! 3. 按指纹去重,本地优先
//! 4. 显示名优先级:本地别名(alias) > 对端广播名
//! 5. 输出排序全序:connected desc → online desc → name asc → fingerprint asc
//!    (指纹 tiebreaker 保证集合不变时相对顺序稳定,卡片不跳动)

use std::collections::{HashMap, HashSet};

/// 合并后的设备视图(壳层 DTO 由此转换)
#[derive(Clone, Debug, PartialEq)]
pub struct MergedDevice {
    pub fingerprint: String,
    pub name: String,
    pub addr: String,
    pub online: bool,
    pub connected: bool,
    pub via_relay: bool,
}

pub fn merge_devices(
    local: &[crate::discovery::DeviceInfo],
    roster: &[crate::relay::proto::RemoteDevice],
    connected: &HashSet<String>,
    aliases: &HashMap<String, String>,
) -> Vec<MergedDevice> {
    let mut result: HashMap<String, MergedDevice> = HashMap::new();

    for d in local {
        let fp_hex = hex::encode(d.fingerprint);
        let display_name = aliases.get(&fp_hex)
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_else(|| d.name.clone());
        result.insert(fp_hex.clone(), MergedDevice {
            fingerprint: fp_hex,
            name: display_name,
            addr: d.addr.to_string(),
            online: d.last_seen.elapsed().as_secs() < 30,
            connected: connected.contains(&hex::encode(d.fingerprint)),
            via_relay: false,
        });
    }

    for r in roster {
        let fp_hex = hex::encode(r.fingerprint);
        if result.contains_key(&fp_hex) {
            continue; // 本地优先
        }
        let display_name = aliases.get(&fp_hex)
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_else(|| r.name.clone());
        result.insert(fp_hex.clone(), MergedDevice {
            fingerprint: fp_hex,
            name: display_name,
            addr: r.lease_addr.to_string(),
            online: true,
            connected: connected.contains(&fp_hex),
            via_relay: true,
        });
    }

    let mut list: Vec<MergedDevice> = result.into_values().collect();
    list.sort_by(|a, b| {
        b.connected.cmp(&a.connected)
            .then(b.online.cmp(&a.online))
            .then(a.name.cmp(&b.name))
            .then(a.fingerprint.cmp(&b.fingerprint))
    });
    list
}

/// 从信任列表提取 指纹→别名 映射(空别名不进表)
pub fn alias_map(
    trust: &crate::identity::TrustStore,
) -> HashMap<String, String> {
    trust.all_peers().into_iter()
        .filter(|p| !p.alias.is_empty())
        .map(|p| (hex::encode(p.fingerprint), p.alias))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DeviceInfo;
    use crate::relay::proto::RemoteDevice;
    use std::time::Instant;

    fn dev(fp_byte: u8, name: &str) -> DeviceInfo {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        DeviceInfo {
            fingerprint: fp,
            name: name.into(),
            addr: format!("192.168.1.{}:47601", fp_byte).parse().unwrap(),
            last_seen: Instant::now(),
        }
    }

    fn rem(fp_byte: u8, name: &str) -> RemoteDevice {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        RemoteDevice {
            fingerprint: fp,
            name: name.into(),
            lease_addr: format!("10.0.0.{}:47601", fp_byte).parse().unwrap(),
            relayed_ephemeral: false,
        }
    }

    fn fp_hex(fp_byte: u8) -> String {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        hex::encode(fp)
    }

    #[test]
    fn dedupes_by_fingerprint_local_wins() {
        let local = vec![dev(1, "Device A")];
        let roster = vec![rem(1, "Device A (Relay)"), rem(2, "Device B")];
        let merged = merge_devices(&local, &roster, &HashSet::new(), &HashMap::new());
        assert_eq!(merged.len(), 2);
        let a = merged.iter().find(|d| d.fingerprint == fp_hex(1)).unwrap();
        assert_eq!(a.name, "Device A");
        assert!(!a.via_relay);
        let b = merged.iter().find(|d| d.fingerprint == fp_hex(2)).unwrap();
        assert!(b.via_relay);
        assert!(b.online);
    }

    #[test]
    fn alias_overrides_broadcast_name() {
        let mut aliases = HashMap::new();
        aliases.insert(fp_hex(1), "我起的名".to_string());
        aliases.insert(fp_hex(2), String::new());
        let merged = merge_devices(&[dev(1, "广播名")], &[rem(2, "Relay B")], &HashSet::new(), &aliases);
        assert_eq!(merged.iter().find(|d| d.fingerprint == fp_hex(1)).unwrap().name, "我起的名");
        assert_eq!(merged.iter().find(|d| d.fingerprint == fp_hex(2)).unwrap().name, "Relay B");
    }

    #[test]
    fn sort_connected_first_then_online_then_name() {
        // 全离线、不同名:按名字升序
        let mut c_offline = HashSet::new();
        c_offline.insert(fp_hex(9));
        let merged = merge_devices(
            &[dev(9, "张三"), dev(3, "李四"), dev(5, "王五")],
            &[],
            &c_offline,
            &HashMap::new(),
        );
        let names: Vec<&str> = merged.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["张三", "李四", "王五"], "离线按名字升序");

        // connected 的排最前(即使名字靠后)
        let mut c2 = HashSet::new();
        c2.insert(fp_hex(5));
        let merged = merge_devices(
            &[dev(9, "张三"), dev(3, "李四"), dev(5, "王五")],
            &[],
            &c2,
            &HashMap::new(),
        );
        assert_eq!(merged[0].name, "王五", "connected 排最前");
    }

    #[test]
    fn same_name_devices_sorted_by_fingerprint_stably() {
        // 两台同名设备:指纹 tiebreaker 保证顺序确定
        let merged = merge_devices(
            &[dev(7, "同名的设备"), dev(2, "同名的设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(merged[0].fingerprint, fp_hex(2));
        assert_eq!(merged[1].fingerprint, fp_hex(7));
        // 重复调用结果一致(无 HashMap 随机序)
        let again = merge_devices(
            &[dev(7, "同名的设备"), dev(2, "同名的设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(merged, again);
    }
}
```

- [ ] **Step 2: 注册模块 + 跑测试**

`crates/localtrans-core/src/lib.rs` 的模块声明区(`pub mod relay;` 之后)加:

```rust
pub mod device_merge;
```

```
cargo test -p localtrans-core --lib device_merge -- --test-threads=1
```
预期:4 passed

- [ ] **Step 3: 桌面壳改为薄包装**

`src-tauri/src/commands.rs` 中,把现有 `pub fn merge_devices(...)`(约 2052-2103 行)和 `pub fn alias_map(...)`(约 2106-2113 行)**整体替换**为:

```rust
/// 设备列表合并——转发 core 实现(排序/去重/别称规则与安卓壳同源)
pub fn merge_devices(
    local: &[discovery::DeviceInfo],
    roster: &[localtrans_core::relay::proto::RemoteDevice],
    connected: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, String>,
) -> Vec<crate::DeviceDto> {
    localtrans_core::device_merge::merge_devices(local, roster, connected, aliases)
        .into_iter()
        .map(|m| crate::DeviceDto {
            fingerprint: m.fingerprint,
            name: m.name,
            addr: m.addr,
            online: m.online,
            connected: m.connected,
            via_relay: m.via_relay,
        })
        .collect()
}

/// 从信任列表提取 指纹→别名 映射——转发 core
pub fn alias_map(
    trust: &localtrans_core::identity::TrustStore,
) -> std::collections::HashMap<String, String> {
    localtrans_core::device_merge::alias_map(trust)
}
```

同时把 `src-tauri/src/commands.rs` tests 模块里的两个旧测试
(`merge_local_and_relay_devices_dedupes_by_fingerprint`、`merge_devices_alias_overrides_broadcast_name`)
删除(逻辑已迁至 core 且断言等价覆盖)。保留 tests 模块本身与其他测试。

- [ ] **Step 4: 全量回归**

```
cargo test -p localtrans-core -- --test-threads=1
cargo test -p localtrans -- --test-threads=1
```
预期:core 全绿(新增 4);tauri 全绿(旧 merge 测试删除后无引用残留)

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/device_merge.rs crates/localtrans-core/src/lib.rs src-tauri/src/commands.rs
git commit -m "refactor(core): merge_devices/alias_map 挪到 core 共用 + 输出稳定排序(connected>online>name>指纹)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: RelayClientConfig.device_name + Register 实名

**Files:**
- Modify: `crates/localtrans-core/src/relay/client.rs:19-22`(结构体)、`:140-143`(Register)、`:583-598`(内部测试)
- Modify: `crates/localtrans-relay/tests/e2e.rs`(8 处构造点补字段,约 86/97/321/329/538/549/890/898 行)
- Modify: `src-tauri/src/commands.rs:1815-1818`(set_relay_config 构造点)
- Modify: `src-tauri/src/main.rs:573-576`(启动重连构造点)

**Interfaces:**
- Produces: `RelayClientConfig { server_addr: SocketAddr, psk: String, device_name: String }`(全调用方必须补字段;空名在 do_connect 兜底为 "LocalTrans")
- Consumes: 无

- [ ] **Step 1: 写失败测试(改内部测试)**

`crates/localtrans-core/src/relay/client.rs` 测试模块中 `config_and_status_constructible` 改为:

```rust
    #[test]
    fn config_and_status_constructible() {
        let config = RelayClientConfig {
            server_addr: "127.0.0.1:8080".parse().unwrap(),
            psk: "test-psk".into(),
            device_name: "我的电脑".into(),
        };
        assert_eq!(config.server_addr.port(), 8080);
        assert_eq!(config.device_name, "我的电脑");

        let s1 = RelayClientStatus::Connecting;
        let s2 = RelayClientStatus::Connecting;
        assert_eq!(s1, s2);

        let s3 = RelayClientStatus::Registered;
        assert_ne!(s1, s3);
    }

    /// Register 名兜底:空 device_name 时上报 "LocalTrans"(兼容旧调用方)
    #[test]
    fn register_name_fallback_for_empty() {
        let name = register_display_name("");
        assert_eq!(name, "LocalTrans");
        assert_eq!(register_display_name("实名"), "实名");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```
cargo test -p localtrans-core --lib relay::client -- --test-threads=1
```
预期:编译失败(缺 device_name 字段 / register_display_name 未定义)

- [ ] **Step 3: 实现**

`client.rs` 三处:

1) 结构体加字段:

```rust
pub struct RelayClientConfig {
    pub server_addr: SocketAddr,
    pub psk: String,
    /// Register 上报的显示名(空串时兜底 "LocalTrans")
    pub device_name: String,
}
```

2) do_connect 的 Register 段(约 135-143 行)改为:

```rust
        // 3. 发 Register{name, fingerprint}——名字来自配置(真实设备名),
        //    此前硬编码 "LocalTrans" 导致名册里所有设备同名
        let (mut tx, _rx) = conn
            .open_bi()
            .await
            .map_err(|e| format!("打开 Register 流失败: {}", e))?;
        let reg = RelayMsg::Register {
            name: register_display_name(&config.device_name),
            fingerprint: identity.fingerprint(),
        };
```

3) 模块级私有函数(impl 块外、`#[cfg(test)] mod tests` 前):

```rust
/// Register 显示名:空名兜底旧默认值
fn register_display_name(name: &str) -> String {
    if name.trim().is_empty() { "LocalTrans".into() } else { name.trim().to_string() }
}
```

- [ ] **Step 4: 修全调用点(编译通过)**

1. `crates/localtrans-relay/tests/e2e.rs`:8 处 `RelayClientConfig { server_addr: ..., psk: ... }` 构造,各补一行:
   - 设备 A 的:`device_name: "设备A-实名".into(),`
   - 设备 B 的:`device_name: "设备B-实名".into(),`
   (按上下文哪台是 A 哪台是 B;同一测试内保持一致)
2. `src-tauri/src/commands.rs` set_relay_config 内构造点(约 1815 行)改为:

```rust
        // 创建新的中继连接(device_name 从配置取,Register 上报真实设备名)
        let device_name = state.config.read().await.device_name.clone();
        let relay_config = localtrans_core::relay::client::RelayClientConfig {
            server_addr,
            psk,
            device_name,
        };
```

3. `src-tauri/src/main.rs` 启动重连构造点(约 566-576 行):`let psk = config.relay_psk.clone();` 后加 `let device_name = config.device_name.clone();`,构造处补 `device_name,`

- [ ] **Step 5: 跑测试确认通过**

```
cargo test -p localtrans-core --lib relay::client -- --test-threads=1
cargo build -p localtrans-relay
cargo check -p localtrans
```
预期:core 2 passed;relay/tauri 编译通过

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-core/src/relay/client.rs crates/localtrans-relay/tests/e2e.rs src-tauri/src/commands.rs src-tauri/src/main.rs
git commit -m "fix(core): Register 上报真实设备名(此前硬编码 LocalTrans 导致名册全员同名)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: relay e2e 断言名册实名

**Files:**
- Modify: `crates/localtrans-relay/tests/e2e.rs`(two_devices_connect_and_exchange_over_relay 测试,名册同步等待循环后)

**Interfaces:**
- Consumes: Task 3 的 device_name 字段

- [ ] **Step 1: 加断言**

在 e2e.rs 首个测试「等待名册同步」循环结束、`has_b` 为真之后(约 120 行前后的循环体后)插入:

```rust
    // Register 实名断言:名册里对方必须是注册时的 device_name(而非 "LocalTrans")
    let entry_b = roster_a.iter().find(|d| d.fingerprint == fp_b).expect("名册中应已有 B");
    assert_eq!(entry_b.name, "设备B-实名", "名册应显示 Register 上报的真实设备名");
```

(对称侧若 events_b 有名册消费,同样断言 `设备A-实名`;没有则只断言 A 侧即可。)

- [ ] **Step 2: 跑测试**

```
cargo test -p localtrans-relay --test e2e -- --test-threads=1
```
预期:全绿(若 10048 AddrInUse 偶发,重跑一次确认)

- [ ] **Step 3: 提交**

```bash
git add crates/localtrans-relay/tests/e2e.rs
git commit -m "test(relay): e2e 断言名册显示 Register 真实设备名

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 桌面壳接线(validate 门 + 改名重启中继 + relay_status error)

**Files:**
- Modify: `src-tauri/src/commands.rs`(set_relay_config 1775 起 / save_settings 1596 起 / relay_status 1907 起)
- Modify: `src-tauri/src/main.rs`(启动自动重连 570 起)

**Interfaces:**
- Consumes: `localtrans_core::relay::validate_relay_config`(Task 1)、`RelayClientConfig.device_name`(Task 3)
- Produces: `relay_status` 返回 JSON 增加 `"error": String`(空串=无错;Task 6 UI 消费)

- [ ] **Step 1: set_relay_config 接验证门**

`commands.rs` set_relay_config 中,「// 1. 写配置并落盘」**之前**插入:

```rust
    // 0. 参数验证(失败不落盘不连接,错误带具体原因)
    localtrans_core::relay::validate_relay_config(enabled, &server, &psk)?;
```

同时删除函数内原有的裸解析段(已被 validate 覆盖):

```rust
        // 解析服务器地址
        let server_addr = server.parse::<std::net::SocketAddr>()
            .map_err(|e| format!("无效服务器地址: {}", e))?;
```

替换为:

```rust
        let server_addr: std::net::SocketAddr = server.parse()
            .map_err(|e| format!("无效服务器地址: {}", e))?; // validate 已确保可解析,此处兜底
```

- [ ] **Step 2: save_settings 改名时重启中继连接**

`commands.rs` save_settings 的 `if name_changed { ... }` 块(约 1626-1631 行)替换为:

```rust
    if name_changed {
        let _ = state.discovery.cmd.send(discovery::DiscoveryCmd::SetName(config.device_name.clone())).await;
        tracing::info!("设备名已变更并即时广播: {}", config.device_name);

        // 中继在线时重启连接,让名册立即换新名(Register 仅在连接建立时上报)
        {
            let relay_guard = state.relay.lock().await;
            if relay_guard.is_some() {
                drop(relay_guard);
                tracing::info!("检测到改名且中继在线,重启中继连接以刷新名册名称");
                if let Err(e) = restart_relay(app, &state).await {
                    tracing::warn!("中继重连失败(将按原配置在重启后恢复): {}", e);
                }
            }
        }
    }
```

同文件新增辅助命令级函数(放在 set_relay_config 之前):

```rust
/// 按当前配置重启中继连接(改名刷新名册 / 配置变更后复用)
async fn restart_relay(app: tauri::AppHandle, state: &State<'_, AppState>) -> Result<(), String> {
    let (enabled, server, psk, device_name) = {
        let config = state.config.read().await;
        (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone(), config.device_name.clone())
    };
    if !enabled { return Ok(()); }

    // 关旧连接与事件任务
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
    *state.relay_roster.lock().await = Vec::new();

    // 复用 set_relay_config 的连接建立逻辑(它内部完成 validate/解析/spawn)
    set_relay_config(app, state.clone(), true, server, psk).await
}
```

注:`set_relay_config` 签名是 `(app: AppHandle, state: State<'_, AppState>, ...)`——restart_relay 传 `state.clone()` 前,把 restart_relay 的参数改为 `state: State<'_, AppState>`(Tauri State 可 clone)。save_settings 调用处传 `app` 需在函数签名加 `app: tauri::AppHandle`(若尚无;save_settings 当前无 app 参数则加上,Tauri 命令注入自动提供)。

- [ ] **Step 3: main.rs 启动自动重连带验证**

`src-tauri/src/main.rs` 启动段(约 570 行)`match server_str.parse::<std::net::SocketAddr>()` 整个 match 的 Err 分支替换为 validate 语义——把外层 `if enabled && !server_str.is_empty()` 块的开头改为:

```rust
                    if enabled && !server_str.is_empty() {
                        // 配置验证:失败打 WARN 并拒绝启动连接(不再静默"连接中")
                        if let Err(reason) = localtrans_core::relay::validate_relay_config(true, &server_str, &psk) {
                            tracing::warn!("中继配置无效, 已跳过连接: {}", reason);
                            let _ = app.handle().emit("relay-state", serde_json::json!({
                                "status": "ConfigError",
                                "error": reason,
                            }));
                        } else {
                            let Ok(server_addr) = server_str.parse::<std::net::SocketAddr>() else {
                                unreachable!("validate 通过但解析失败");
                            };
                            // ...原有构造 RelayClientConfig(device_name 已在 Task 3 接入)与 spawn 逻辑不变...
                        }
                    }
```

(实现时保留原有 spawn 体,仅外层包 validate 判断;注意原有 Err 分支的 toast 提示保留语义。)

- [ ] **Step 4: relay_status 增加 error 字段**

`commands.rs` relay_status(1907-1921)返回的 json 改为:

```rust
    // 配置即时校验:失败时 UI 显示"配置错误"而非"连接中"
    let config_error = localtrans_core::relay::validate_relay_config(
        config.relay_enabled, &config.relay_server, &config.relay_psk,
    ).err().unwrap_or_default();

    Ok(serde_json::json!({
        "enabled": config.relay_enabled,
        "connected": connected,
        "server": config.relay_server,
        "devices": device_count,
        "error": config_error,
    }))
```

- [ ] **Step 5: 编译 + 既有测试回归**

```
cargo check -p localtrans
cargo test -p localtrans -- --test-threads=1
```
预期:编译通过;测试全绿

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/commands.rs src-tauri/src/main.rs
git commit -m "feat(desktop): 中继配置验证门 + 改名刷新名册 + 状态可显配置错误

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: Settings.vue 配置错误显示

**Files:**
- Modify: `ui/src/types.ts`(RelayStatus 加 error)
- Modify: `ui/src/pages/Settings.vue`(relay-status 显示逻辑)

**Interfaces:**
- Consumes: `relay_status` 的 `error` 字段(Task 5)

- [ ] **Step 1: types.ts**

```typescript
export interface RelayStatus {
  enabled: boolean
  connected: boolean
  server: string
  devices: number
  /** 配置校验错误(空串=无错);有值时 UI 显示"配置错误: ..." */
  error?: string
}
```

- [ ] **Step 2: Settings.vue 显示逻辑**

中继卡片状态行(约 188-190 行)替换为:

```vue
            <div v-if="relayState" class="relay-status" :class="relayState.connected ? 'ok' : (relayState.error ? 'bad' : '')">
              {{ relayStatusText }}
            </div>
```

script setup 中(`relayState` 声明附近)加 computed:

```typescript
/** 中继状态一行字:已连接 / 配置错误(带原因) / 连接中 / 未启用 */
const relayStatusText = computed(() => {
  const r = relayState.value
  if (!r) return ''
  if (r.connected) return `已连接 · ${r.devices} 台远程设备`
  if (r.error) return `配置错误: ${r.error}`
  return r.enabled ? '连接中…' : '未启用'
})
```

(handleRelaySave 的 catch 已把后端 Err 文本进 toast——Task 5 的 validate 错误自然带原因,无需改。)

- [ ] **Step 3: 构建验证**

```
cd ui && npm run build
```
预期:无 TS 报错,vite 构建成功

- [ ] **Step 4: 提交**

```bash
git add ui/src/types.ts ui/src/pages/Settings.vue
git commit -m "feat(ui): 中继状态显示配置错误及原因(替代永远连接中)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: FFI validate_relay + 安卓前置校验

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(新增 FFI 方法)
- Regenerate: `android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/settings/SettingsViewModel.kt`(save 前置校验)

**Interfaces:**
- Consumes: `localtrans_core::relay::validate_relay_config`(Task 1)
- Produces: FFI `validate_relay(enabled: bool, server: String, psk: String) -> Result<(), String>`(Kotlin 侧 throws AppException)

- [ ] **Step 1: FFI 方法**

`crates/localtrans-ffi/src/lib.rs` impl FfiApp 块内(与 settings 相关方法附近)加:

```rust
    /// 校验中继配置(设置页保存前调用;规则与服务端一致,Rust 单一实现防双端漂移)
    pub fn validate_relay(&self, enabled: bool, server: String, psk: String) -> Result<(), String> {
        localtrans_core::relay::validate_relay_config(enabled, &server, &psk)
    }
```

(FFI 层无需 state——纯函数直接转发。)

- [ ] **Step 2: 重建 so + 重生成绑定(构建链坑规避)**

```bash
# 1) 重建双 ABI so(必须 -o 到 jniLibs;target/android 滞留旧 so 是已知坑)
cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release

# 2) 确认新符号已进 so(应输出若干匹配行)
strings android/app/src/main/jniLibs/x86_64/liblocaltrans_ffi.so | grep -c validate_relay

# 3) 生成 Kotlin 绑定(out-dir 路径尾部不得含 "uniffi",否则嵌套生成)
cargo run -p localtrans-ffi --bin uniffi-bindgen generate \
  --library target/release/liblocaltrans_ffi.dll \
  --language kotlin \
  --out-dir android/app/src/main/java/uniffi/localtrans_ffi_tmp

# 4) 搬出并替换(保留旧目录中已打补丁的部分需重做,见 Step 3)
```

注:步骤 3/4 以仓库现有 genUniffi gradle 任务或既有脚本为准——**优先用 `gradle genUniffi`**(JAVA_HOME 与 gradle 路径见 docs/build-and-test.md);手工命令仅在 gradle 路径失效时用。

- [ ] **Step 3: AppException 手工补丁重做(已知坑)**

重生成后 `localtrans_ffi.kt` 中 `AppException` 的 `message` 与 `Throwable.message` 冲突——把该类中 `message` 字段/构造参数改名为 `errorMessage`(保留 override val message 取 errorMessage 的模式,与仓库现状一致)。核对:`git diff android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt | grep -i message`。

- [ ] **Step 4: Kotlin 前置校验**

`SettingsViewModel.kt` save 函数中,构造 SettingsDto/调用保存**之前**加:

```kotlin
            // 中继配置前置校验(Rust 单一规则实现):失败 snackbar 提示,不落盘
            if (form.relayEnabled) {
                try {
                    bridge.app.validateRelay(true, form.relayAddr, form.relayPsk)
                } catch (e: Exception) {
                    _saveResult.value = SaveResult.Error(e.message ?: "中继配置无效")
                    return@launch
                }
            }
```

(接入点以现有 save 结构为准——`_saveResult`/snackbar 通道沿设置页既有保存反馈;若变量名不同按实际改。`bridge` 引用按 ViewModel 现有依赖注入方式取。)

- [ ] **Step 5: 构建验证**

```bash
export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8"
C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat -p android testDebugUnitTest --offline
C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat -p android assembleDebug --offline
```
预期:单测全绿;APK 构建成功

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-ffi/src/lib.rs android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt android/app/src/main/java/com/localtrans/app/ui/settings/SettingsViewModel.kt
git commit -m "feat(android): 中继配置保存前置校验(FFI validate_relay,规则与桌面同源)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 8: v0.8.3 收尾(版本 + CHANGELOG + 全量回归)

**Files:**
- Modify: `Cargo.toml`(workspace version → 0.8.3)
- Modify: `src-tauri/Cargo.toml`(→ 0.8.3)
- Modify: `src-tauri/tauri.conf.json`(→ 0.8.3)
- Modify: `android/app/build.gradle.kts`(versionCode 8 / versionName "0.8.3")
- Modify: `CHANGELOG.md`(顶部加 v0.8.3 小节)

**Interfaces:**
- Consumes: 全部前序任务

- [ ] **Step 1: 版本五处对齐** (按上述文件逐一修改,值:0.8.3 / versionCode 8)

- [ ] **Step 2: CHANGELOG.md 顶部插入**

```markdown
## v0.8.3 — 设备体验修复(名册实名/列表稳定/中继配置校验) - 2026-08-24

### 修复
- **中结名册里所有设备都显示「LocalTrans」**:Register 上报名字此前硬编码,现上报真实设备名;本机改名后自动重连中继,名册秒级刷新
- **设备列表顺序随机跳动难以选中**:合并输出改为全序稳定排序(已连接 > 在线 > 名字 > 指纹),集合不变时卡片位置不动
- merge_devices/alias_map 下沉到 core,桌面与安卓共用一份实现(行为不变)

### 新增
- **中继配置保存校验(双端)**:缺端口/多冒号/密钥过短等错误保存时立即提示具体原因;坏配置重启后设置页显示「配置错误」而非永远「连接中」
- relay_status 返回 error 字段;FFI 新增 validate_relay(安卓侧规则同源)

### 说明
- 协议零变更(Register 本就携带 name 字段),**单端升级即生效**;名册实名需对端升级后重连一次
```

- [ ] **Step 3: 全量回归**

```bash
cargo test -p localtrans-core -- --test-threads=1
cargo test -p localtrans-ffi -- --test-threads=1
cargo test -p localtrans-relay -- --test-threads=1
cargo test -p localtrans -- --test-threads=1
cd ui && npm run build
```
预期:四 crate 全绿 + UI 构建成功

- [ ] **Step 4: 提交**

```bash
git add Cargo.toml src-tauri/Cargo.toml src-tauri/tauri.conf.json android/app/build.gradle.kts CHANGELOG.md
git commit -m "chore: v0.8.3 收尾(版本五处 + CHANGELOG + 全量回归)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

- [ ] **Step 5: 打 tag**

```bash
git tag v0.8.3
```
