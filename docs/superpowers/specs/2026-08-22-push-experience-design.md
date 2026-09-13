# 推送体验完善设计(主动推送任意文件 + 接收确认交互强化)

日期:2026-08-22
状态:已与用户逐节确认(第 1-4 节全部通过)
版本目标:v0.5.0(用户可见行为大变 + 协议新增可选字段)

## 0. 背景与问题

当前推送(把本机文件推到对方设备)只有**拖放**一条入口:
- 不用拖放就推不了;没有"选择文件"按钮,无法从任意位置挑选文件
- 没有"先选目标设备再选文件"的独立推送入口
- 接收方确认弹窗交互简陋:无超时、无总大小汇总、长列表平铺

用户需求:主动推送可推送任意位置文件;需要选择目标设备、选择要推送的文件;接收方需要选择接收和拒绝;完善交互。

## 1. 需求澄清结论(已确认)

| 项 | 决定 |
|---|---|
| 推送入口 | 双入口:设备卡片【推送文件】按钮 + 设备页顶部向导;拖放保留 |
| 文件选择 | 系统对话框(@tauri-apps/plugin-dialog,零新 UI 依赖);文件夹另入口,选完可移除单项 |
| 接收确认 | 保留三档策略;Ask 档弹窗 60s 倒计时(可调 15-600s),超时自动拒绝 |
| 发送方体验 | 后台任务化,不打断其它操作 |
| 超时范围 | 仅确认环节;发送方有兜底超时防挂起 |
| 拒绝/超时后续 | 任务标"已拒绝/已超时",任务内【重发】(新 job_id 重新 OfferReq);仅拒/超可重发 |
| Auto 档 | 自动接受存默认目录,完成后系统通知(不无声) |
| 多设备 | 单设备一次;多设备=多个独立任务 |
| 拖放路径 | 直接推,不加确认步 |

## 2. 总体架构

沿用既有"core 状态机 / 壳桥接 / UI 展示"分层:

```
UI 层    Devices.vue(向导入口+拖放保留) · DeviceCard.vue(推送按钮)
         PushWizard.vue(新:选设备→选文件→确认) · App.vue(接收确认弹窗升级)
         TransferItem.vue(拒绝/超时状态 + 重发按钮)
壳层      commands.rs: 选文件对话框命令 · push_files_rel(复用) · 重发命令 · 发送前预检
          main.rs: 新事件桥(倒计时/超时/通知) · 系统通知(tauri-plugin-notification)
core 层   protocol.rs: OfferResp 加 reason 字段
          engine.rs: 接收方 Ask 档确认 deadline · 发送方等待 resp 兜底 deadline
          store.rs: offer_timeout_secs 配置(默认 60)
```

### 交互流程

**发起路径 1 — 设备卡片**:点【推送文件…】→ 系统文件选择器(multiple)→ 发送确认小面板(清单+总大小+可移除单项+【发送】/【取消】)→ 发出。

**发起路径 2 — 全局向导**:设备页顶部【推送文件】→ PushWizard 两步模态:
- 步 1:设备单选列表(仅已配对+在线,含远程徽标;无设备引导去连接)
- 步 2:【选择文件】+【选择文件夹】可多次追加,列表可移除单项 → 底部【发送】

**发起路径 3 — 拖放(现状保留)**:拖上去直接推,无确认步。

**接收方(Ask 档)**:弹窗出现即起倒计时(默认 60s,可调 15-600s)→ 到 0 自动拒绝(reason=timeout)→ 三按钮:拒绝 / 另存到… / 接收。Auto 档直收 + 完成系统通知。

**发送方**:发出即后台任务化(传输页+设备卡片角标),状态流转:
`等待对方确认 → 传输中 → 完成 / 已拒绝 / 已超时`,已拒绝/已超时带【重发】。

## 3. 协议变更(protocol.rs,唯一一处)

```rust
pub enum OfferDenyReason { Denied, Timeout }   // serde snake_case: "denied"/"timeout"

// OfferResp 增加:
#[serde(skip_serializing_if = "Option::is_none")]
reason: Option<OfferDenyReason>,   // accepted=false 时给出;true 时 None
```

向后兼容:老版本收不到 reason(Option 默认 None)不报错;两端同版本为常规约束(v0.4.0 起已确立)。

## 4. core 层设计(engine.rs / store.rs)

### 接收方 OfferReq 处理

- `Deny` → 立即回 `{accepted:false, reason:Some(Denied)}`
- `Auto` → 立即回 `{accepted:true, save_dir:默认目录}`(现状不变)
- `Ask` → 发 `OfferRequested` 事件(带 job_id、文件清单、**deadline 绝对时刻**)给 UI;core 同时起 deadline 任务,到点仍未收到 respond → 自动回 `{accepted:false, reason:Some(Timeout)}` + 发 `OfferTimeout` 事件(UI 关弹窗)
- 超时判定在 core:UI 关不关不影响语义;超时后 respond 到达 → 返回"已超时"错误
- **另存中顺延**:收到"另存中"信号(UI 点【另存到…】打开目录对话框)→ deadline 顺延一次(重置为完整时长),每 job 仅一次,防选目录被超时打断

### 发送方

- `push_files_rel` 等 OfferResp 兜底超时 = `offer_timeout_secs + 30s` 宽限,超时任务标 failed("对方无响应"),防对端掉线永久挂起
- 发送方任务终态细分:`已拒绝(reason=denied)` / `已超时(reason=timeout)` / failed(兜底超时等)

### 配置(store.rs)

- Config 加 `offer_timeout_secs: u64`(serde default 60)
- 钳制分层(与同意门 consent_timeout_secs 同款):core 内 `clamp(1, 600)`(供测试用短值),用户面壳层 save_settings `clamp(15, 600)`

## 5. 壳层设计(src-tauri)

### commands.rs

- **选文件对话框命令**:包装 plugin-dialog,`pick_files`(multiple)与 `pick_folder` 两命令,返回路径数组(UI 不直接碰插件,便于测试)
- **发送前本地预检**:push 命令统一做——路径存在+是文件+可读,不过则整单拒绝并列出坏路径(拖放路径同样受益)
- **重发命令**:传输记录已存 local paths 清单 → 【重发】= 用原清单重新调 push_files_rel(新 job_id 新任务),旧任务保持原状;重发前同样预检(原文件已删 → toast"文件不存在")

### main.rs

- 事件桥:OfferRequested(带 deadline)→ 前端;OfferTimeout → 前端;接收任务完成(push 接收方角色)→ 系统通知
- **系统通知**:新增 tauri-plugin-notification 依赖,通知"【设备名】推送了 N 个文件,已存入【目录】";权限未开/失败降级为应用内 toast,不阻断

## 6. UI 层设计

### 发起侧

- **DeviceCard.vue**:菜单加【推送文件…】→ pick_files → 发送确认小面板(清单+总大小+移除单项+发送/取消)
- **Devices.vue 顶部**:【推送文件】按钮 → PushWizard.vue 两步模态(步 1 选设备/步 2 选文件+发送)
- 文件夹经现有 `expand_local_paths` 递归展开(保留相对结构)

### 接收侧弹窗升级(App.vue 现有模态改造)

- 顶部倒计时条(mm:ss,最后 10s 变红),数据来自事件携带的 deadline
- 文件列表 >8 项折叠为"共 N 个文件(展开查看)",新增总大小汇总行
- 点遮罩=拒绝(保留);【另存到…】打开目录对话框时 UI 发"另存中"信号,core 将 deadline 重置为完整时长(每 job 仅一次),UI 倒计时随之重置显示
- 超时事件到达 → 弹窗自动关闭 + toast"已超时自动拒绝"

### 传输页(TransferItem.vue)

- 发送方 pending 文案三态:`等待对方确认…`(resp 未回)→ 现有 pending → 传输中
- 终态新增 `已拒绝`/`已超时` 标签(红色),仅这两态显示【重发】
- 设备卡片角标:有等待确认/传输中推送任务时小转圈

## 7. 锐角场景与错误处理

| 场景 | 发送方 | 接收方 |
|---|---|---|
| Ask 超时 | 任务"已超时",可重发 | 弹窗自动关+toast |
| 手动拒绝 | 任务"已拒绝",可重发 | 弹窗关闭 |
| Deny 档直拒 | 任务"已拒绝" | 无感(现状) |
| 发送方兜底超时(对端掉线) | 任务 failed"对方无响应" | — |
| 选文件部分不可读 | 确认面板标红+禁用发送 | — |
| 另存目录无权限 | — | toast 报错,弹窗保留可重选 |
| 重发时原文件已删 | 本地预检失败,toast"文件不存在" | — |
| 通知权限未开 | — | 降级应用内 toast |

## 8. 测试策略

- **core 单测**:① Ask 档起 deadline,超时自动回 timeout ② 超时后 respond 返回"已超时" ③ 另存中顺延一次 ④ 发送方兜底超时标 failed ⑤ reason snake_case 序列化快照
- **壳层单测**:save_settings 钳制 15-600;push 预检坏路径清单
- **UI 单测**:PushWizard 两步流转/设备过滤;TransferItem 三态 pending+重发按钮条件;接收弹窗倒计时与折叠列表
- **E2E**(relay tests 双会话框架):Ask 档超时分支 + 接受分支,reason 传播到发送方任务状态
- **回归**:拖放路径、Auto 档、断点续传现有测试全绿

## 9. 不做的事(YAGNI)

- 多设备同发任务组(=N 个独立任务,不引入组概念)
- 拒绝历史持久化(任务内重发即可)
- 应用内文件浏览器(系统对话框够用)
- 发起方预设接收方保存位置(接收方主权)
- 已完成/已取消/传输中断任务的"重发"(中断走断点续传,取消重选)
