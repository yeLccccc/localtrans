# E2E 测试与远程调试手册

面向 AI agent 与人类协作者：如何用本仓库的 E2E 工具链**全自动**完成 LocalTrans 的部署、测试、取证。
设计 spec 见 `docs/specs/0001-e2e-test-automation.md`，执行记录见 `docs/plans/0001-e2e-test-automation.md`。
全量验收状态：**api-acceptance 13/13 PASS（2026-09-06）**。

## 1. 能力总览

通过这套工具，agent 可以远程做到（无需任何人手动操作被测机器）：

| 类别 | 能力 |
|---|---|
| 部署 | 一键构建测试构建 → 推送 huss_laptop → 双端拉起（`lib/deploy.mjs`） |
| 观测 | 双 PC 状态快照、tracing 日志（seq 游标增量拉取）、全窗口截图、UI 树/文本 |
| UI 操作 | 导航、点击、输入（含 blur/enter 事件派发）、等待选择器出现 |
| 传输控制 | 白名单 invoke（connect / push_files / clear_completed_transfers 等）+ UI 级暂停/继续/取消/删卡 |
| Android | 唤醒/亮屏保持、安装（MIUI 自动确认）、启动/强停、uiautomator 语义树（testTag）、点按、输入、截图、logcat（LT-BANNER 就绪信号） |
| 重置 | L1 软清（终态卡 view 级清理）、L2 进程重启（L3 出厂重置待 M8） |
| 取证 | 每步 journal.jsonl 流水、结束自动收割双端日志/状态/截图，生成 markdown 报告 |

## 2. 设备与连接

| 名字 | 角色 | 通道 | 说明 |
|---|---|---|---|
| `huss_pc` | 开发机（本机） | HTTP `127.0.0.1` | 跑测试编排的机器，同时也是被测端 |
| `huss_laptop` | 测试 PC | HTTP + SSH（密钥） | Windows，`schtasks /IT` 交互式任务拉起 GUI（session-0 限制） |
| `huss_phone` | Android 测试机 | adb（USB，serial 见配置） | MIUI；配对弹窗由 DebugTestHooks 自动同意 |

连接信息在 `tests/e2e/targets.local.yaml`（**已 gitignore，禁止提交**）：

```yaml
huss_pc:    { api: "http://127.0.0.1:39872", token: "<test-api token>" }
huss_laptop: { api: "http://192.168.0.222:39871", token: "<token>",
               ssh: { user: huss_laptop, host: 192.168.0.222, key: ssh/id_ed25519 } }
huss_phone:  { adb: { serial: "<serial>", sdk: "<platform-tools 路径>" } }
```

### 安全红线

- 不提交：`targets.local.yaml`、`tests/e2e/ssh/`、`tests/e2e/deploy/test-api.key`、`tests/e2e/.local-token`。
- SSH 密码只允许经环境变量 `LT_SSH_PW` 一次性引导装公钥（`lib/sshSetup.mjs`），之后一律密钥认证；密码不落盘不进 git。
- 提交前自查：`git diff --cached | grep -cE "<密码>|<token片段>"` 必须为 0。

## 3. HTTP API（test-api）速查

服务端：`src-tauri/src/test_api/`，由 cargo feature `test-api` 门控（发布构建零开销）。
认证：`Authorization: Bearer <token>` + `X-LocalTrans-TestAPI: 1` 双校验，token 常数时间比较。

| 端点 | 用途 |
|---|---|
| `GET /api/health` `/api/version` | 存活 / 版本（含 bridgeReady：前端测试桥是否就绪） |
| `GET /api/logs/tail` | tracing ring buffer 增量日志（seq 游标） |
| `GET /api/ui/tree` `/api/ui/text` | DOM 树 / 文本提取 |
| `POST /api/ui/navigate` `click` `input` | UI 三板斧；input 支持 `clear` 与 `events:["blur","enter",...]` |
| `POST /api/ui/wait` | 等待选择器出现/消失 |
| `GET /api/state/transfers` `/api/state/app` | Rust 侧权威状态快照（不依赖 DOM） |
| `POST /api/state/wait` | 点路径条件轮询（超时即失败） |
| `GET /api/screenshot` | 全窗口 PNG |
| `POST /api/invoke` | 白名单命令（15 个：11 只读 + connect/push_files/push_files_rel/clear_completed_transfers） |
| `POST /api/test/begin` `step` `end` | 测试标记：journal 上标注步骤边界与结论 |

## 4. 驱动库速查（`tests/e2e/lib/`）

```js
import { loadTargets } from './lib/config.mjs';
import { Target } from './lib/target.mjs';
import androidChannel from './lib/adb.mjs';
import { deployAll } from './lib/deploy.mjs';
import { reset } from './lib/reset.mjs';

const t = new Target('huss_pc', loadTargets().huss_pc, journal);
await t.state();                       // 状态快照
await t.uiNavigate('/transfers');
await t.uiClick('[testid=transfer-pause-btn]');
await t.uiInput('[testid=settings-device-name]', '新名字', { events: ['blur'] });
await t.pollUntil((s) => s.transfers.find((x) => x.state === 'done'), { timeoutMs: 60_000 });
await t.screenshot('路径.png');
await t.logs({ sinceSeq: 0 });
await t.invoke('push_files', { target_id, files: [...] });

const ch = androidChannel();
ch.wake(); ch.setStayOn(true); await ch.launch();
const els = await ch.dump();           // 元素含 .testTag 字段（不是 resourceId！）
await ch.tap({ testTag: 'nav-transfers-link' });
await ch.waitForBanner(30_000);        // logcat 中 LT-BANNER（Rust 层就绪信号）
```

重置：`reset(1, targets)` 软清终态卡；`reset(2, targets)` 双端杀进程重启（runId 失效，需重新 beginTest）；L3 出厂重置待 M8。

## 5. 构建与部署

```bash
# 一键：杀本机实例 → ui build:test → cargo release(test-api+custom-protocol) → 推 laptop → 双端拉起
cd tests/e2e && node lib/deploy.mjs            # 可选 --skip-build --skip-huss_pc --skip-huss_laptop
```

手动构建等价命令（**custom-protocol 必带**，否则产物指向 devUrl，测试机白屏）：

```bash
npm --prefix ui run build:test && cargo build --release -p localtrans --features "test-api tauri/custom-protocol"
```

远端拉起走既有计划任务 `LT-Test`（`schtasks /run`），**勿重复 create**；防火墙规则一次性脚本 `deploy/fw-once.bat`（UDP 47600-47601）。

## 6. 跑场景

```bash
cd tests/e2e
node scenarios/api-acceptance.mjs   # 13 项全能力验收（含 500MB 暂停续传 / 200MB 取消 / Android）
node scenarios/pc-pc-transfer.mjs   # M6 真实配对+传输
node scenarios/connect-memory.mjs   # M3a FR5 连接记忆自动重连（双 PC；任一端不可达自动 SKIP 退出=exit 0，已注册 run-all）
node scenarios/card-exchange.mjs    # M3a FR3/FR4 名片粘贴添加→发现→配对（双 PC；任一端不可达自动 SKIP 退出=exit 0，已注册 run-all）
node scenarios/trust-broken.mjs     # M3a FR6 TrustBroken 移除信任即时断连降级（双 PC；任一端不可达自动 SKIP 退出=exit 0，已注册 run-all）
node scenarios/routing-probe.mjs    # M3b T4 探测选路通道记录端到端（list_channels 只读观测面；双 PC；任一端不可达自动 SKIP=exit 0，已注册 run-all）
node smoke-m5.mjs                   # 冒烟：部署+发现+推拉
```

产物：`tests/e2e/reports/<场景>-<时间戳>-<rand>/` —— `journal.jsonl`、`report.md`（含每步 PASS/FAIL）、
`v-*.png` 视觉证据。**验收标准包含前端**：关键状态截图后 agent 须逐张目检渲染正确性，不是只看后台断言。

## 7. 陷阱清单（实证结论，勿重蹈）

1. **构建**：漏 `tauri/custom-protocol` → 测试机 webview 白屏（ERR_CONNECTION_REFUSED）。
2. **输入**：程序化设 value 不触发 Vue 事件——保存类流程必须 `events:['blur']`；回车确认用 `events:['enter']`。
3. **确认弹窗**：生产用 OS 原生对话框（DOM 不可见）；测试构建经 `ui/src/test-support/confirmAdapter.ts` 走 DOM 版（`[testid=confirm-dialog]` / `confirm-cancel` / `confirm-ok`）。
4. **取消语义**：点取消后卡片先入 `cancelling` 仲裁态（非 active 也非终态）——必须轮询到真终态（done/failed/interrupted）再操作；终态卡收进折叠的"历史"区，删卡前先点 `[testid=transfers-history-fold-btn]`。
5. **卡歧义**：一次推送可能产生占位卡+实体卡，断言按 `bytesTotal`/`cardId` 匹配，不要按列表序。
6. **Android dump 字段名**：uiautomator 的 `resource-id` 在通道里映射为 **`.testTag`**（写 `.resourceId` 恒 undefined，navHit=0 假失败）。
7. **Android 亮屏**：息屏边缘态语义树空壳——dump 前 `wake()`；长步骤前 `setStayOn(true)`；唤醒瞬间 Compose 树残缺，等 1.5s 再 dump。
8. **MIUI**：安装弹窗需自动确认（install 已内置）；唤醒滑动可能误拉通知栏（wake 已按 dumpsys 状态分支处理）。
9. **Windows 远端 GUI**：ssh 属 session-0，直接起 GUI 不可见——必须 schtasks 交互式任务；本机编排用 `Start-Process` 脱离会话。
10. **日志编码**：远端 Windows 输出 GBK，须转 UTF-8 再断言中文。
11. **banner 非致死**：`waitForBanner` 偶发不中，Android UI 断言与 banner 相互独立，场景里做了重试兜底。

## 8. 已知遗留（勿重复排查，按用户指示推后）

- 单次推送在发送端可能产生多卡（失败占位卡 + done×2）——待查。
- 取消后的推送可能占住引擎槽位，下一次推送 pending——待查。
- ffi 存量 7-8 个端口绑定测试失败（已验证与 E2E 无关，等任务卡 P0-1 端口基线）。
- toast 层级盖在弹窗遮罩之上（外观小瑕疵）。
- offer 接收弹窗尚无 testid、peer 字段选择器受限（M8 补）。

## 9. 新增场景指南

复制 `scenarios/pc-pc-transfer.mjs` 骨架：`Journal` + `step()` 包装 + `loadTargets()` 起新 Target；
步骤内失败即整场 FAIL 并写入 report.md；fixture 生成参考 `fixtures/gen.mjs`（大文件跑完即删）。
改了 Rust/前端后先 `node lib/deploy.mjs` 重建双端再跑场景；Android 侧改动需重新 assemble+install（`ch.install()`）。
