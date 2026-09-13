# 贡献指南（从源码构建）

感谢关注 LocalTrans！本文帮助你在 10 分钟内把项目跑起来，并说明各项构建与测试约定。

## 环境要求

| 目标 | 必须 | 说明 |
|---|---|---|
| 桌面端开发/构建（Windows） | Rust 稳定版（rustup）、Node.js 18+、[tauri-cli](https://v2.tauri.app/start/prerequisites/) v2 | 主开发平台 |
| 前端 | Node.js 18+ | Vue 3 + Vite + Vitest |
| 安卓壳（可选） | JDK 17、Android SDK + NDK r27、cargo-ndk | 见下文"安卓" |
| Linux 中继（可选） | WSL Ubuntu-22.04 或任意 Linux + Rust | 见下文"中继" |

```bash
# 一次性安装 tauri 命令行
cargo install tauri-cli --locked
```

## 开发模式（桌面端）

```bash
git clone <本仓库>
cd localtrans/ui
npm install
npm run tauri:dev        # 启动桌面壳 + 前端热更新（Vite @ :1420）
```

首次运行如需局域网发现，在应用"设置"页点击"添加防火墙规则"（UAC 提权），
或手动放行 UDP 47600-47601。

## 测试门禁

仓库按"泳道"组织，改哪条泳道跑哪条门禁（合并前跑 `all`）：

```bash
bash scripts/gate.sh core    # cargo test -p localtrans-core
bash scripts/gate.sh shell   # cargo test -p localtrans（Tauri 壳）
bash scripts/gate.sh relay   # cargo test -p localtrans-relay -- --test-threads=1（必须串行：端口竞争）
bash scripts/gate.sh ui      # cd ui && npm run build（vue-tsc + vite）
bash scripts/gate.sh all
```

前端单测：`cd ui && npm test`（Vitest）。真机 E2E 工具链见
[docs/e2e-harness.md](docs/e2e-harness.md)（需要按文档配置测试机）。

## 发布构建

```bash
# 桌面便携版（注意：必须带 tauri/custom-protocol，否则界面显示"localhost 拒绝连接"）
cd ui && npm run build && cd ..
cargo build --release -p localtrans --features tauri/custom-protocol
# 产物: target/release/localtrans.exe
```

### 安卓（可选）

```bash
# 1) 编译 Rust so（Gradle 的 Rust 自动构建是禁用的，必须手动执行）
cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release

# 2) 配置本机路径 android/local.properties（已被 gitignore，不入库）
#    sdk.dir=<Android SDK>
#    ndk.dir=<NDK r27>
#    cargo.bin=<Git Bash 形式的 cargo bin 目录，如 /c/<user>/.cargo/bin>

# 3) 构建 APK
cd android && ./gradlew :app:assembleRelease
```

签名：把 `storeFile/storePassword/keyAlias/keyPassword` 写入
`android/keystore.properties`（已 gitignore）；缺省时 release 构建不签名。

### Linux 中继（可选）

```bash
bash scripts/build-relay-linux.sh   # WSL Ubuntu-22.04 或移植到任意 Linux
```

服务端部署（systemd/TOML 配置）见 `crates/localtrans-relay/deploy/`。

## 工程约定

- **泳道**：`core / relay / shell / ui / infra`。跨协议/DTO 的改动先单独
  提交接口定义，再并行实现。
- **提交信息**：`feat(core): ...` / `fix(壳): ...` / `docs: ...` 等现有格式，
  用中文描述做了什么、为什么。
- **测试纪律**：纯函数下沉 `localtrans-core`（可单测），编排留在壳层；
  新功能先有失败测试再实现（TDD）。
- **发布纪律**：版本号四处同步、产物统一布局、出包前跑敏感信息扫描，
  完整清单见 [docs/build-and-test.md](docs/build-and-test.md) §2。

## 提交 PR / MR

1. 从 `main` 拉分支，改动范围与描述一致；
2. 对应泳道门禁全绿；
3. 描述写清动机、改动点、验证方式（截图/日志更佳）。
