# AGENTS.md — AI Agent 开发公约

任何 AI agent（Claude Code / Codex / ZCode / Trae / Gemini CLI 等）在本仓库开工前，必须先读完本文件。人类协作者同样适用。

## 项目速览

LocalTrans：Rust + Tauri 2 + Vue 3 的局域网/中继文件传输工具（QUIC 传输、断点续传、配对权限管控）。当前 v0.12.0。

Rust workspace 四成员：

| 成员 | 职责 | 备注 |
|---|---|---|
| `crates/localtrans-core` | 协议/发现/配对/传输核心 | UDP 发现端口 47600，QUIC 数据端口 47601 |
| `crates/localtrans-relay` | 中继服务器 | 可独立部署；Linux 版在 WSL Ubuntu-22.04 原生构建 |
| `crates/localtrans-ffi` | Android JNI 壳 | `android/**/jniLibs/*.so` 是构建产物，**禁止手改** |
| `src-tauri`（包名 `localtrans`） | 桌面 Tauri 壳 | 前端在 `ui/`（Vue 3 + Vite） |

## 构建与测试

改动哪个泳道就跑哪条门禁（见 `scripts/gate.sh`），不要只跑自己"觉得有关"的：

```bash
bash scripts/gate.sh core    # cargo test -p localtrans-core
bash scripts/gate.sh shell   # cargo test -p localtrans（Tauri 壳）
bash scripts/gate.sh relay   # cargo test -p localtrans-relay -- --test-threads=1（必须串行：端口竞争）
bash scripts/gate.sh ui      # cd ui && npm run build
bash scripts/gate.sh all     # 以上全部，合并前由集成者执行
```

测试进程卡住/exe 被占用：`tasklist | grep localtrans` 查 PID → `taskkill //F //PID <pid>`。

## E2E 测试与远程调试

本仓库有一套已验收（13/13 PASS）的全自动 E2E 工具链，位于 `tests/e2e/`，**详细用法必读 `docs/e2e-harness.md`**。要点：

- **设备**：`huss_pc`（开发机本机）、`huss_laptop`（SSH 密钥 + schtasks 拉 GUI）、`huss_phone`（adb/USB，MIUI）。连接配置在 `tests/e2e/targets.local.yaml`。
- **改 Rust/前端后跑场景前**先 `cd tests/e2e && node lib/deploy.mjs` 一键重建双端（构建必须带 `--features "test-api tauri/custom-protocol"`，漏 custom-protocol 测试机白屏）。
- **跑场景**：`node scenarios/api-acceptance.mjs`（全能力验收）/ `scenarios/pc-pc-transfer.mjs`；报告与截图自动落 `tests/e2e/reports/<时间戳>/`。
- **验收含前端**：关键状态须截图目检渲染正确性，不允许只看后台断言。
- HTTP API 由 `test-api` feature 门控（`src-tauri/src/test_api/`），发布构建零开销；17 个端点与陷阱清单见手册。

## 并行开发约定

- **一任务一分支一 worktree**：`bash scripts/wt.sh new <任务名> <泳道>`。worktree 放 D 盘（C 盘空间紧张），分支名 `agent/<任务名>`。
- **泳道**：`core` / `relay` / `shell` / `ui` / `infra` / `read`（只读任务）。同泳道串行排队，跨泳道可并行。任务卡见 `docs/parallel/TASKS.md`。
- **跨层改动契约先行**：协议/DTO 变更会波及 core + 两个壳 + ui。这类改动必须先单独提交接口定义并合入 main，之后各方实现才能并行。禁止一个分支同时横跨两个以上泳道。
- **分支短命**：目标当天合并。合并到 main 由集成者（人 or 指定 orchestrator 会话）串行执行；agent 只负责把自己分支 rebase 到最新 main 并通过门禁。
- 提交信息沿用现有格式：`fix(壳): ...`、`feat(ui): ...`、`feat(core): ...`。

## 禁区与陷阱

- `Cargo.lock`：不要手工编辑。合并冲突时以 main 为准 `git checkout main -- Cargo.lock`，再 `cargo check` 重新生成。
- **端口**：测试代码禁止硬编码 47600/47601。使用 `LOCALTRANS_TEST_PORT_BASE` 环境变量做偏移（该支持由任务卡 P0-1 提供；落地前 relay E2E 保持 `--test-threads=1`，且本机同时只跑一个 relay 测试泳道）。
- `target/`、`target.android/`、`ui/node_modules/`、`dist/`、`tmp/`、`android/app/build/` 均为本地产物，不提交；不要清理其他 worktree 的产物。
- **E2E 机密不落 git**：`tests/e2e/targets.local.yaml`、`tests/e2e/ssh/`、`tests/e2e/deploy/test-api.key`、`tests/e2e/.local-token`、`tests/e2e/reports/` 一律不提交；SSH 密码只允许经 `LT_SSH_PW` 环境变量一次性引导，不写入任何文件。
- **打包与发版规约（固定，勿自创格式）**：产物唯一入口 `dist/localtrans-vX.Y.Z/` 合并文件夹——子目录固定 `PC-Windows/`（localtrans.exe + localtrans-relay.exe + usage.md）、`Android/`（apk + usage-android.md）、`Relay-Ubuntu/`（tar.gz + service + 占位符 toml 模板），根下必含 `README-先读我.txt` 与 `CHANGELOG.md`；外打 `dist/localtrans-vX.Y.Z-all.zip` 总包；发版完 `dist/` 只留当前版。版本号**四处同步**（根 `Cargo.toml` / `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` / `android/app/build.gradle.kts` 的 versionName+versionCode 递增）。发版前 `scripts/gate.sh all` 全绿；发布构建禁带 `test-api` feature；Android 必须手动 `cargo ndk` 重编 .so（gradle 的 Rust 自动构建是禁用的，光跑 gradle 出旧 so）；单端更新时版本号仍统一 bump、未变端沿用上一版二进制并在 README 注明，协议变更则三端齐编；出包前跑敏感信息扫描（真实 IP/PSK/密码/token/内网地址/个人路径一律不入包，示例用占位 IP）。完整流程与历史坑见 `docs/build-and-test.md` §2。
- agent 不执行 `git push`、不合并 main；远端 `hub`（D:/repos/localTrans-hub.git）是本地裸仓库，供集成与备份。
- Windows + Git Bash 环境；涉及 Linux 中继产物时用 WSL Ubuntu-22.04。

## 任务的"完成"定义

1. 改动只涉及任务卡声明的文件范围（越界需在任务卡备注说明原因）；
2. 对应泳道 `gate.sh` 全绿；
3. 提交信息符合格式，任务卡状态更新为"待评审"。
