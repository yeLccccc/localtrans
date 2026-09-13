# 契约 · 桌面 test-api HTTP 服务（v1）

| 项 | 值 |
|---|---|
| 来源 spec | `docs/specs/0001-e2e-test-automation.md` §7.1 |
| 状态 | M0-M4 已实现（health/version/logs-tail/ui-*/state-*/screenshot/invoke/test-*）；M6 扩列 invoke 白名单（传输发起三命令）（M9 终审） |
| 门控 | cargo feature `test-api`（默认关） + `LOCALTRANS_TEST_API=1` + 产物扫描 |

本文是编排器对接 test-api 的唯一契约入口。错误码为封闭集合，新增/变更须先改本文再改实现。

## 1. 门控与安全（三重门，spec §7.1.1）

| 门 | 机制 |
|---|---|
| 编译期 | cargo feature `test-api` 默认关闭，`axum` 等 optional，正式构建不进依赖树 |
| 运行期 | `LOCALTRANS_TEST_API=1` 才监听，否则代码路径完全不触网 |
| 发布期 | `scripts/check-release-clean.sh` 扫描 release 产物特征串，命中即构建失败 |

### 环境变量

| 变量 | 含义 | 默认 |
|---|---|---|
| `LOCALTRANS_TEST_API` | `1` 才启用服务 | 未设（不监听） |
| `LOCALTRANS_TEST_API_BIND` | 监听地址（建议测试网段） | `0.0.0.0` |
| `LOCALTRANS_TEST_API_PORT` | 监听端口 | `39871` |
| `LOCALTRANS_TEST_API_KEY` | Bearer Token，优先级最高 | 见下 |

### Token 来源

`LOCALTRANS_TEST_API_KEY` > app 数据目录 `test-api.key`（32 字节随机数的 hex，64 字符；不存在则首次启动生成写入，类 unix 平台 0600，Windows 尽力而为）。

### 认证

- `GET /api/health` 免认证（仅存活探测）；
- 其余所有端点要求 `Authorization: Bearer <token>`；
- 校验为常量时间比较；失败时服务端记 warn 日志（含来源 IP）、延迟 500ms 再返回 401。

### 全局响应头

所有响应携带 `X-LocalTrans-TestAPI: 1`（该字面量即发布期产物扫描的特征串，勿改）。

## 2. 响应包络（spec §7.1.2）

统一包络，所有端点一致：

```jsonc
// 成功
{ "ok": true, "data": { /* ... */ }, "meta": { "apiVersion": 1, "runId": "..." } }
// 失败
{ "ok": false, "error": { "code": "ELEMENT_NOT_FOUND", "message": "...", "detail": { "selector": "..." } } }
```

- `meta.runId` 在 `test/begin` 之前缺省（M0 的 health/version 不含 runId）；
- `error.detail` 为可选对象，按错误码补充上下文。

## 3. 错误码表（封闭集合）

| HTTP | code | 含义 | 实现状态 |
|---|---|---|---|
| 401 | `AUTH_FAILED` | token 错误/缺失 | 已实现（M0） |
| 400 | `BAD_REQUEST` | 参数错误 | 已实现（M1：logs/tail 的 afterSeq 非法；M2：ui/* 入参校验与请求体 JSON 解析失败、前端桥 BAD_REQUEST 透传；M3：state/wait 入参校验） |
| 403 | `INVOKE_NOT_ALLOWED` | 命令不在白名单（detail 列出允许项） | 已实现（M4：`/api/invoke` 白名单外命令，detail.allowed 见 §5.5） |
| 404 | `ELEMENT_NOT_FOUND` | 选择器无匹配 | 已实现（M2：click/input/text 指定 selector 时短轮询 ~1s 仍无匹配） |
| 404 | `NOT_FOUND` | 未知路径或方法不匹配（统一 fallback 包络） | 已实现（M4：Router fallback 与 method_not_allowed_fallback，带特征头） |
| 408 | `WAIT_TIMEOUT` | 等待条件超时（detail 带最后一次观测值） | 已实现（M2：ui/wait 前端内部超时 + Rust 兜底超时；M3：state/wait 超时 detail 带 lastValue/polls/条件全文） |
| 409 | `BRIDGE_NOT_READY` | 前端无 testBridge（构建不匹配） | 已实现（M2：ui/* 在前端握手前置位前调用） |
| 500 | `INTERNAL` | 内部错误（带 tracing 关联 id） | 已实现（M2：eval 失败/通道中断/前端未知错误码） |

## 4. 版本握手

`GET /api/version` 返回：

```jsonc
{ "ok": true,
  "data": { "appVersion": "0.12.0", "apiVersion": 1, "bridgeReady": false, "buildProfile": "release" },
  "meta": { "apiVersion": 1 } }
```

编排器每次连接先调它：`apiVersion` 不符或 `bridgeReady` 缺失立即终止场景。`bridgeReady` 由前端 testBridge 启动握手置位（M2 起生效，读 BridgeRegistry 真实状态；M0/M1 恒 `false`）。

## 5. 端点清单（v1）

| 端点 | 方法 | 认证 | 入参 → 出参要点 | 状态 |
|---|---|---|---|---|
| `/api/health` | GET | 免 | 存活 + bridgeReady（M2 起读真实握手状态） | **已实现（M0，bridgeReady 实化于 M2）** |
| `/api/version` | GET | 需 | 版本握手（见 §4） | **已实现（M0，bridgeReady 实化于 M2）** |
| `/api/ui/tree` | GET | 需 | → 元素摘要数组（spec §7.1.4，见 §5.2） | **已实现（M2）** |
| `/api/ui/navigate` | POST | 需 | `{path}` → 路由结果 | **已实现（M2）** |
| `/api/ui/click` | POST | 需 | `{selector}` → 元素文本回传 | **已实现（M2）** |
| `/api/ui/input` | POST | 需 | `{selector, value, clear?}` | **已实现（M2）** |
| `/api/ui/text` | GET | 需 | `{selector?}` → innerText（缺省全文） | **已实现（M2）** |
| `/api/ui/wait` | POST | 需 | `{selector \| text, timeoutMs}` → 200/408 | **已实现（M2）** |
| `/api/state/app` | GET | 需 | → Pinia 聚合快照（带 schemaVersion，见 §5.3） | **已实现（M3）** |
| `/api/state/transfers` | GET | 需 | → Rust TestSnapshot（spec §7.1.6，见 §5.3） | **已实现（M3）** |
| `/api/state/wait` | POST | 需 | `{source, path, op, value, timeoutMs, intervalMs}` → 200/408（见 §5.3） | **已实现（M3）** |
| `/api/logs/tail` | GET | 需 | `?afterSeq=&level=&target=&runId=` → 增量日志 + 游标（过滤语义见 §5.1） | **已实现（M1，runId/step 关联实化于 M4）** |
| `/api/screenshot` | GET | 需 | `?restore=true` → `image/png` 原始字节，meta 放响应头（见 §5.4） | **已实现（M4）** |
| `/api/invoke` | POST | 需 | `{cmd, args}` → 白名单内命令结果（见 §5.5） | **已实现（M4）** |
| `/api/test/begin` | POST | 需 | `{scenario}` → `runId`（见 §5.6） | **已实现（M4）** |
| `/api/test/step` | POST | 需 | `{name}` → 步骤标记入日志流 | **已实现（M4）** |
| `/api/test/end` | POST | 需 | `{runId, outcome}` → 统计 | **已实现（M4）** |

选择器语法（ui/* 端点，M2 生效）：CSS 选择器 + `[testid=x]` 简写 + `^=` 前缀匹配（应对 `transfer-card-{id}` 类动态 id）。`[testid=x]` 由前端桥翻译为 `[data-testid="x"]`（支持引号值与 `$=`/`*=`/`~=` 等操作符、可嵌套组合），命名表见 `docs/contracts/testid-naming.md`。

### 5.1 `/api/logs/tail` 过滤语义（M1 已实现）

数据源：tracing 环形缓冲 layer（容量 10000 条，满弹出最老；仅 test-api 构建挂载）。
条目字段：`{seq, ts, level, target, message, runId, step}`（`ts` 为 epoch 毫秒；`runId`
在 `test/begin`…`test/end` 窗口内写入的条目上出现（M4 起生效），`step` 在
`test/step` 之后出现；窗口外二者的键缺省）。`message` 为事件的 message 字段，
其余非空字段以 ` k=v` 追加在尾部（前端桥上报的条目形如
`logBridge attached level=info route=null`，`target=ui`）。

查询参数（均可选，空串视为未提供）：

| 参数 | 语义 |
|---|---|
| `afterSeq` | 只取 **seq 严格大于** 它的条目；缺省 0 = 从头拉。非法值（非非负整数）→ 400 `BAD_REQUEST` |
| `level` | **精确匹配某一级**（`TRACE/DEBUG/INFO/WARN/ERROR`，大小写不敏感）。不是阈值——查 `ERROR` 不会带回 `WARN`/`INFO` |
| `target` | **前缀匹配**：`target=ui` 同时命中 `ui`、`ui::child` 与一切以 `ui` 开头的 target |
| `runId` | **精确匹配**；缺省不过滤（M1 阶段所有条目均无 runId，传了必空） |

响应 `data`：

```jsonc
{ "entries": [ /* LogEntry[]，单次上限 500 条 */ ],
  "nextSeq": 7 /* 下次查询应传的 afterSeq */ }
```

`nextSeq` 游标语义（与"严格大于"配套，重要）：

- 有命中 → **本次返回的最后一条 seq**。下次传 `afterSeq=nextSeq`，严格大于语义
  下既不重复也不丢条（不要用 last+1，否则每次整页翻页会漏一条）；
- 无命中 → 跳到缓冲已分配的最高 seq（被过滤/被淘汰的条目无需重扫），
  且不低于调用方的 `afterSeq`（游标不倒退）。

增量轮询建议：`afterSeq ← data.nextSeq` 循环；`afterSeq` 落后于缓冲头部
（容量淘汰）时拉到的是从幸存条目开始的增量，游标照常前进。

### 5.2 UI 通道语义（M2 已实现：`/api/ui/*` 六端点）

**桥机制（spec §7.1.4 往返时序）**：HTTP 线程分配单调递增 id → 在主 webview
`eval("window.__testBridge&&window.__testBridge.exec(<请求JSON字符串>)")`（请求
整体作为一个 JS **字符串字面量** 嵌入，杜绝转义歧义；ExecuteScript 通道不受
页面 CSP 限制）→ 注册 oneshot 等待；前端执行完
`invoke('test_bridge_result', {id, ok, payload})`（payload 为数据本体或
`{code, message}`）唤醒对应 oneshot。多请求可并发在途，id 全局单调递增。

**就绪握手**：前端测试构建（`vite --mode test-api`，由 `VITE_TEST_API=1` 门控）
启动早期 `invoke('test_bridge_hello')` 置位 `bridgeReady`（`/api/health`、
`/api/version` 如实上报）。未就绪时所有 `ui/*` 端点立刻 **409 `BRIDGE_NOT_READY`**，
message 提示需以 `npm --prefix ui run dev:test`（开发）/ `build:test`（打包）
构建前端——把"前端忘了测试构建"从超时玄学变成一句话报错。Rust 侧两个
Tauri 命令无条件注册、函数体 feature 门控（非 test-api 构建返回 Err）。

**超时语义（双层）**：

- 前端内部超时：`ui/wait` 的 `timeoutMs`（缺省 10000，上限 120000），命中即回；
  前端侧超时回 `WAIT_TIMEOUT`（408）；
- Rust 兜底超时：等待前端回包 `max(15s, wait.timeoutMs + 5s)`（非 wait 动作固定
  15s）。超时回 408 `WAIT_TIMEOUT`（此时前端回包若迟到会记
  `test_bridge_result 未命中在途请求` warn，不影响后续请求）。

**各端点行为要点**：

| 端点 | 校验（失败 400 `BAD_REQUEST`） | 成功 `data` |
|---|---|---|
| `GET /api/ui/tree` | 无入参 | 元素摘要数组（见下） |
| `POST /api/ui/navigate` | `path` 非空且以 `/` 开头 | `{path}`（导航完成后实际路由） |
| `POST /api/ui/click` | `selector` 非空 | `{text}`（目标元素文本，截 200 字符） |
| `POST /api/ui/input` | `selector` 非空、`value` 必须字符串（可为空串=清空）、`clear` 可选布尔 | `{value}`（写入后的控件值） |
| `GET /api/ui/text` | 无（`?selector=` 可选） | 字符串（缺省整个 body 文本） |
| `POST /api/ui/wait` | `selector`/`text` 二选一、`timeoutMs` 可选非负整数 | `{matched: true}` |

**tree 元素摘要**（spec §7.1.4 序列化契约，编排方"看页面"的唯一入口）：

```jsonc
{ "tag": "button", "testId": "send-btn", "role": "button",
  "text": "发送", "value": null, "disabled": false,
  "visible": true, "rect": [x, y, w, h] }
```

- 收录集合：`button, input, select, textarea, a, [role], [data-testid]`；
- 剪枝：布局不可见（`offsetParent == null` 且无 client rects——后者兜底
  `position: fixed` 元素，如底部导航，spec 原文的 offsetParent 单一信号会误杀）；
  自身或祖先 `aria-hidden="true"`；
- `role`：显式 `role` 属性优先，否则隐式 ARIA 映射（button/a[href]/input[type]/
  select→combobox/textarea→textbox）；`text` 空白折叠、截 80 字符；
- `value`：表单控件值，其余 null；`rect`：`getBoundingClientRect` 四舍五入。

**前端执行语义**：元素查找带短轮询（~1s，容忍渲染间隙），仍无匹配 → 404
`ELEMENT_NOT_FOUND`；`input` 用原型链原生 value setter + 派发 `input`/`change`
（v-model 兼容）；`wait` 用 MutationObserver + 100ms 轮询双通道，命中即回；
前端动作失败按封闭错误码回传（未知码由 Rust 按 `INTERNAL` 包装）。

### 5.3 状态断言语义（M3 已实现：`/api/state/*` 三端点）

断言原则（spec §5 设计原则 2）：**判定基于状态快照**——`state/transfers` 与
`state/app` 是只读快照数据面，`state/wait` 是服务端条件等待（spec §7.1.5
双层等待的第二层；第一层 `ui/wait` 见 §5.2）。

#### 5.3.1 `GET /api/state/transfers` → Rust TestSnapshot

数据源：Rust 壳 `AppState`（内存态，不触网）。取锁只做克隆/收集、序列化在
锁外（与只读 Tauri 命令同习惯）。`schemaVersion: 1`，serde camelCase：

```jsonc
{
  "schemaVersion": 1,
  "selfDevice": { "id": "<指纹hex64>", "name": "设备名", "shortCode": "1234", "hidden": false },
  "devices": [
    { "id": "<指纹hex64>", "name": "A 机", "addr": "192.168.1.5:47601",
      "online": true, "connected": true, "viaRelay": false }
  ],
  "sessions": [ { "peer": "<指纹hex64>", "trusted": true, "name": "A 机" } ],
  "transfers": [
    { "id": "<job_id hex16>", "name": "大文件.bin", "direction": "pull",
      "state": "active", "bytesDone": 1024, "bytesTotal": 2048, "speedBps": 512,
      "peer": "<指纹hex64>", "remoteDone": 0, "instant": false, "failReason": null }
  ],
  "cards": [
    { "cardId": "<card_id hex16>", "engineId": "<engine job hex16>|null",
      "state": "done", "bytesDone": 2048, "bytesTotal": 2048,
      "terminal": true, "removed": false }
  ],
  "discoveryStats": null   // 暂缺，见下表
}
```

字段语义与来源：

| 字段 | 语义 | 来源 |
|---|---|---|
| `selfDevice` | 本机身份 | identity 指纹/短码 + config 设备名 + hidden 原子位 |
| `devices[]` | 合并视图：本地发现 + 中结名册 + 信任表兜底（离线已配对设备常驻），与 UI 设备页同源同序（core `device_merge`：connected > online > name 排序） | `devices` + `relay_roster` + `connected_fps` + 信任表 || `sessions[]` | **QUIC 会话在连**的对端（`connected` 只代表广播可见，这里才是连接真的活着）；按指纹排序；`trusted` 查信任表；`name` 查设备合并视图（查不到 null） | `connected_fps` + 信任表 |
| `transfers[]` | **活动传输**（未 removed 的卡片，与前端 `TransferDto` 同源；`id` 即前端 `job_id`） | `transfers` 表 |
| `cards[]` | **全部卡片**（含 removed，断言两级删除用），按 cardId 升序；`terminal` = done/failed/interrupted（终态吸收一切事件） | `transfers` 表 |
| `discoveryStats` | 发现层广播收发计数 | **暂缺：null** |

ID 口径：`transfers[].id` == `cards[].cardId` == 前端 `TransferDto.job_id`，
均为 **16 位小写 hex 字符串**（与 `localtrans_core::serde_compat::u64_hex_string`
一致）；`cards[].engineId` 同口径或 null（未绑定引擎）。字节/速率字段为数字。

> **字段名澄清（M6 核查）**：M5 执行记录曾报"devices[].address 序列化为
> undefined"——实测（双端 release 实例 + 单测钉死）该字段名即 **`addr`**
> （core `discovery::DeviceInfo.addr` → `device_merge::MergedDevice.addr` →
> `DeviceBrief.addr`），按 `.address` 取值自然 undefined，映射本身无 bug。
> 已在 `snapshot.rs` 加序列化钉死单测防回归与再误读。信任表兜底条目（对端
> 离线/跨网）`addr` 为空串（无地址信息）。

**暂缺登记**（spec 基线有、内部未暴露，置 null——待 core 暴露后接真值并升
`schemaVersion` 若破坏兼容）：

| 字段 | 原因 |
|---|---|
| `discoveryStats`（广播收发计数） | core `DiscoveryHandle` 只暴露设备 watch 表与命令通道，无计数器 |
| `sessions[].state`（spec 注释"状态"细化） | SessionManager 内部 Session 结构私有；会话活性即入表本身（在表=在连），无中间态可报 |

#### 5.3.2 `GET /api/state/app` → Pinia 聚合快照

数据面走 §5.2 桥机制：HTTP → eval `testBridge.exec({action:"state"})` →
前端四 store 各自 `toTestSnapshot()`（纯 JSON：JSON 往返剥响应式/函数/undefined）
→ `test_bridge_result` 回传。未就绪 → 409 `BRIDGE_NOT_READY`。

```jsonc
{
  "schemaVersion": 1,
  "devices":   { "schemaVersion": 1, "devices": [...], "pairingPending": [...],
                 "selectedFp": "<hex>|null", "reconnecting": ["<hex>"],
                 "loading": false, "error": null },
  "transfers": { "schemaVersion": 1, "transfers": [...], "resumeJobs": [...],
                 "diskJobs": [...], "loading": false, "error": null },
  "settings":  { "schemaVersion": 1, "config": {...}|null, "trustedPeers": [...],
                 "loading": false, "error": null },
  "toast":     { "schemaVersion": 1, "toasts": [ { "id": 0, "level": "success", "text": "..." } ] }
}
```

- `devices.devices[]` 字段与 Rust `DeviceDto`（蛇形）一致：`fingerprint/name/addr/online/connected/via_relay`；
- `transfers.transfers[]` 与 Rust `TransferDto`（蛇形）一致：`job_id/done/total/state/speed_bps/...`；
- `devices.reconnecting` 为数组（store 内是 Set）；`transfers.lastRequest`
  （失败重试的请求参数 Map）是内部簿记，**不进快照**；
- 与 `state/transfers` 的差异：这里是**前端看到的**状态（经 4Hz 事件/UI
  动作更新），断言 UI 表现用 app 源；断言传输真实进度用 transfers 源。

#### 5.3.3 `POST /api/state/wait` → 200 / 408 / 400

入参（全部校验失败 → 400 `BAD_REQUEST`）：

| 字段 | 必填 | 语义 |
|---|---|---|
| `source` | 是 | `transfers`（Rust TestSnapshot，每拍重建）/ `app`（Pinia 快照，每拍经桥取） |
| `path` | 是 | 点分路径：对象键 `a.b.c`、数组下标 `a.0.b`、数组长度 `a.length` |
| `op` | 是 | `eq` / `ne` / `gte` / `lte` / `contains` / `exists` / `empty` |
| `value` | op 相关 | eq/ne/gte/lte/contains 必填；exists/empty **不得传**（传了 400） |
| `timeoutMs` | 否 | 缺省 10000；0 = 只查一次即判负 |
| `intervalMs` | 否 | 缺省 **500**；可调小是给单测用的（生产场景勿低于 100，避免快照构建打满）；必须 ≥1 |

求值规则（封闭语义，勿单边改动）：

- **路径不存在**（键缺失/下标越界/对标量下钻/空段）→ `exists` 为 false，
  **其余 op 一律 false**（含 `ne`、`empty`）；`a` 显式为 null 是"存在且值为
  null"，与不存在可区分（`exists` true / `empty` true）；
- `eq`/`ne`：string/number/bool（数字经 f64 比较，1 与 1.0 相等；额外允许
  null≡null）；类型不匹配 eq 为 false、**ne 为 true**（不等于的语义）；
- `gte`/`lte`：仅 number，任一侧非数字 → false；
- `contains`：仅 string 包含，非字符串 → false；
- `empty`：空串 / 空数组 / null；数字 0、空对象 `{}` **不**匹配。

轮询：先查后判超时（首拍即中不等待）；每拍 `intervalMs`；`source=app`
每拍经桥往返——**桥错误（409 未就绪/408 桥超时/500）立即原样返回，不靠
轮询自愈**（桥兜底超时 15s，见 §5.2；wait 的 `timeoutMs` 可短于它，但条件
判定错误与桥通道错误是两类失败，分开报）。

成功 200 `data`：

```jsonc
{ "matched": true, "path": "devices.length", "op": "gte",
  "observed": 2, "polls": 1, "elapsedMs": 3 }
```

超时 408 `WAIT_TIMEOUT`，`error.detail` **带最后一次观测值**：

```jsonc
{ "ok": false,
  "error": { "code": "WAIT_TIMEOUT", "message": "等待条件超时（transfers devices.length gte 999，1500ms 内未满足）",
             "detail": { "source": "transfers", "path": "devices.length", "op": "gte",
                          "value": 999, "lastValue": 0, "polls": 4,
                          "timeoutMs": 1500, "intervalMs": 500 } } }
```

`lastValue` 为路径从未存在时为 null（与"观测到 null"同形，区分要靠条件本身）。

### 5.4 `/api/screenshot`（M4 已实现，spec §7.1.8）

`GET /api/screenshot?restore=true` → **`image/png` 原始字节**（全 test-api 唯一
非 JSON 包络端点；失败仍走 500 `INTERNAL` 包络）。截图只作证据，断言不得依赖像素。

**捕获路径（与 spec 设想的 hwnd 枚举不同，实测偏差）**：xcap 0.9.8 的
`Window::all()` 走 WebRTC 语义过滤，**排除当前进程自己的窗口**——test-api
运行在应用进程内，永远枚举不到主窗口。因此改用显示器裁剪：

1. tauri 主窗口 `outer_position`/`outer_size`（物理像素）得窗口矩形；
2. 匹配 xcap `Monitor::all()`：先按 tauri `current_monitor` 原点全等，兜底取
   包含窗口左上角的显示器；
3. 窗口矩形 ∩ 显示器矩形，平移为显示器相对坐标并钳制界内（最大化窗口带
   DWM 阴影越界的部分被裁掉；跨界两显示器时只留所匹配显示器内的部分）；
4. `Monitor::capture_region`（GDI 桌面 BitBlt，物理像素 1:1），捕获前
   `set_focus` 尽量把窗口提到前台消除遮挡。

| 项 | 语义 |
|---|---|
| `?restore=` | 缺省 `true`：最小化先 `unminimize` 并等 ~300ms 渲染稳定再截；显式 `false`/`0`（大小写不敏感）跳过还原。其余任意值按 true |
| 捕获 | 物理像素（阻塞调用已下放线程池） |
| 遮挡 | 桌面捕获语义：若仍有他窗遮挡会拍进证据（截图仅作证据，spec §5 原则 2） |
| DPI | `X-Scale` = 窗口所在显示器 scale_factor，两位小数字符串（如 `1.50`） |

响应头：`Content-Type: image/png`、`X-Width`/`X-Height`（PNG 实际像素）、
`X-Scale`（取不到时省略）、全局特征头 `X-LocalTrans-TestAPI: 1`。

### 5.5 `/api/invoke` 白名单（M4 已实现，spec §7.1.9）

`POST /api/invoke {cmd, args?}`（args 缺省 `{}`，必须为对象）。白名单为封闭清单
（实现 `src-tauri/src/test_api/invoke.rs` 的 `ALLOWED`），未列入一律 **403
`INVOKE_NOT_ALLOWED`**，`detail.allowed` 列出全部允许项（含 Class）；命令执行
失败 500 `INTERNAL`，detail 带 `{cmd, error}`。**不设全命令代理**：删除/解绑/
清信任/停止引擎/写配置/触发传输/文件系统任意读类命令绝不入列（硬重置走进程级方案）。

`Class ∈ {ReadOnly, Mutating}`；Mutating 每次调用服务端 `tracing::warn!` 高亮
（含 cmd 与当前 run/step 上下文）。当前清单：

| cmd | Class | 用途 |
|---|---|---|
| `get_settings` | ReadOnly | 读配置（ConfigDto） |
| `list_devices` | ReadOnly | 本地发现设备表 |
| `list_trusted` | ReadOnly | 信任表 |
| `list_transfers` | ReadOnly | 传输卡片列表（活动视图） |
| `get_pairing_pending` | ReadOnly | 配对等待列表（**B 侧含 own_code**——编排器读码兜底用） |
| `list_disk_jobs` | ReadOnly | 磁盘任务表 |
| `pending_resume_jobs` | ReadOnly | 待恢复任务 |
| `relay_status` | ReadOnly | 中继连接状态 |
| `get_network_status` | ReadOnly | 网络体检（5s 缓存） |
| `get_device_fingerprint` | ReadOnly | 本机指纹/短码 |
| `has_parts` | ReadOnly | 秒传分片目录存在性（args: `{job_id}`） |
| `get_business_card` | ReadOnly | M3a：本机名片文本（人可读多行，core `business_card` 序列化） |
| `list_channels` | ReadOnly | M3b：通道探测记录表（每设备每通道一条：`fingerprint`/`addr`/`via_relay`/`rtt_ms`/`est_bps`/`loss_rate`/`current`/`score_ready`/`probe_disabled`/`age_secs`；内存态不持久化，无会话返回 `[]`） |
| `probe_now_peer` | ReadOnly | M3c T2：手动单对端快检（args: `{fingerprint}`；当前通道 64KB 快检、掉 50% 升级全量，只更新内存通道表——通道面板「重新探测」同款路径，场景数据刷新编排用） |
| `clear_completed_transfers` | Mutating | 终态卡片 view 级清理（removed=true，数据保留） |
| `connect` | Mutating | M6：按指纹发起 QUIC 会话（args: `{fingerprint}`）；未配对对端会触发对端配对弹窗 |
| `push_files` | Mutating | M6：推送本地文件（args: `{fingerprint, local_paths: [绝对路径...]}`）；返回占位卡片 id（hex16） |
| `push_files_rel` | Mutating | M6：带结构推送（args: `{fingerprint, items: [[路径, 相对目录]...]}`）；返回占位卡片 id（hex16） |
| `add_by_card` | Mutating | M3a：粘贴名片添加（args: `{text: 名片全文}`）；解析→逐地址单播探测（入 probe_targets 重探表）→返回 `{accepted, addresses_tried}`，各地址"是否真的出现"经 manual-probe-result 事件异步反馈；本机自身名片 accepted=false |

**M6 传输发起入列理由**（与 M4 的"触发传输不入列"边界修订）：UI 的文件选择
（`push_files`/`push_files_rel` 的上游）是 OS 原生对话框，WebView DOM 无法驱动，
传输场景必须语义注入；`connect` 是其前置（建会话）。参数形状无路径白名单——
Token 门控下编排器是信任操作者（fixture 目录随仓库演进，穷举过滤反而脆）。
**配对确认仍不入列**（`grant_consent`/`deny_consent`/`submit_pair_code`）：
配对是安全敏感交互，场景脚本走 UI 点击（`btn-grant`、`code-input`）保真实
用户流程；B 侧亮出的配对码优先经 `ui/text` 读屏、`get_pairing_pending` 作兜底。

明确排除示例：`remove_transfer`/`destroy_disk_job`/`remove_trusted`/`remove_share`
（破坏性）、`save_settings`/`set_*`（写配置）、`start_download*`（下载发起——
浏览页可 UI 驱动，无须注入）、`grant_consent`/`deny_consent`/`submit_pair_code`
（配对确认，见上）、`prepare_shutdown`（停引擎）、`add_firewall_rule`（弹 UAC）、
`open_*_dir`（打开资源管理器）、`expand_local_paths`（文件系统任意读，NFR-8）。

### 5.6 `/api/test/begin|step|end` run/step 关联（M4 已实现，spec §7.1.7-3）

| 端点 | 入参 | 行为 |
|---|---|---|
| `POST /api/test/begin` | `{scenario}`（非空） | 生成 `runId`（16 字节随机数 hex，32 字符），置入共享 TestContext 并**清空 step**；打"test run 开始"标记（该条起所有日志带 runId）。返回 `data.runId` |
| `POST /api/test/step` | `{name}`（非空） | 置当前 step 并打"test step"标记；后续日志条目带 step 字段。**无活动 run → 400** |
| `POST /api/test/end` | `{runId, outcome}`（均非空） | 校验 runId 与当前活动 run 一致（不匹配/无活动 run → 400，上下文不变）；打"test run 结束"标记后统计环形缓冲中该 runId 的条数，清空 run/step。返回 `data.{runId, outcome, entries}` |

- `begin` 会**替换**当前活动 run（重复 begin 不报错，旧 runId 的日志已写入不可回溯）；
- `outcome` 为编排器自定义字符串（pass/fail/aborted…），服务端只透传进日志标记；
- begin 与 end 之间产生的**所有**日志（Rust tracing + 前端桥 target=ui）带
  `runId`，`test/step` 之后带 `step`——事后 `logs/tail?runId=` 即可提取按步骤
  分段的完整时间线；`test/end` 的统计为幸存条目口径（被容量淘汰的不计）。


## 6. 变更记录

| 日期 | 变更 |
|---|---|
| 2026-09-06 | M0 建骨架：包络/错误码/端点清单抄自 spec §7.1.2、§7.1.3；health/version 标已实现 |
| 2026-09-06 | M1：`/api/logs/tail` 标已实现，补 §5.1 过滤/游标语义；BAD_REQUEST 状态更新（afterSeq 非法） |
| 2026-09-06 | M2：`/api/ui/*` 六端点标已实现，补 §5.2 桥机制/就绪握手/双层超时/tree 契约/选择器语义；ELEMENT_NOT_FOUND、WAIT_TIMEOUT、BRIDGE_NOT_READY、INTERNAL 状态更新；health/version 的 bridgeReady 实化为真实握手状态 |
| 2026-09-06 | M3：`/api/state/*` 三端点标已实现，补 §5.3 快照 schema（TestSnapshot 字段表 + 暂缺登记 + Pinia 聚合快照）与 state/wait 求值规则（点分路径/全 op 矩阵/轮询与桥错误语义）；WAIT_TIMEOUT detail 语义实化 |
| 2026-09-06 | M4：`/api/screenshot`、`/api/invoke`、`/api/test/begin|step|end` 标已实现，补 §5.4（截图/restore/响应头）、§5.5（invoke 白名单清单与排除项）、§5.6（run/step 关联语义）；错误码表新增 `NOT_FOUND`（未知路径统一 fallback 包络）；logs/tail 条目的 runId/step 实化 |
| 2026-09-06 | M6：§5.5 白名单扩列 `connect`/`push_files`/`push_files_rel`（Mutating，传输发起语义注入——原生文件对话框无法 DOM 驱动；配对确认仍排除走 UI）；§5.3.1 补 `addr` 字段名澄清（M5 误读结论） |
| 2026-09-08 | M3a：§5.5 白名单扩列 `get_business_card`（ReadOnly，本机名片文本）与 `add_by_card`（Mutating，粘贴名片添加——写 probe_targets 重探表并可触发对端配对弹窗，语义同 connect；card-exchange 场景编排入口） |
| 2026-09-08 | M3b：§5.5 白名单扩列 `list_channels`（ReadOnly，通道探测记录表——M3b 探测选路观测面，routing-probe 场景探测记录断言数据源；数据=AppState.channels，core::routing::ChannelTable 内存态） |
| 2026-09-08 | M3c T2：§5.5 白名单扩列 `probe_now_peer`（ReadOnly，手动单对端快检——通道面板「重新探测」同款路径，只更新内存通道表；场景数据刷新编排用）。强制走中继开关不走白名单（`set_*` 写配置排除原则不变），e2e 经 ⋮ 菜单 UI 点击驱动 |
