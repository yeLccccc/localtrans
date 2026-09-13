# PLAN-0001 · 自动化测试体系实现计划（M0–M4 执行版）

| 项 | 值 |
|---|---|
| 日期 | 2026-09-06 |
| 状态 | 执行中 |
| 上游 spec | `docs/specs/0001-e2e-test-automation.md`（契约以 spec 为准，本计划只定实现路径与验证） |
| 执行模型 | 每个里程碑一个 subagent，顺序执行（后级依赖前级）；每个任务完成后必须通过"验收命令"门禁才进入下一个 |
| 本期范围 | **M0–M4（纯开发机，无硬件依赖）**；M5–M9 依赖测试机/手机到位后另行细化任务 |

## 0. 全局约定（所有 subagent 必须遵守）

1. **禁止 `git commit` / `git push`**；工作区已有未提交改动（jniLibs、src-tauri/Cargo.toml、target.android/），只做增量修改，**不得回退他人改动**。
2. **既有测试不得破坏**：每轮结束 `cargo test --workspace` 与 `npm --prefix ui test` 必须全绿（新增失败即修）。
3. 命名与常量（与 spec §7.1 对齐，不得擅改）：
   - cargo feature：`test-api`（默认关）；optional 依赖：`axum`（0.8 系）、`xcap`（最新稳定）
   - 环境变量：`LOCALTRANS_TEST_API=1` 启用；`LOCALTRANS_TEST_API_KEY`；`LOCALTRANS_TEST_API_BIND`（默认 `0.0.0.0`）；`LOCALTRANS_TEST_API_PORT`（默认 **39871**）
   - 特征串（产物扫描目标）：所有响应带 HTTP 头 **`X-LocalTrans-TestAPI: 1`**
   - 响应包络/错误码：spec §7.1.2；端点清单：spec §7.1.3
4. **Tauri 命令注册策略**：`ui_log`、`test_bridge_result`、`test_bridge_hello` 无条件注册（避免 `generate_handler!` 内 cfg 问题），函数体在非 test-api 构建下为 no-op/返回 Err。UI 相关命令不受 feature 门控（前端日志桥在正式版也有价值，FR-7 不要求门控）。
5. **前端测试构建**：vite `--mode test-api` + `ui/.env.test-api`（`VITE_TEST_API=1`）；新增 npm scripts `dev:test` / `build:test`。正式 `dev`/`build` 不受影响。
6. tokio 需要的 features（net 等）按需在 src-tauri 依赖上追加（workspace 依赖可叠加 features）。
7. 每完成一个 Task：在本文档末尾"执行记录"表追加一行（Task、结果、关键产物、偏差）。

---

## Task M0 · 门控骨架（feature + env + Token + health/version + 产物扫描）

**涉及文件**：`src-tauri/Cargo.toml`、`src-tauri/src/main.rs`、新建 `src-tauri/src/test_api/mod.rs`（及子模块）、新建 `scripts/check-release-clean.sh`、新建 `docs/contracts/test-api.md`（骨架）。

**实现要点**：
1. Cargo.toml：`[features] test-api = ["dep:axum", "dep:xcap"]`；axum/xcap optional。axum 0.8，按需 enable 基础 features（json、tokio）。
2. `test_api/mod.rs`：`start_test_api(app: AppHandle)`（feature 门控）——读 env（开关、bind、port、key），生成/读取 `test-api.key`（app 数据目录，hex 32B，写文件 0600 尽力而为），axum Router + 中间件：Bearer 校验（常量时间比较，手写 XOR 累加即可）+ 全局响应头 `X-LocalTrans-TestAPI: 1`；认证失败记 tracing（含来源 IP）+ 500ms 延迟 + 401 包络。
3. 端点：`GET /api/health`（`{status:"ok", bridgeReady:false}`）、`GET /api/version`（appVersion 取 tauri PackageInfo，apiVersion=1，bridgeReady 占位，buildProfile=debug/release）。`/health` 免认证，其余全部需要。
4. main.rs：setup 中调用 `test_api::start_test_api(app)`（内部 `#[cfg(feature)]`）；日志打印监听地址。
5. `scripts/check-release-clean.sh`：对指定二进制 grep 字节串 `X-LocalTrans-TestAPI`，命中退出 1（bash 实现，cygpath 兼容）。
6. `docs/contracts/test-api.md`：把 spec §7.1.2/§7.1.3 抄为契约文档骨架（后续里程碑持续补齐）。

**单元测试**：Token 常量时间比较（等长/不等长）、key 生成格式、包络序列化、401 路径（axum tower::ServiceExt oneshot 测试或抽中间件函数直测）。

**验收命令**：
- `cargo check -p localtrans`（无 feature）与 `cargo check -p localtrans --features test-api` 均过
- `cargo test -p localtrans` 全绿
- `cargo build --release -p localtrans` 后 `scripts/check-release-clean.sh target/release/localtrans.exe` 退出 0；`cargo build --release -p localtrans --features test-api` 后同一脚本退出 1

## Task M1 · 可观测地基（环形缓冲 + logs/tail + 前端 console 桥）

**涉及文件**：`src-tauri/src/test_api/`（新 ring.rs、logs 端点）、`src-tauri/src/main.rs`（subscriber 组装）、`ui/src/main.ts`、新建 `ui/src/lib/logBridge.ts`、`src-tauri/src/commands.rs` 或 main.rs（`ui_log` 命令）。

**实现要点**：
1. `ring.rs`：自定义 `tracing_subscriber::Layer`，事件进 `VecDeque`（Mutex），容量 10000 满则弹出头部；字段 `{seq(AtomicU64), ts, level, target, message, runId, step}`；message 取 `message` 字段，其余字段附加 k=v。runId/step 从线程本地或全局 test 上下文读取（空则 null）。
2. main.rs 初始化处：现有 subscriber 追加该 layer（test-api 构建才挂；非 test 构建零开销）。注意与现有 env-filter/rolling appender 共存。
3. `GET /api/logs/tail?afterSeq=&level=&target=&runId=`：过滤 + 增量 + `nextSeq` 游标，单次上限 500 条。
4. 前端 `logBridge.ts`：仅 Tauri 环境（有 `window.__TAURI__`）启用——拦截 `console.warn/error`、`app.config.errorHandler`、`unhandledrejection`；50ms 批量 `invoke('ui_log', {level, message, route})`；route 由 router.afterEach 维护；**不得破坏 vitest/jsdom 与浏览器 mock 模式**（非 Tauri 环境完全 no-op）。
5. `ui_log` 命令：无条件注册，非 test-api 构建直接写 `tracing::info!(target:"ui", ...)`（这样正式版前端日志也进文件——有价值）；test-api 构建同样经 tracing 进环形缓冲。

**单元测试**：环形缓冲 seq 连续/容量淘汰/过滤；`ui_log` 命令；logBridge 用 vitest（mock invoke，验证批处理与 no-op 分支）。

**验收命令**：
- `cargo test -p localtrans --features test-api` 全绿；`npm --prefix ui test` 全绿
- 手动冒烟（脚本化进 `tests/e2e/smoke-m1.mjs`，Node 18+ 零依赖）：`LOCALTRANS_TEST_API=1` 启动 dev 构建 → curl `/api/health` → 前端触发一条 console.error（经 bridge）→ `logs/tail` 能按 seq 拉到 `target=ui` 记录 → 关进程

## Task M2 · UI 通道（testBridge 往返 + ui/* 端点 + testid 命名表）

**涉及文件**：`ui/src/test-support/testBridge.ts`（新建）、`ui/src/main.ts`、`ui/.env.test-api`、`ui/package.json`（scripts）、`src-tauri/src/test_api/`（bridge.rs + ui 端点）、`src-tauri/src/main.rs`（`test_bridge_result`/`test_bridge_hello` 命令）、页面/组件补 `data-testid`、`docs/contracts/testid-naming.md`（新建）。

**实现要点**：
1. `testBridge.ts`：`import.meta.env.VITE_TEST_API==='1'` 时注册 `window.__testBridge = { exec(jsonString) }`，启动即 `invoke('test_bridge_hello')`；exec 按 spec §7.1.4 实现动作：`navigate`（router.push）、`click`、`input`（原生 setter+input/change 事件，v-model 兼容）、`text`、`tree`（§7.1.4 序列化契约，offsetParent/aria-hidden 剪枝，文本截 80 字符）、`wait`（selector/text，MutationObserver+100ms 轮询）。选择器：CSS 直用；`[testid=x]` → `[data-testid="x"]`。结果 `invoke('test_bridge_result',{id, ok, data|error})`。
2. Rust `bridge.rs`：`BridgeRegistry { ready: AtomicBool, map: Mutex<HashMap<u64, oneshot::Sender>> }`；handler：ready 检查（未就绪→409 `BRIDGE_NOT_READY`）→ id 分配 → eval(`window.__testBridge&&window.__testBridge.exec('...')`)，payload 为单 JSON 字符串字面量（防转义歧义）→ await oneshot，默认 15s 超时（408 包络）。并发多请求在途。`test_bridge_hello` 置 ready。
3. 端点：`/api/ui/tree|navigate|click|input|text|wait`（spec §7.1.3）。
4. testid 铺设：盘点 `ui/src/pages/`（4 页）+ `ui/src/components/`（7 组件），关键可交互元素补 `data-testid`；命名表落 `docs/contracts/testid-naming.md`（命名规范：kebab-case、区域-元素-动作、动态 id 用前缀匹配约定）。**改动只加属性，不动结构与逻辑**。
5. npm scripts：`dev:test`（`vite --mode test-api`）、`build:test`（`vite build --mode test-api`）+ `.env.test-api`。

**单元测试**：bridge registry 配对/超时/并发（Rust）；选择器翻译、tree 剪枝、v-model 输入序列（vitest + jsdom）。

**验收命令**：
- 全部单测绿
- 冒烟 `tests/e2e/smoke-m2.mjs`：`npm --prefix ui run build:test` 后 `cargo build --features test-api`（debug）→ 带 env 启动 exe → health 的 bridgeReady=true → `ui/navigate` 到 /settings → `ui/wait` 文本"设置" → `ui/tree` 含 `nav` 相关 testid → `ui/click` 一个无害按钮 → 关进程

## Task M3 · 状态断言（TestSnapshot + state 端点 + state/wait）

**涉及文件**：`src-tauri/src/test_api/snapshot.rs`（新建）、main.rs/commands.rs 只读接入、`ui/src/stores/` 各 store 增 `toTestSnapshot()`、`ui/src/test-support/testBridge.ts` 增加 `state` 动作。

**实现要点**：
1. 通读 `src-tauri/src/main.rs` 的 AppState 与 transfer_state.rs，映射为显式 `TestSnapshot`（spec §7.1.6 字段为基线，实际内部结构缺失的字段在契约文档中标注"暂缺"而非硬凑；`discovery_stats` 若 core 未暴露计数则先置 null 并记录）。持锁只做克隆/快照，序列化在锁外。
2. `/api/state/transfers` → TestSnapshot；`/api/state/app` → 前端 Pinia 快照（经 bridge `state` 动作，四个 store 的 `toTestSnapshot()`，含 schemaVersion）。
3. `/api/state/wait`：`{source, path, op, value, timeoutMs?}`；path 点分（`.length` 支持数组长度），op ∈ {eq,ne,gte,lte,contains,exists,empty}；500ms 轮询；超时 408 包络且 detail 带末次观测值。
4. 契约文档补快照 schema。

**单元测试**：路径求值器（全 op、越界、类型不匹配）；wait 的满足/超时（tokio::time::pause 或短超时）；TestSnapshot 从构造 AppState 的映射正确性。

**验收命令**：单测全绿 + 冒烟 `tests/e2e/smoke-m3.mjs`：启动后 `state/transfers` 返回 schema_version、devices 数组；`state/wait`（`devices.length gte 0`）立即成功；人为非法路径得 400。

## Task M4 · 证据收尾（screenshot + invoke 白名单 + 单机冒烟闭环）

**涉及文件**：`src-tauri/src/test_api/`（shot.rs、invoke.rs）、`tests/e2e/smoke-m4.mjs`、契约文档补全。

**实现要点**：
1. `/api/screenshot`：xcap 按窗口定位（优先 `window.hwnd()`，取不到再按标题 `LocalTrans 局域网互传` 过滤 `Window::all()`）；`?restore=true` 默认（最小化先还原）；返回 `image/png` 字节 + 头 `X-Scale/X-Width/X-Height`（meta 放响应头，避免改包络）。
2. `/api/invoke`：白名单 `ALLOWED: &[(&str, Class)]`（ReadOnly 起步：通读 commands.rs 选只读命令如设置读取/设备列表/卡片列表等；Mutating 暂只留 `clear_finished_cards` 类若存在，否则为空）；未列 → 403 包络 detail 列允许项；Mutating 调用 tracing 高亮。
3. `smoke-m4.mjs`：完整单机闭环——health → version → navigate → wait → click → state → wait-state → screenshot（存 `tests/e2e/reports/`）→ logs/tail → invoke 白名单一次（403 用例 + 200 用例各一）→ 退出码汇总。此脚本即体系第一条 dogfooding 回归。
4. 契约文档 `docs/contracts/test-api.md` 定稿到与实现一致（M9 再终审）。

**验收命令**：单测全绿；`node tests/e2e/smoke-m4.mjs` 在本机全绿且产出截图与日志文件。

---

## M5–M9（硬件就绪后另立任务卡，此处仅占位）

- M5 测试机 SSH 链路 + 三级重置 + fixtures（依赖：第二台 PC 到位）
- M6 PC↔PC 场景 + journal + 证据收割 + 报告（依赖 M5）
- M7 Android logcat 桥/testTag/adb 工具层/配对钩子（依赖手机到位）
- M8 跨端与中继场景（依赖 M6+M7）
- M9 场景库铺量 + 契约终审 + e2e-harness 文档

## 执行记录

| Task | 结果 | 关键产物 | 偏差 |
|---|---|---|---|
| M0 | 通过 | `src-tauri/Cargo.toml`（feature `test-api = ["dep:axum"]`，axum 0.8 optional）；`src-tauri/src/test_api/mod.rs`（env 门控 + Token 文件 + Bearer 常量时间校验 + 特征响应头中间件 + health/version 端点 + 9 个单测）；`main.rs` 挂载点；`scripts/check-release-clean.sh`；`docs/contracts/test-api.md`。验收：check/test（65 绿）/check+test --features test-api（74 绿）全过；release 干净产物扫描退出 0、test-api 产物扫描退出 1 | 按任务建议 xcap 不进本里程碑 feature（M4 再扩为 `["dep:axum","dep:xcap"]`）；未引 tower（中间件逻辑抽纯函数直测）；无 feature 单测默认不编译（由 `--features test-api` 覆盖） |
| M1 | 通过 | `src-tauri/src/test_api/ring.rs`（环形缓冲 Layer：容量 10000/AtomicU64 seq/TestContext run/step 占位/11 单测）；`mod.rs` 挂 `GET /api/logs/tail`（afterSeq/level/target/runId 过滤 + nextSeq 游标 + 500 上限 + 4 单测）；`main.rs` tracing 初始化改 Registry 组装（fmt layer + 可选 ring layer + EnvFilter，lossy 语义与原 with_env_filter 一致）；`commands.rs` 新增 `ui_log`（无条件注册）；前端 `ui/src/lib/logBridge.ts`（console/errorHandler/unhandledrejection 拦截 + 50ms 批量 + 500 字符截断 + attach 锚点）+ `main.ts` 接入 + `tauriMock.ts` 加 `__localtransMock` 标记 + 13 个 vitest；`ui/package.json` 补 `test` script；`tests/e2e/smoke-m1.mjs` 冒烟。验收：cargo test 65 绿 / --features test-api 89 绿 / npm test 92 绿 / smoke 退出 0（锚点 seq=6，游标续读不重复） | nextSeq 语义定为"本次最后一条命中 seq"（配合严格大于 afterSeq，翻页不丢条；原设想的 last+1 会整页漏一条，测试暴露后修正并写入契约）；浏览器 mock 检测靠 tauriMock 注入 `__localtransMock` 标记区分（比调用顺序更稳）；`npm test` script 原本缺失，本轮补 `"test": "vitest run"`；smoke 脚本会 taskkill 遗留 localtrans.exe（防 single-instance 转发拿错窗口） |
| M2 | 通过 | 前端 `ui/src/test-support/testBridge.ts`（exec 六动作 navigate/click/input/text/tree/wait + `[testid=x]`→`[data-testid]` 翻译纯函数 + 元素短轮询 ~1s + 原生 value setter 的 v-model 兼容 + MutationObserver/100ms 轮询 wait + 双重门控 VITE_TEST_API/真实 Tauri 探测，`main.ts` 动态 import 接入）+ 40 个 vitest；Rust `src-tauri/src/test_api/bridge.rs`（BridgeRegistry ready/oneshot 配对/单调 id/超时计算 max(15s,timeout+5s)/eval JS 构造/错误码映射 + 12 单测）、`mod.rs` 挂 6 个 `/api/ui/*` 端点（参数校验抽纯函数 + 5 单测）与 health/version bridgeReady 实化、`commands.rs` `test_bridge_hello`/`test_bridge_result` 无条件注册函数体 feature 门控；testid 铺设（App 导航 4 链接 + 4 页 + DeviceCard/PairingDialog/PushWizard/TransferItem 关键控件，动态前缀 `transfer-item-{id}` 等）+ `docs/contracts/testid-naming.md`；`ui/package.json` dev:test/build:test + `ui/.env.test-api` + `ui/src/vite-env.d.ts`；`tests/e2e/smoke-m2.mjs` 冒烟。验收：cargo test 65 绿 / --features test-api 106 绿（+17）/ npm test 132 绿（+40）/ build:test dist 含 `__testBridge` 且普通 build 不含 / smoke-m2 退出 0（bridgeReady 握手 → navigate /settings → wait 共享区 → tree 20 元素全带 testId → click nav 链接跳转生效 → 负路径 401） | **关键踩坑**：eval 载荷必须是"请求序列化为 JSON 文本后再整体作为一个 JS 字符串字面量"（二次序列化）——若直接嵌 JSON 对象字面量，exec 收到的是已求值对象，JSON.parse 失败被静默吞掉、端点表现为 15s 超时；前端 parseBridgeRequest 同步容错对象形态。可见性剪枝以 getClientRects 兜底 position:fixed（底部导航），单一 offsetParent 信号会误杀；wait 锚点选"共享区"（设置页特有文案）而非"设置"（底部导航标签恒在，会假命中）；click 派发不带 `view`（jsdom MouseEvent 对 view 成员做 Window 校验会拒）；testBridge 结果回传用裸 invoke（api.ts 的 invokeCommand 有 15s 超时竞速与 console.warn 噪音，桥回包不宜走它）；评估过 tauri mock runtime 方案测 handler，因 AppHandle<Wry> 类型不匹配放弃，handler 层由冒烟覆盖、校验逻辑抽纯函数直测 |
| M3 | 通过 | Rust `src-tauri/src/test_api/snapshot.rs`（显式 TestSnapshot：schemaVersion/selfDevice/devices/sessions/transfers/cards/discoveryStats，serde camelCase，ID 统一 16 位 hex 字符串口径；纯映射函数 map_devices（复用 core device_merge，与 UI 设备页同源同序）/map_sessions（connected_fps×信任表）/map_cards（活动 transfers 视图 + 全量 cards 视图含 removed/terminal）；build_snapshot 持锁只克隆/收集序列化在锁外；点分路径求值器 resolve_path（对象键/数组下标/`a.length` 合成值，链式下钻正确）；apply_op 全 op 矩阵（路径不存在全 false、eq/ne 标量+null≡null、gte/lte 仅 number、contains 仅 string、empty 空串/空数组/null）；wait_loop 泛型轮询（首拍即查后判超时、Err 立即上抛）+ 22 单测）、`mod.rs` 挂 `GET /api/state/transfers`（经 app.state::<AppState>() 取管理态）+ `GET /api/state/app`（桥 state 动作）+ `POST /api/state/wait`（默认 10s/500ms，intervalMs 可调小供单测；超时 408 detail 带 lastValue/polls/条件回显，非法 op/source/多余 value → 400，超时包络抽 wait_timeout_response 直测）；`bridge.rs` 抽 `bridge_eval`（dispatch 与 state/wait 的 app 源共用，行为不变）；前端四 store 各加 `toTestSnapshot()`（JSON 往返剥响应式/函数/undefined，Set 转数组，lastRequest 不进快照）+ testBridge 第七动作 `state`（聚合 {schemaVersion,devices,transfers,settings,toast}，setupTestBridge 增可选 pinia 参数、runAction 增可选第 4 参，回退 active pinia）+ `main.ts` 传 pinia + 8 个 vitest（四 store 序列化 + state 聚合/无 pinia INTERNAL/显式 pinia 优先）；契约文档 §5.3（快照 schema 字段表 + 暂缺登记 + 求值规则全集）；`tests/e2e/smoke-m3.mjs` 冒烟。验收：cargo test 65 绿 / --features test-api 128 绿（+22）/ npm test 140 绿（+8）/ vue-tsc 净 / smoke-m3 退出 0（state/transfers schemaVersion+数组字段+selfDevice hex → state/app 四 store 键 → wait gte 0 首拍即中 polls=1 → op=pfx 400 / source=bogus 400 → 永假 1500ms 得 408 detail.lastValue=1 polls=4） | **与 spec 基线差异**（OQ-1 定稿记录进契约 §5.3 暂缺表）：discoveryStats 恒 null（core DiscoveryHandle 无广播计数）；sessions[].state 无中间态可报（SessionManager Session 私有，在表=在连即状态）；ids 用 16 位 hex 字符串而非数字（对齐前端 TransferDto.job_id 序列化口径，避免 u64 JSON 精度）；self_device 裁剪为 id/name/shortCode/hidden（core DeviceInfo 是对端发现结构，本机身份字段在 identity+config）。AppState 单测不可构造（SessionManager/发现线程/信号量组装过重），build_snapshot 组装层由冒烟覆盖、映射逻辑抽纯函数直测；tokio 无 test-util feature，wait 轮询单测不用 start_paused 而用真实时钟短间隔（250ms/50ms，polls 用下限断言） |
| M4 | 通过 | Rust `src-tauri/Cargo.toml`（feature 扩为 `test-api = ["dep:axum","dep:xcap"]`，xcap 0.9 optional，其重导出 image 故 PNG 编码零新增依赖）；`src-tauri/src/test_api/shot.rs`（GET /api/screenshot：tauri 主窗口 outer_position/outer_size 物理矩形 → xcap Monitor 定位（current_monitor 原点全等优先、含窗口左上角兜底，纯函数 locate_monitor_index/clamp_region 直测）→ 窗口∩显示器矩形钳制 → Monitor::capture_region（GDI 桌面 BitBlt），restore=true 缺省先 unminimize+300ms、捕获前 set_focus+150ms 消遮挡，阻塞捕获+PNG 编码下放 spawn_blocking，返回 image/png 原始字节 + X-Width/X-Height/X-Scale 头，失败 500 包络）；`invoke.rs`（POST /api/invoke：ALLOWED 12 项=11 ReadOnly+clear_completed_transfers 1 项 Mutating，逐分支手写调用命令函数（State 经 app.state() 传递、has_parts 的 job_id 从 args 反序列化），未列入 403 INVOKE_NOT_ALLOWED + detail.allowed 全集含 Class，Mutating warn 高亮带 run/step 上下文，执行错误 500 detail 带 cmd+error）；`mod.rs` 挂 test/begin（rand 16B hex runId=32 字符，置 TestContext 并清 step，开始标记本身带 runId 入环形缓冲）/test/step（无活动 run 400）/test/end（runId 匹配校验、结束标记先打再统计再清空，返回 count_run 条数）+ Router fallback/method_not_allowed_fallback 统一 404 NOT_FOUND 包络带特征头（M0 遗留小修）；`ring.rs` 去掉 set_run_id/set_step/test_context 的 dead_code 标记、新增 count_run；`tests/e2e/smoke-m4.mjs`（体系首条 dogfooding 回归：17 步 PASS/FAIL 汇总表——health 特征头/version/begin/step/navigate/wait 共享区/click nav 链接/state transfers/state-wait 首拍即中/screenshot PNG 魔数+X-* 头+>10KB 落盘 reports/m4-<时间戳>/shot-1.png/logs-tail 按 runId 过滤出 begin/step 标记/invoke 正反例（get_settings 200、remove_transfer 403 allowed=12）/end 统计 entries=4/无活动 run 负路径/未知路径 404）+ `tests/e2e/reports/.gitignore`；契约文档补 §5.4/§5.5（白名单 12 项含 Class 与排除清单）/§5.6 + NOT_FOUND 错误码。验收：cargo test 65 绿 / --features test-api 147 绿（+19）/ npm test 140 绿 / release 产物扫描退出 0 / smoke-m4 17/17 通过产出 46KB PNG（1016x719@1.00x） | **关键实测偏差**：xcap 0.9.8 `Window::all()` 的 is_valid_window 显式排除当前进程窗口（WebRTC 语义，防 GetWindowText 死锁），应用进程内永远枚举不到自己（冒烟实证 12 候选无主窗口；Window::new 为 pub(crate) 亦无法按 hwnd 构造）→ 截图改走"显示器定位+区域裁剪"路径并写入契约 §5.4；代价是桌面 BitBlt 会拍进遮挡物，捕获前 set_focus 缓解、截图仅作证据不作断言。白名单宁缺勿滥：Mutating 仅 clear_completed_transfers（plan 明示的 clear_finished_cards 类，view 级数据保留）；probe_now 触发类/set_hidden 隐身类均未入列。invoke 执行层（需 AppHandle）由冒烟覆盖、白名单/入参/403/500 包络抽纯函数直测。xcap Monitor 持原生 HMONITOR 非 Send：定位收拢进同步函数即建即弃 + spawn_blocking 闭包内按下标重新枚举显示器，handler future 不跨越非 Send 值（Handler Send 约束的坑，debug_handler 需 macros feature 未开、靠同步边界根治）。axum 0.8.9 的 method_not_allowed_fallback 一并接同一 404 包络（否则方法错配仍是裸 405 无包络，与 fallback 目的相悖） |
| M5 | 通过（部署链路+双机互发现已验证；三级重置/fixtures/部署脚本化待补入 M6） | `tests/e2e/ssh/id_ed25519` 密钥认证（密码零持久化，仅引导时装公钥经 env 传入）+ `targets.local.yaml`（gitignored：pcA=127.0.0.1:39872 / pcB=192.168.0.222:39871 / android serial）+ `lib/sshSetup.mjs`（环境体检：Win11/磁盘704GB/WebView2✓/单网卡WLAN/管理员组/47600-47601空闲）+ `deploy/start-test.bat`（env+启动）与 `fw-once.bat` + 测试机部署目录 `C:\Users\huss_laptop\localtrans-test\`（schtasks `/IT` 交互式任务 LT-Test 启动 GUI；一次性提权任务 LT-FW 预埋 UDP 47600-47601 防火墙规则后即删）+ `smoke-m5.mjs`（6/6：双端 health/version→test/begin→state/wait 双向发现[pcA 见 HUSS+huss_phone，pcB 见 huss_pc]→双端截图→runId 日志→test/end）。验收：跨 LAN Token 认证 200、双端 bridgeReady=true | **关键发现：裸 `cargo build --release` 产物仍指向 devUrl（localhost:1420），测试机 webview 报 ERR_CONNECTION_REFUSED——必须加 `tauri/custom-protocol` feature 才嵌入 dist**（`cargo tauri build` 的隐含行为）；此前冒烟全走 vite devUrl 故未暴露，测试构建命令固化为 `npm --prefix ui run build:test && cargo build --release -p localtrans --features "test-api tauri/custom-protocol"`。ssh 会话属 session-0，GUI 应用必须经 schtasks 交互式任务启动（spec 部署节未预见，已实证）。测试机→开发机 ICMP 不通（开发机防火墙挡 ping）但 UDP 发现双向正常，判定非 AP 隔离。开发机侧实例须 Start-Process 脱离 shell 启动（bash 后台子进程随会话退出被回收）。快照 devices[].address 序列化为 undefined（字段映射问题，M6 前修）→ **M6 核查为误读：字段名是 `addr`（映射本身正确），已在 snapshot.rs 加序列化钉死单测 + 契约 §5.3.1 澄清** |
| M6 | 通过（PC↔PC 真传输场景实跑绿 + 部署脚本化 + journal/证据/报告闭环） | Rust：`test_api/invoke.rs` 白名单 12→15（Mutating 增 `connect`/`push_files`/`push_files_rel`——原生文件选择对话框无法 DOM 驱动，传输发起必须语义注入；参数助手 str_vec_arg/pairs_arg + 3 单测；配对确认 grant_consent/deny_consent/submit_pair_code 仍排除走 UI）；`test_api/snapshot.rs` addr 字段钉死单测；**产品 bug 修复**：`main.rs` 接收泵 InstantHit 分支秒传卡 pending 直收 Finished 被状态机丢弃、卡片永卡"等待中"——补 Started 激活（与 recheck_parent_terminal 同款）+ `transfer_state.rs` 回归锚单测。编排器 `tests/e2e/`：`lib/config.mjs`（targets.local.yaml 行级解析，smoke-m5 改引公共模块）/`lib/journal.mjs`（journal.jsonl 全量流水）/`lib/target.mjs`（Target 抽象：api 自动入账/ok 包络断言/waitState/pollUntil/ui 便捷/invoke/截图存盘/logs）/`lib/deploy.mjs`（CLI+编程双用：停 pcA→build:test→cargo release[custom-protocol]→ssh 杀+sftp 推 exe+schtasks LT-Test→Start-Process 起 pcA→waitReady 双端；ssh2 exec/sftp 封装）/`lib/evidence.mjs`（双端 logs+截图+state 快照收割）/`lib/report.mjs`（markdown 步骤矩阵+工件链接+环境）/`lib/reset.mjs`（L1 invoke 清卡、L2 双端重启实测绿、L3 抛"待 M8"）/`fixtures/gen.mjs`（确定性 fixtures + 运行期唯一变体 genRunVariants；*.bin 与 runs/ 不入库）/`scenarios/pc-pc-transfer.mjs`（begin→L1 重置→双向发现[addr 钉死断言]→配对[A invoke connect；B UI 点 btn-grant；B 屏读码 .code-display 兜底 get_pairing_pending；A UI 输码 btn-submit；历史信任命中自动跳过]→push_files 4 fixtures→B UI 点 .offer-modal .btn-success 接收→双端终态/字节/文件名覆盖断言→截图+证据+报告；失败自动收割+FAIL 报告+退出 1）。验收：cargo 66 绿 / --features test-api 151 绿 / ui 140 绿 / `node lib/deploy.mjs` 全流程 87.2s 双端 bridgeReady / 场景 7/7 PASS（4 文件 5.2MB Wi-Fi 秒级，连续多次重跑绿）/ smoke-m5 6/6 绿 | **实测踩坑四则**：① 接收端内容去重（收件箱索引按全文件 sha256+size 秒传）会吞掉重复推送——场景 fixture 必须运行期加 runId 戳，且 runId 需秒级+随机尾（分钟级时间戳同分钟重跑同内容又触发秒传）；② LT-Test 任务带"电池模式不启动"条件，测试机改用电池后 schtasks 永久 已排队 且不报错——已改任务电源条件（DisallowStartIfOnBatteries=false），排障时 schtasks"成功"≠进程已起，必须以 health 收尾；③ waitReady 初版拉 /api/version 不带 Token 恒 401 误判未就绪；④ pcB 接收卡 `peer` 为空（接收事件泵未传对端指纹，main.rs 注释明示的产品限制）——场景按 direction+文件名覆盖识别，产品侧补齐留 M8。另：push 确认弹窗（App.vue offer 模态）未铺 testid，用 CSS 直选（.offer-modal .btn-success），ui/ 属并行泳道未越界；开发机实例曾被并行泳道 taskkill（共享开发机协调面），部署脚本秒级拉起恢复 |
| M7 | 通过（Android 实机 9/9 绿：install→launch→LT-BANNER 横幅→dump testTag→tap 切页往返→互发现→截图/logcat 归档） | Rust：`crates/localtrans-ffi/src/logcat.rs`（自实现 tracing Layer 直绑 NDK `__android_log_write`，不引 android_logger/tracing-log——那套是 log facade 反向桥且多两个依赖；仅 android target 增 workspace 内 tracing-subscriber；tag=`LT::`+crate 缩写+首段子模块（localtrans_core::discovery::x→LT::core::discovery，64 字符截断）；level 映射 TRACE→VERBOSE…ERROR→ERROR；默认 LevelFilter::INFO；多行消息按行拆条）+ `lib.rs` 构造器 android 下 `logcat::init()`（Once+try_init 防二次）+ `start()` 末尾启动横幅 `LT-BANNER ready version=<ffi版本> device=<设备名> fingerprint=<64hex>`（tag LT::ffi::banner，waitForBanner 就绪信号）。Kotlin：testTag 铺设与桌面同表（AppNav 根 `Modifier.semantics{testTagsAsResourceId()}` + `nav-{devices,files,transfers,settings}-link`；设备页 `devices-hidden-toggle`/`devices-local-ip-chip`/`devices-manual-add-open-btn`/`device-card-{fingerprint}`/手动添加对话框 3 项；传输页 `transfers-clear-completed-btn`/`transfer-item-{jobId}`；设置页 `settings-device-name-input`/`settings-relay-enabled-toggle`/`settings-relay-server-input`/`settings-relay-psk-input`/`settings-save-btn`；文件页 `files-location-{local,remote}-tab`/`files-create-folder-fab`；配对对话框沿用桌面共表旧名 `btn-grant`/`btn-deny`/`code-input`/`btn-submit`）；配对自动同意钩子（`debug/DebugTestHooks.kt` SharedPreferences debug_test_hooks 独立于 Rust config 防跨层契约改动 + EventRouter.route ConsentRequested 分支 BuildConfig.DEBUG 门控：开关开→respondConsent(true)+LT::kotlin::hooks 日志+return 不下发 UI + SettingsScreen debug 区块开关 `settings-test-auto-consent-toggle`，实机验证 False→True 翻转+日志）；`build.gradle.kts` NDK 路径配置化（local.properties ndk.dir → sdk.dir/ndk 最高版本目录 → 回退原硬编码）+ buildFeatures.buildConfig=true（AGP8 默认关）。编排器：`tests/e2e/lib/adb.mjs`（零依赖：uiautomator dump→元素数组（resource-id/text/contentDesc/clickable/selected/包名/bounds 中心，每次交互新鲜 dump）；tap 定位+中心坐标+短重试+失败附 dump 摘要；inputText ASCII-only（空格 %s、元字符转义）非 ASCII 明确抛 ADBKeyboard 错误；screencap PNG 头校验落盘；logcat -d -v time + tagPrefix 前缀过滤 + waitForBanner 轮询；install 带 MIUI 确认弹窗自动监视器（仅点系统安装器包名的继续安装/确定按钮）；adb 二进制解析 env LOCALTRANS_ADB→yaml android.adb→local.properties sdk.dir→PATH）+ `tests/e2e/smoke-m7.mjs`。验收：cargo ndk 双 target（arm64-v8a/x86_64）release + gradle assembleDebug + testDebugUnitTest 全绿；实机 M2002J9E（f9f8b0e/Android 12）smoke 9/9 PASS（横幅实文 `I/LT::ffi::banner: LT-BANNER ready version=0.12.0 device=我的手机 fingerprint=4643…ad0d`；dump 命中 7 testTag；nav-transfers-link tap 后 selected=true 且页面文案切换往返；手机设备页 dump 出 huss_pc/HUSS 双桌面卡片=发现互通[pcA test-api 可达时启用]；截图+LT 日志归档）；cargo test -p localtrans-core 208+ 全绿 | MIUI 坑两则：① `INSTALL_FAILED_USER_RESTRICTED` 实为“USB安装”开启后每次安装的手机端确认弹窗（10s 超时自动取消，屏幕无痕）——install 内置弹窗监视器自动点“继续安装”解决；② input 注入对非 debuggable 应用（设置等）受“USB调试（安全设置）”门控（本机已开启但偶发 SecurityException 需重试），对本 app debug 构建注入稳定可用。中文输入未做（ADBKeyboard 未安装，inputText 非 ASCII 明确报错不静默，OQ-4 留待后续）。uniffi 接口零变更无需 regen（校验和不变）；jniLibs 两个 .so 由 cargo ndk 重新生成属正常产物。ffi host 测试 29 过/11 败经 stash 对照验证为存量环境问题（开发机 pcA 实例占用 UDP 47600/47601 的端口类用例，与本次改动无关）。logcat.rs 两个纯函数单测仅 android target 编译可见，当前无 android 门禁跑不到（低风险遗留） |
| 验收 | 通过 | `tests/e2e/scenarios/api-acceptance.mjs` 13/13（协议安全负路径/四页观测+截图/双快照+日志游标/UI改名持久化/L2重置+配对/500MB含UI暂停→停滞→继续/200MB取消→真终态→DOM确认框→删卡/Android唤醒+dump+tab切换/证据收割）；配套补齐：bridge input events:[blur/enter]、confirmAdapter（测试构建原生confirm→DOM）、adb wake反通知栏误拉+setStayOn+pkill清场、设备命名统一 huss_pc/huss_laptop/huss_phone | E1 长期navHit=0根因是我方字段名笔误（通道字段testTag误用resourceId）；追查中顺带修复真实问题：息屏dump空壳、无锁屏上滑误开通知栏、雷电旧adb与SDK adb互杀server。疑点清单新增：一次push发送端产生多卡（failed占位+done×2）；取消后引擎任务疑似仍占活跃槽——均不阻塞验收，按约定延后 |
| 文档化 | 通过 | `docs/e2e-harness.md`（能力总览/设备与机密红线/17端点速查/驱动库速查/构建部署命令/跑场景/11条陷阱清单/已知遗留/新场景指南）+ `AGENTS.md` 新增「E2E 测试与远程调试」节（要点速览+指向手册）与「禁区」机密条款（targets.local.yaml/ssh//test-api.key/.local-token/reports/ 不提交；SSH 密码仅 LT_SSH_PW 环境变量一次性引导）；CLAUDE.md 本就是 @AGENTS.md 引用无需改 | 面向后续 agent：改码→deploy.mjs→跑场景→截图目检 的完整流程已可循文档独立执行 |
