# SPEC-0001 · LocalTrans 自动化测试体系（test-api + 三端编排）

| 项 | 值 |
|---|---|
| 日期 | 2026-09-06 |
| 状态 | **Draft**（评审通过后冻结为 Approved，作为实现计划的唯一输入） |
| 关联现状文档 | `docs/build-and-test.md`、`docs/e2e-android-manual.md`（现行手工流程，本体系是其自动化替代） |
| 后续文档 | 实现计划（`docs/plans/`，另行编写）；契约文档三份（见 §13 交付物） |

---

## 1. 背景与问题陈述

LocalTrans 是局域网互传工具：

- **桌面端**：Tauri 2 壳（`src-tauri/`）+ Vue 3 / Pinia / vue-router 前端（`ui/`），50 个 IPC 命令、约 69 处事件 emit；
- **Android 端**：独立原生工程（`android/`），Jetpack Compose UI，经 UniFFI 调用共享 Rust core（`crates/localtrans-core` → `crates/localtrans-ffi`）；
- **中继**：`crates/localtrans-relay` 独立 QUIC 服务二进制。

现状：约 368 个 Rust 单测 + 7 个 vitest 前端测试，但**没有 E2E**。核心业务场景（A 发起 → B 确认 → 双方进度一致 → 双方终态一致）需要**同时观测与控制多台设备**，现有工具（tauri-driver 不支持 Android 且无法远程观测 Rust 状态；Appium/WinAppDriver 重且选择器脆弱；纯 CDP 只覆盖桌面 webview 且无认证）均不满足。每次发版依赖手工点验，复杂且重复。

本方案：应用内 HTTP 测试控制面（桌面）+ adb 驱动与 debug 钩子（Android）+ 导演机编排器，构成可全自动执行的发版回归体系。

## 2. 目标与非目标

### 2.1 目标

1. 发版手工回归可由**导演机**（开发电脑，不装被测软件）全自动执行，产出带证据的报告；
2. 被测环境：局域网内两台 Windows PC + 一台 USB 连接、开发者模式的 Android 真机。**最小配置即两台 PC**——开发机兼任导演机与被测机 A（编排器经 localhost 驱动本机应用实例），专用测试机为被测机 B，A↔B 间是真实局域网传输；若可提供第三台 PC，导演机独立不装被测软件更干净（优选，非必需）；
3. 桌面应用暴露带认证的 HTTP 测试控制面：页面切换、元素点击/输入、状态查询、日志、截图、语义命令；
4. Android 通过 adb + 应用内 debug 钩子达到同等"看-操作-断言-取证"能力；
5. **正式发布产物不包含任何测试面**（三重门，见 §7.1.1）。

### 2.2 非目标（明确不做）

| 项 | 理由 |
|---|---|
| 像素比对 / 视觉回归 | 不可靠，与"断言靠状态"原则冲突；未来需要另立专题 |
| 场景并行执行 | 被测对象是真实网络互传，并行互相干扰制造 flaky |
| CI 集成 | 体系稳定且频次价值证明后再接入（自托管 runner + 三设备是硬门槛） |
| 平台化/插件化（钩子、自定义断言框架） | 脚本 + SDK 已覆盖需求，框架化只增加维护面 |
| macOS/Linux 被测端 | 当前发布目标不含 |
| YAML/自然语言场景 DSL | 表达力不足会倒逼框架膨胀，采用 TS + SDK（ADR-6） |

## 3. 术语

- **导演机**：开发电脑，运行编排器、fixture 进程，不安装被测软件；
- **被测机 A/B**：局域网两台 Windows，运行桌面应用测试构建；
- **被测机 C**：Android 真机，USB 连接导演机；
- **test-api**：桌面应用内 cargo feature 门控的 HTTP 测试服务；
- **testBridge**：前端页面内注册的测试执行桥（`window.__testBridge`）；
- **Target**：编排器对一台被测设备的统一抽象（`t.pcA` / `t.pcB` / `t.android`）；
- **journal**：编排器记录的全量操作流水（`journal.jsonl`）；
- **证据收割**：失败时自动打包双端日志/截图/快照/流水；
- **三级重置**：L1 软重置 / L2 应用重启 / L3 出厂重置（§7.4.5）。

## 4. 需求

### 4.1 功能需求

| 编号 | 需求 |
|---|---|
| FR-1 | 桌面 UI 控制：HTTP 客户端可令应用切换路由、按选择器点击元素、输入文本（Vue v-model 兼容：原生 setter + input/change 事件） |
| FR-2 | 桌面 DOM 观测：获取可交互元素摘要树（tag/testId/role/text/value/disabled/visible/rect）与文本 |
| FR-3 | 桌面状态观测：获取 Pinia 快照与 Rust 侧 TestSnapshot，均带 schemaVersion |
| FR-4 | 双层等待：页面内元素/文本等待（MutationObserver）；服务端状态条件等待（超时响应附末次观测值） |
| FR-5 | 截图：捕获主窗口 PNG（物理像素 + DPI 元信息，最小化可先还原） |
| FR-6 | 日志访问：按 seq 游标增量拉取环形缓冲日志（level/target/runId 过滤） |
| FR-7 | 前端日志桥：console.warn/error、`app.config.errorHandler`、unhandledrejection 批量汇入 tracing（target=`ui`，附路由与组件名） |
| FR-8 | 语义命令：IPC 命令白名单代理，白名单分级 ReadOnly / Mutating |
| FR-9 | 运行关联：`test/begin`、`test/step`、`test/end` 在日志流中打 runId 与步骤标记 |
| FR-10 | Android 观测与操作：adb 通道完成 UI 树获取（uiautomator dump 解析）、点击、文本输入（ASCII 走 input text，中文走 ADBKeyboard）、截图（screencap） |
| FR-11 | Android 日志：Rust core 日志经桥接进 logcat；启动横幅行（版本+设备标识）作为就绪信号 |
| FR-12 | Android 语义稳定：关键控件铺设 testTag，根布局 `testTagsAsResourceId()`，命名与桌面共用一张表 |
| FR-13 | 配对自动化：debug 构建提供"自动同意配对"钩子（桌面经 FR-8 白名单注入，Android 经 debug 设置项） |
| FR-14 | 统一 Target 抽象：编排器以同一接口操作 PC 与 Android，通道差异（HTTP/SSH/adb）全部封装 |
| FR-15 | 部署与重置：SSH 部署桌面构建并带 env 启停；三级重置（L1/L2/L3），场景声明所需级别 |
| FR-16 | 操作流水：journal 记录每个动作的请求/响应摘要、时间戳、目标机，可回放 |
| FR-17 | 证据收割：任一步失败自动打包双端 runId 范围日志 + 双端截图 + 双端状态快照 + journal 段落 |
| FR-18 | 报告：每次运行输出 markdown 汇总（场景×步骤×断言矩阵、失败详情、工件索引） |
| FR-19 | 测试数据：`tests/e2e/fixtures/`（尺寸梯度文件/中文文件名/空文件/预生成 Ed25519 信任库种子） |
| FR-20 | 环境校验：一键体检（网卡与广播口、AP 隔离、静态 IP、进程残留、Android IME、防火墙） |
| FR-21 | 中继 fixture：localtrans-relay 作为导演机受管子进程，固定端口启停 |

### 4.2 非功能需求

| 编号 | 需求 |
|---|---|
| NFR-1 | 正式产物零测试面：编译期 feature 默认关 + 运行期 env 开关 + 发布产物特征串扫描（构建脚本内强制） |
| NFR-2 | 认证：Bearer token（env 优先，否则数据目录 `test-api.key` 自动生成，权限 0600）；常量时间比较；认证失败记日志附来源 IP 并延迟响应；不添加 CORS 头 |
| NFR-3 | 契约版本化：`apiVersion` 与快照 `schemaVersion` 独立演进，破坏性变更必须升版 |
| NFR-4 | 确定性：场景禁止裸 sleep（SDK 不提供该原语）；重置级别由场景显式声明 |
| NFR-5 | 时钟无关：日志游标用单调 seq，跨机器不比较时间戳 |
| NFR-6 | Android 输入真实：每次交互前重新 dump（缓存必失效）；中文经专用 IME |
| NFR-7 | 维护契约：testId/testTag 命名表为书面契约，纳入 code review |
| NFR-8 | 最小攻击面：不提供文件系统任意读；日志端点仅读环形缓冲；invoke 仅白名单 |

## 5. 设计原则（冲突时按序号优先）

1. **可观测先于可控制**：先能看清应用（状态/日志/DOM），再操作应用；
2. **断言靠状态，不靠像素**：判定基于状态快照、DOM 文本、日志事件；截图只作证据；
3. **一切契约显式化**：端点、快照结构、命名表、错误码都是带版本的书面契约，与公共 API 同等地位；
4. **测试面永不进入正式产物**：三重门缺一不可；
5. **失败必须自带现场**：一次失败 = 一份可复盘的完整证据包。

## 6. 总体架构与能力分层

```
┌──────────────── 开发电脑（导演机，不装被测软件）────────────────┐
│  编排器 = e2e CLI + 场景 SDK + 断言 + 报告生成                    │
│  fixture 进程：localtrans-relay（中继场景用，本地子进程管理）      │
└─────┬──────────────────┬───────────────────┬─────────────────┘
      │ HTTP + Bearer     │ SSH（部署/启停/重置）│ adb（USB）
      ▼                   ▼                   ▼
 被测机 A（Windows）   被测机 B（Windows）   被测机 C（Android）
 Tauri 壳+test-api     Tauri 壳+test-api     Compose 壳
 └─ webview+testBridge                       └─ testTag+logcat桥
      └──────────── 真实 QUIC/UDP 互连 ────────────┘
```

> 最小硬件配置（两台 PC）：开发机兼任导演机与被测机 A（编排器经 localhost 驱动本机实例）；上图三机拓扑为优选配置。

能力分层（每层只依赖下一层）：

```
L3 编排层   场景SDK · fixtures · 三级重置 · 报告 · journal
L2 控制层   ui/navigate·click·input · invoke白名单 · 进程生命周期(ssh/adb)
L1 观测层   ui/tree · ui/text · state快照 · state/wait · logs/tail
L0 证据层   screenshot · 日志归档 · journal回放
```

## 7. 详细设计

### 7.1 桌面 test-api 服务（`src-tauri/src/test_api/`，feature `test-api`）

#### 7.1.1 门控与安全（三重门）

- **编译期**：cargo feature `test-api` 默认关闭；`axum`、`xcap` 为 optional 依赖，正常构建不进依赖树；
- **运行期**：`LOCALTRANS_TEST_API=1` 才监听，否则代码路径完全不触网。绑定地址 `LOCALTRANS_TEST_API_BIND` 可限制（默认 `0.0.0.0`，建议测试网段）。Token 来源：`LOCALTRANS_TEST_API_KEY` > app 数据目录 `test-api.key`（32 字节随机 hex，0600）；
- **发布期**：打包脚本追加产物扫描——release 二进制中搜索特征串（专属响应头 `X-LocalTrans-TestAPI`），命中即构建失败。

服务器跑在 Tauri 已有 tokio runtime 上，随应用退出优雅关闭，不引入新进程。

#### 7.1.2 协议契约

统一响应包络（所有端点一致）：

```jsonc
// 成功
{ "ok": true, "data": { /* ... */ }, "meta": { "apiVersion": 1, "runId": "..." } }
// 失败
{ "ok": false, "error": { "code": "ELEMENT_NOT_FOUND", "message": "...", "detail": { "selector": "..." } } }
```

错误码表（封闭集合，新增须改契约文档 `docs/contracts/test-api.md`）：

| HTTP | code | 含义 |
|---|---|---|
| 401 | `AUTH_FAILED` | token 错误/缺失 |
| 400 | `BAD_REQUEST` | 参数错误 |
| 403 | `INVOKE_NOT_ALLOWED` | 命令不在白名单（detail 列出允许项） |
| 404 | `ELEMENT_NOT_FOUND` | 选择器无匹配 |
| 408 | `WAIT_TIMEOUT` | 等待条件超时（detail 带最后一次观测值） |
| 409 | `BRIDGE_NOT_READY` | 前端无 testBridge（构建不匹配） |
| 500 | `INTERNAL` | 内部错误（带 tracing 关联 id） |

版本握手：`GET /api/version` 返回 `{appVersion, apiVersion, bridgeReady, buildProfile}`。编排器每次连接先调它，版本不符或 bridge 缺失立即终止场景。

#### 7.1.3 端点清单（v1）

| 端点 | 方法 | 入参 → 出参要点 |
|---|---|---|
| `/api/health` | GET | 存活 + bridgeReady |
| `/api/version` | GET | 版本握手 |
| `/api/ui/tree` | GET | → 元素摘要数组（§7.1.4） |
| `/api/ui/navigate` | POST | `{path}` → 路由结果 |
| `/api/ui/click` | POST | `{selector}` → 元素文本回传 |
| `/api/ui/input` | POST | `{selector, value, clear?}` |
| `/api/ui/text` | GET | `{selector?}` → innerText（缺省全文） |
| `/api/ui/wait` | POST | `{selector \| text, timeoutMs}` → 200/408 |
| `/api/state/app` | GET | → Pinia 快照（带 schemaVersion） |
| `/api/state/transfers` | GET | → Rust TestSnapshot（§7.1.6） |
| `/api/state/wait` | POST | `{source, path, op, value, timeoutMs}` |
| `/api/logs/tail` | GET | `?afterSeq=&level=&target=&runId=` → 增量日志 + 游标 |
| `/api/screenshot` | GET | → `image/png`，meta 带 `{width, height, scale}` |
| `/api/invoke` | POST | `{cmd, args}` → 白名单内命令结果 |
| `/api/test/begin` | POST | `{scenario}` → `runId` |
| `/api/test/step` | POST | `{name}` → 步骤标记入日志流 |
| `/api/test/end` | POST | `{runId, outcome}` |

选择器语法：CSS 选择器 + `[testid=x]` 简写 + `^=` 前缀匹配（应对 `transfer-card-{id}` 类动态 id）。

#### 7.1.4 UI Bridge 机制

往返时序（解决 `webview.eval` 单向性）：

```
HTTP 线程                    webview (testBridge)                IPC 线程
   │ POST /api/ui/click           │                                 │
   │ id = next_id()               │                                 │
   │ eval("__testBridge.exec(J)")─▶│ 查元素(短轮询)→点击              │
   │ 注册 oneshot[id]             │ invoke('test_bridge_result',    │
   │ await oneshot (带超时)       │   {id, ok, data|error}) ────────▶│ 唤醒 oneshot[id]
   ◀──────────────────────────── HTTP 200/408 ◀────────────────────│
```

- 请求 id 单调递增；bridge 侧异步排队，允许多请求并发在途；
- payload 以单个 JSON 字符串字面量嵌入 eval，杜绝转义歧义；eval 经 ExecuteScript 执行，不受页面 CSP 限制，现有 CSP 无需放宽；
- **就绪握手**：testBridge 启动早期调用 `test_bridge_hello`，服务端置位 `bridgeReady`；feature 开而 bridge 缺失时所有 `ui/*` 返回 `BRIDGE_NOT_READY` 并附提示（把"前端忘了测试构建"从玄学变成一句话报错）；
- 输入兼容 v-model：原生 value setter + 派发 `input`/`change`。

`/api/ui/tree` 序列化契约（编排方"看页面"的唯一入口，稳定性最高优先）：

```jsonc
{ "tag": "button", "testId": "send-btn", "role": "button",
  "text": "发送", "value": null, "disabled": false,
  "visible": true, "rect": [x, y, w, h] }
```

不可见元素（`offsetParent == null`、`aria-hidden`）剪枝；文本截断 80 字符。

#### 7.1.5 等待语义（双层）

- `/api/ui/wait`：页面内 MutationObserver + 短轮询，元素出现即刻返回，不受 HTTP 往返延迟影响；
- `/api/state/wait`：Rust 侧对快照函数求值，`path` 点分路径（数组长度用 `.length`），`op ∈ {eq, ne, gte, lte, contains, exists, empty}`，默认 500ms 轮询，超时 detail 附末次观测值。

#### 7.1.6 状态快照契约（显式 TestSnapshot）

不序列化内部结构（内部类型一重构测试全红），定义显式裁剪结构：

```rust
struct TestSnapshot {
    schema_version: u32,
    self_device: DeviceInfo,
    devices: Vec<DeviceBrief>,          // id、名称、状态、地址
    sessions: Vec<SessionBrief>,        // 对端、状态、可信与否
    transfers: Vec<TransferBrief>,      // id、方向、状态机值、bytes_done/total、速率
    cards: Vec<CardBrief>,              // 卡片 id、engine_id、计数器、终态
    discovery_stats: DiscoveryStats,    // 广播收发计数（网络问题排查抓手）
}
```

Pinia 侧同理：四个 store 各自定义 `toTestSnapshot()`。`schemaVersion` 独立于 app 版本演进。

#### 7.1.7 日志子系统

1. **环形缓冲 layer**：挂现有 tracing，容量 1 万条，字段 `{seq, ts, level, target, message, runId, step}`；`/api/logs/tail` 用 seq 游标增量拉取，单次上限 500 条；
2. **前端 console 桥**：Vue 插件拦截 console.warn/error + errorHandler + unhandledrejection，50ms 批量聚合走 IPC 入 tracing（target=`ui`，附路由与组件名）；
3. **run/step 关联**：begin 之后所有日志（Rust + 前端）带 runId，step 在日志流打标记，事后按 runId 提取按步骤分段的完整时间线。

#### 7.1.8 截图

`xcap` 按窗口 HWND 捕获，返回物理像素 PNG，meta 报告 DPI 缩放；参数 `restore=true`（默认）在最小化时先还原。

#### 7.1.9 invoke 白名单

`const ALLOWED: &[(&str, Class)]`，`Class ∈ {ReadOnly, Mutating}`；Mutating 每次调用日志高亮。典型入列：读配置、注入配对同意、清理已完成卡片。**不设全命令代理**：删除/解绑/清信任等破坏性命令绝不入列，硬重置走进程级方案（L2/L3）。

### 7.2 前端（`ui/`）

- `testBridge` 模块（`VITE_TEST_API` 构建期开关，测试构建才打包）：实现 §7.1.4 契约，直接 import router/Pinia 实例；
- console/error 日志桥（§7.1.7-2）；
- data-testid 铺设：4 页面 + 7 组件关键控件，命名表见 §13（与 Android 共用）。

### 7.3 Android（`android/`，全部 debug buildType 门控）

adb 工具层（编排器 `AndroidTarget` 实现）：

| 能力 | 实现 | 关键契约 |
|---|---|---|
| 看页面 | `uiautomator dump` → XML 解析为元素+bounds | **每次交互前重 dump**；根布局 `testTagsAsResourceId()` 使 testTag 以 resource-id 可见 |
| 点击 | dump 定位 → 中心坐标 → `input tap` | 短重试 + 重 dump 确认 |
| 输入 | ASCII `input text`；**中文 ADBKeyboard 广播**（input text 不支持非 ASCII） | 测试机统一装 ADBKeyboard 并设默认 IME |
| 截图 | `adb exec-out screencap -p` | 零应用代码 |
| 日志 | `adb logcat -v time` 按 tag 过滤 + 落盘 | 见下 |
| 部署 | `adb install -r`；Gradle 硬编码 NDK 路径改为 `local.properties` 读取 | 换机不断 |

应用内小改（debug 门控）：

1. tracing → logcat 桥（`android_logger` + `tracing-log`），tag 按模块前缀；启动横幅行（版本+设备标识）作就绪信号；
2. testTag 铺设（命名表与桌面共用）；
3. 配对自动同意钩子（debug 设置项 + 持久化）——系统级弹窗交给 UI 自动化是 flaky 之源，语义钩子才是正解；
4. （可选后置）NanoHTTPD debug HTTP 服务：经 `adb reverse` 直达，仅暴露 `/api/state`（Repos 快照）+ 语义动作，响应包络与桌面契约一致；树与截图仍走 adb。是否需要由 M7 后评估（OQ-7）。

### 7.4 编排器（`tests/e2e/`）

#### 7.4.1 目录契约

```
tests/e2e/
├─ bin/cli.ts          # e2e 命令入口
├─ lib/
│  ├─ target.ts        # Target 统一接口（pc/android 两实现）
│  ├─ http.ts / ssh.ts / adb.ts
│  ├─ journal.ts       # 操作流水
│  └─ evidence.ts      # 失败取证
├─ scenarios/*.ts      # 场景（每文件一个）
├─ fixtures/           # 测试数据 + 信任库种子
├─ targets.local.yaml  # 三台设备地址与 token 引用（不进 git）
└─ reports/            # 每次运行输出目录
```

#### 7.4.2 场景 SDK 形态（TS + 类型化 SDK）

```ts
scenario('pc-pc-send-large-file', { requires: ['pcA','pcB'], reset: 'app' }, async t => {
  await t.step('双向发现', () =>
    Promise.all([ t.pcA.waitState('devices.length','gte',1),
                  t.pcB.waitState('devices.length','gte',1) ]));
  await t.step('A→B 发送 2GB 文件', async () => {
    await t.pcA.ui.click({ testid: 'send-btn' });
    await Promise.all([
      t.pcA.waitState('transfers[0].state','eq','Done'),
      t.pcB.waitState('transfers[0].state','eq','Done') ]);
    t.expect(t.pcA.transfers[0].bytesDone).eq(t.pcB.transfers[0].bytesDone);
  });
});
```

每个 step 自动：截图（可配 before/after）→ 日志标记 → 失败即触发证据收割。

#### 7.4.3 Journal

CLI 把每个动作（请求/响应摘要、时间戳、目标机）追加写入 `journal.jsonl`。作用：失败复盘精确回放操作序列；作为"人或 AI 重跑同样操作"的执行依据。

#### 7.4.4 证据收割

任一步失败自动打包：双端 runId 范围全量日志 + 双端截图 + 双端状态快照 + journal 段落 → `reports/<run>/failure-<step>/`。

#### 7.4.5 三级重置

| 级别 | 手段 | 清除范围 |
|---|---|---|
| L1 软重置 | `/api/invoke` 白名单清理命令 | 已完成卡片、toast |
| L2 应用重启 | ssh/adb 杀进程 + 带 env 重启（**确认 pid 已死再启**，规避 single-instance 只聚焦旧实例的坑） | 内存态（保留配置与信任） |
| L3 出厂重置 | 删应用数据目录 + fixture 信任库播种 | 全部，配对从确定性种子开始 |

#### 7.4.6 报告

每次运行输出 markdown 汇总：场景×步骤×断言矩阵、失败详情链接、工件目录索引。发版回归 = 对报告做决策。

### 7.5 环境工程（坑位对策表）

| 坑 | 对策（写进环境文档 + 校验脚本） |
|---|---|
| 多网卡（Hyper-V/WSL 虚拟网卡）致 UDP 广播走错口 | 环境校验列网卡；应用增加发现接口绑定配置项，测试环境显式指定 |
| 路由器 AP 隔离 | 三台设备同 SSID/交换机、隔离关闭；校验脚本双向探测 |
| `tauri-plugin-single-instance` | 重启前 kill + 轮询 pid 确认死亡 |
| 首次运行防火墙弹窗 | 环境预配置一次性管理员授权（复用 `firewall.rs`） |
| Windows 休眠/锁屏 | 测试机禁用休眠/睡眠 |
| DHCP 地址漂移 | 静态 IP 或 DHCP 保留 |
| DPI 缩放 | 截图 meta 带 scale；坐标计算只在 dump 数据内闭环 |
| uiautomator dump 过期 | 每次交互前重 dump（入契约） |
| adb 中文输入 | ADBKeyboard 专用 IME（入契约） |

## 8. 关键设计决策记录（ADR 摘要）

| # | 决策 | 理由 | 否决的替代 |
|---|---|---|---|
| ADR-1 | 自建 HTTP 控制面 + 三端编排 | 核心场景需同时观测控制多台设备，含 Rust 内部状态 | tauri-driver（无 Android、远程驱动别扭）；Appium/WinAppDriver（重、选择器脆）；纯 CDP（仅桌面 webview、无认证） |
| ADR-2 | UI 控制走页面内 bridge，非坐标/事件模拟 | 真实 DOM 事件、selector 稳定、Vue 兼容 | 坐标点击（脆弱）；全局键盘/鼠标钩子（影响导演机） |
| ADR-3 | 显式 TestSnapshot，非序列化内部结构 | 内部类型重构不连坐测试红；契约可 review | 直接 `Serialize` 内部 AppState |
| ADR-4 | 断言不依赖截图 | 像素比对不可靠；证据与判定分离 | 像素 diff |
| ADR-5 | Android 用 adb + 语义标签，非 in-app UI 自动化框架 | Compose 无 webview；uiautomator 成熟；应用改动最小 | 在 app 内做 UI 遥控（Compose 生产代码难驱动 UI） |
| ADR-6 | 场景 = TS + 类型化 SDK | 表达力完整、可断言可复用 | YAML DSL（表达力不足倒逼框架膨胀） |
| ADR-7 | 日志游标用 seq，非时间戳 | 跨机时钟偏差不可信 | ts 游标 |
| ADR-8 | 编译期 feature + 运行期 env 双门 | 单门皆有失误面：feature 防"装进去"，env 防"误启用" | 仅 feature；仅 env |
| ADR-9 | invoke 白名单分级，非全命令代理 | 破坏性命令绝不暴露；日志可审计 | 代理全部 50 个命令 |
| ADR-10 | 配对同意用语义钩子注入，非 UI 点弹窗 | 系统弹窗自动化是 flaky 之源 | uiautomator 点系统对话框 |

## 9. 里程碑与验收标准

依赖驱动排序，每个里程碑是下一个的地基；验收标志全部可执行验证。

| # | 内容 | 验收标志 |
|---|---|---|
| M0 | 门控骨架：feature + env + Token + health/version + 产物扫描脚本 | 正常构建产物扫描无特征串；测试构建 curl health 通、错 Token 得 401 |
| M1 | 可观测地基：环形缓冲 + logs/tail + 前端 console 桥 | 前端 `console.error` 能按 seq 从 HTTP 拉到 |
| M2 | UI 通道：bridge 往返 + tree/navigate/click/input/text/wait | CLI 一条命令完成切页→点击→断言文案；**testid 命名表定稿（OQ-2）** |
| M3 | 状态断言：TestSnapshot + state/wait | 对真实传输过程进度断言成功；**快照字段定稿（OQ-1）** |
| M4 | 证据收尾：screenshot + invoke 白名单 | 单机最小场景（nav→操作→断言→截图→拉日志）闭环 |
| M5 | 部署与重置：SSH 链路 + 三级重置 + fixtures | 导演机一键将两台 PC 从干净态拉起到就绪 |
| M6 | 跨机闭环：PC↔PC 场景 + journal + 证据收割 + 报告 | 人为注入一次失败，现场包完整可复盘 |
| M7 | Android：logcat 桥 → testTag 铺设 → adb 工具层 → 配对钩子 | adb 通道完成同级别"看-点-断言-截图-日志" |
| M8 | 跨端与中继：PC↔Android 配对/传输、relay fixture 场景 | 跨端全流程场景通过 |
| M9 | 场景库与契约收口：手工清单逐条脚本化、三份契约文档终稿 | 发版回归 = 一条命令出报告 |

顺序核心逻辑：M0→M1"能安全地看见"；M2→M4"能可靠地操作与断言"；M5→M6"跨机闭环"；M7→M8 扩到三端；M9 收敛成资产。

## 10. 体系自身的验证策略（dogfooding）

- test-api 的 Rust 模块自带单元测试：Token 常量时间比较、oneshot 配对与超时、环形缓冲 seq 连续性、state/wait 求值器（各 op、超时路径）；
- testBridge 用 vitest（jsdom）：选择器解析、v-model 兼容输入、tree 剪枝规则；
- **每个里程碑的验收标准本身编写为冒烟场景**，体系用体系验证——M6 起所有早期验收项变成常驻回归；
- 契约一致性：端点表 ↔ axum 路由注册的静态比对脚本（防止文档与实现漂移）。

## 11. 开放问题（实现期决策点，不阻塞 spec 冻结）

| # | 问题 | 决策时点 |
|---|---|---|
| OQ-1 | TestSnapshot 具体字段终稿（依 AppState 实际内部结构裁剪） | M3 |
| OQ-2 | testId/testTag 命名清单（依 4 页面 7 组件与 Compose 屏幕盘点） | M2 起持续维护 |
| OQ-3 | `targets.local.yaml` 最终 schema | M5 |
| OQ-4 | ADBKeyboard 分发方式（fixtures 内置 apk vs 环境文档指引安装） | M7 |
| OQ-5 | 发现接口绑定配置的产品形态（临时配置 vs 正式设置项） | M5（涉及 core，需评估） |
| OQ-6 | relay fixture 端口与生命周期管理细节 | M8 |
| OQ-7 | Android debug HTTP 服务是否需要（logcat 够用则不做） | M7 后评估 |
| OQ-8 | 环形缓冲容量/丢弃策略的实证参数 | M1 定初值，M6 校准 |

## 12. 风险与对策

| 风险 | 对策 |
|---|---|
| eval 往返联调不顺（唯一技术未知点） | 退路：HTTP 轮询结果端点（糙但可靠）；M2 首先验证此机制 |
| AppState 锁竞争影响被测行为 | 快照函数持锁极短、只读；不在持锁中做序列化以外的计算 |
| 场景 flaky | 双层等待 + 禁裸 sleep + 每次交互重 dump + 环境校验前置 |
| testid 契约腐化 | 命名表入 review + 契约一致性脚本 |
| 测试面泄漏进正式产物 | 三重门 + 产物扫描进构建脚本（NFR-1） |
| 测试环境网络漂移 | 静态 IP + 校验脚本 + 发现接口绑定配置 |

## 13. 交付物清单

**代码**：
- `src-tauri/src/test_api/`（feature `test-api`）：服务器、认证、bridge 配对、环形缓冲、快照、截图、白名单
- `src-tauri/` 构建脚本追加产物扫描
- `ui/src/test-support/testBridge.ts` + console 日志桥 + data-testid 铺设
- `android/`：logcat 桥、testTag 铺设、配对钩子、NDK 路径配置化（debug 门控）
- `tests/e2e/`：CLI、Target 抽象、场景 SDK、journal、证据收割、报告、fixtures、环境校验

**文档（随实现产出、随 M9 终稿）**：
- `docs/contracts/test-api.md`：端点、错误码、包络、快照 schema
- `docs/contracts/testid-naming.md`：桌面与 Android 共用命名表
- `docs/e2e-harness.md`：环境搭建（SSH/防火墙/IME/USB）、Token 获取、执行方法

---

*本 spec 冻结后如需变更，走变更记录（附于文末），并同步影响到的契约文档。*
