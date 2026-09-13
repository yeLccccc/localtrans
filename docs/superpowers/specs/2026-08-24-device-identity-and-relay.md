# 设备体验修复批(v0.8.3)设计文档

日期:2026-08-24
状态:已确认(用户批准分两步走,本批为第一步)
关联:2026-08-24-android-relay.md(第二步,依赖本批的 A 项)

## 背景与问题

用户在真实部署中继(PC v0.8.2 ⇄ 阿里云 Ubuntu 中继)后发现三个问题:

1. **中结名册里所有设备都叫「LocalTrans」**——`relay/client.rs` Register 上报名字硬编码
   `"LocalTrans"`(relay/client.rs:141),与设备实际名称无关。对端从名册看到的永远是
   一个叫 LocalTrans 的陌生设备,叠加已有配对关系后表现为"对方识别为新设备"。
2. **设备列表顺序来回跳动**——桌面壳 `merge_devices` 返回
   `HashMap::into_values()` 无序(commands.rs:2102),每次 `device-list` 事件顺序随机;
   `Devices.vue` 直接 `v-for` 渲染不排序。多设备时卡片位置抖动,难以选中。
3. **中继配置错误无提示**——用户实测踩坑:`203.0.113.10`(缺端口)、
   `203.0.113.10::9443`(双冒号)两种错误输入都静默落盘,UI 只显示"连接中",
   日志仅启动时一条 WARN,排障困难。安卓端更糟:`save_settings` 连地址解析都不做。

### 已验证的现状(不需要做的)

- **指纹唯一标识已实现**:发现层设备表为 `HashMap<[u8;32], DeviceInfo>`
  (discovery.rs:548),按指纹天然去重;`merge_devices` 按指纹合并本地+中结名册;
  配对/信任/权限/别名全部以指纹为键。
- **IP 不绑定设备已实现**:IP 仅为展示字段,连接路由按指纹查当时地址
  (commands.rs:82-118),地址随发现层自动刷新。

## 范围

三项修复(A/C/D),一个共用化重构。版本号 v0.8.3,协议零变更,双端各自升级即生效。

## A. Register 上报真实设备名

### 现状缺陷

```rust
// relay/client.rs:140-143
let reg = RelayMsg::Register {
    name: "LocalTrans".into(),   // ← 硬编码
    fingerprint: identity.fingerprint(),
};
```

### 设计

1. `RelayClientConfig` 增加字段:

```rust
pub struct RelayClientConfig {
    pub server_addr: SocketAddr,
    pub psk: String,
    pub device_name: String,   // 新增:Register 上报的显示名
}
```

2. `do_connect` 的 Register 改用 `config.device_name`。空名兜底 `"LocalTrans"`
   (兼容旧调用方;桌面/安卓壳都会传真实名)。

3. 桌面壳接线:
   - `main.rs` 启动自动重连处、`commands.rs configure_relay` 处,从 `config.device_name`
     构造 `RelayClientConfig`。
   - 改名感知:`set_device_name` 命令检测到改名且中继在线时,重启中继连接
     (shutdown + 重建),让新名立即进名册。中继 Register 只在连接建立时上报一次,
     重连即刷新;租约 45s 刷新周期内对端名册自动跟随。

4. 安卓壳 FFI 接线(本批仅传名,完整中继支持在第二批):`lib.rs` 构造
   `RelayClientConfig` 的位置同步传 `config.device_name`。第二批实现 start() 内
   RelayClient 生命周期时自然消费。

### 名字实时性说明

名册名字的更新链:本机改名 → 重连中继 → Register 新名 → 中继名册更新 →
对端 RosterUpdated 推送。全程 ≤ 秒级(主动重连)或 ≤ 45s(租约周期)。
可接受,不做名字单独上报协议(YAGNI)。

## C. 设备列表排序稳定

### 现状缺陷

- `merge_devices` 输出无序(HashMap::into_values)。
- 安卓 FFI `devices()` 顺序 = 发现层 watch 顺序,与桌面不一致。

### 设计

**后端排序,前端不排**(单一事实源,双端一致)。

1. 排序规则(先后依次比较,全序无歧义):

```
connected desc      // 已连接的排最前(正在用的设备最好找)
online desc         // 在线优先于离线
name asc            // 名字字典序(中文按 Unicode 码点,可预期)
fingerprint asc     // 最终 tiebreaker:同名设备保持稳定相对顺序
```

2. 实现:`merge_devices` 末尾 `sort_by` 链。指纹 tiebreaker 保证:
   即使两台设备改名/上下线,只要集合不变,相对顺序不重排(稳定排序 + 唯一键)。

3. Vue 侧:`Devices.vue` 保持 `v-for devicesStore.devices` 直接渲染
   (后端已排);`:key="fingerprint"` 已存在,不动。

4. 安卓侧:第二批 B 项重构 `devices()` 时使用同一 merge 函数(见共用化重构),
   排序自动一致。本批安卓只排序本地发现列表(对齐规则)。

### 明确不做

- `online` 30s 窗口的闪烁(排序稳定后不影响选中,不改)。
- 用户手动拖拽排序/置顶(YAGNI,等真实需求)。

## D. 中继配置参数验证

### 现状缺陷

- 桌面 `configure_relay`:地址解析失败返回 Err,但 Settings.vue 只弹通用错误;
  启动自动重连路径只打一条 WARN,UI 永远显示"连接中"。
- 安卓 `save_settings`:完全不校验,坏地址静默落盘,运行时静默失败。

### 设计

#### 校验规则(纯函数,双端各一实现,规则对齐)

core 层新增纯函数(Rust,桌面直接用;安卓经 FFI 间接用同一函数):

```rust
/// 校验中继配置,返回 Ok(()) 或中文错误信息
pub fn validate_relay_config(enabled: bool, server: &str, psk: &str) -> Result<(), String>
```

规则:
1. `!enabled` → Ok(关闭状态不校验空值)。
2. `enabled && (server 为空 || psk 为空)` →
   Err("启用中继需要填写服务器地址和密钥")。
3. 地址解析 `server.parse::<SocketAddr>()`:
   - 失败且包含 `"::"` → Err("地址格式无效: 疑似多了一个冒号,应为 IP:端口
     (如 203.0.113.10:9443)")
   - 失败且不含 `":"` → Err("地址格式无效: 缺少端口,应为 IP:端口
     (如 203.0.113.10:9443)")
   - 其它失败 → Err("地址格式无效: {底层错误},应为 IP:端口")
4. PSK 长度 < 16 → Err("密钥过短: 至少 16 字符(与服务端要求一致)")。

注:IPv6 字面量带端口(如 `[::1]:9443`)能被 `SocketAddr::parse` 接受,
自然通过;不支持域名(现状如此,域名支持为独立需求,不在本批)。

#### 桌面接线

- `configure_relay` 开头调用 `validate_relay_config`,Err 直接返回(不落盘不连接)。
- Settings.vue 保存失败 toast 显示完整错误文本(已有 toast 通道,只需错误文本带原因)。
- `main.rs` 启动自动重连:`validate` 失败时打 `WARN 中继配置无效: {原因}`,
  且**不启动连接任务**——状态查询 `relay_status` 返回
  `{"enabled": true, "connected": false, "error": "..."}`,Settings.vue 显示
  「配置错误: {原因}」替代「连接中」。
- `configure_relay` 连接失败路径(验证通过但连不上)同样把失败原因写入
  relay 状态的 error 字段(重试仍走现有 backoff)。

#### 安卓接线(本批)

- `SettingsViewModel.save` 前置校验:调用 FFI 新增的
  `validate_relay(enabled, server, psk) -> Result<(), String>`(uniffi 映射为
  异常/错误字符串),失败时 snackbar 显示原因、不发保存。
- 校验规则在 Rust 侧一份(Kotlin 不重复实现,防漂移)。

## 共用化重构:merge_devices 挪到 core

桌面壳 `commands.rs` 的 `merge_devices` + `alias_map` 挪到
`localtrans-core`(如 `src/device_merge.rs`),签名不变,桌面壳 re-export。
理由:第二批安卓 B 项需要同一合并逻辑,两份实现必然漂移。
本批先挪,第二批直接消费。行为零变化,现有单测跟随移动。

## 测试策略

| 项 | 测试 |
|---|---|
| A | core 单测:RelayClientConfig 带 device_name → Register 编码含该名(e2e 测试桩侧断言);空名兜底单测 |
| C | merge_devices 单测扩展:connected/online/name/指纹 tiebreaker 四组用例;同名双设备顺序稳定性用例 |
| D | validate_relay_config 单测:9 组(关闭通过/空地址/空 PSK/缺端口/双冒号/合法 IPv4/合法 IPv6 带端口/端口越界靠 parse 拒绝/PSK 15 字符拒绝 16 通过) |
| 共用化 | 桌面壳现有 merge 单测迁移后全绿 |

## 非目标

- 安卓中继名册/连接(第二批 spec:2026-08-24-android-relay.md)
- 活跃会话的通路迁移(局域网/中继自动切换,下期)
- 域名形式的中继地址(需 DNS 解析 + 会话地址协议改造,独立需求)
- 用户手动设备排序

## 验收清单

1. 两台 PC 配不同设备名,都连中继 → 各自名册里对方显示真实设备名(非 LocalTrans)
2. 设备页多设备时,刷新/上下线/改名过程中卡片相对位置稳定不跳动
3. 桌面填 `1.2.3.4`(缺端口)保存 → toast「地址格式无效: 缺少端口…」;
   填 `1.2.3.4::9443` → toast「疑似多了一个冒号…」;均不落盘
4. 重启 app(配置仍坏)→ 设置页显示「配置错误: …」而非「连接中」
5. 安卓设置页同样输入 → snackbar 显示对应原因,保存被拦截
6. cargo test 全绿(core + tauri);双端编译通过
