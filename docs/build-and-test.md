# LocalTrans 构建与测试手册

覆盖:双平台发布包构建(Windows / WSL Ubuntu)、本地中继测试、服务器部署。

---

## 1. 构建环境

| 平台 | 工具 | 说明 |
|---|---|---|
| Windows | Rust(msvc)、Node.js | 客户端 + Windows 版中继 |
| WSL Ubuntu-22.04 | Rust + build-essential | **服务器用的 Linux 版中继**(原生构建,非交叉编译,产物最可靠) |

WSL 环境一次安装(已装过则跳过):

```bash
wsl -d Ubuntu-22.04
sudo apt-get update && sudo apt-get install -y build-essential pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
```

## 2. 版本发布流程

> 发版产物**唯一入口**:`dist/localtrans-vX.Y.Z/` 合并文件夹 + `localtrans-vX.Y.Z-all.zip` 总包。
> 支持**单端更新**(只改了 Android / 只改了 relay):版本号仍全仓统一 bump,只重编受影响端,未受影响端沿用上一版二进制并在 README 注明"本版变更:仅XX端";**协议/契约变更必须三端同步重编**。

### 2.1 版本号(手动,四处同步,缺一不可)

| 位置 | 字段 |
|---|---|
| `Cargo.toml`(workspace 根) | `version` |
| `src-tauri/Cargo.toml` | `version` |
| `src-tauri/tauri.conf.json` | `"version"` |
| `android/app/build.gradle.kts` | `versionName`(与上面同号)+ `versionCode`(每次发版 +1) |

```bash
grep -E '^version' Cargo.toml src-tauri/Cargo.toml && grep '"version"' src-tauri/tauri.conf.json && grep -E 'version(Code|Name) =' android/app/build.gradle.kts
```

版本规则:每次发版三段号末位 +1(如 0.13.0 → 0.13.1;大功能 0.14.0)。
> 历史教训:v0.13.0 发版时 `src-tauri/Cargo.toml` 漏同步(exe 内嵌旧版本号);安卓 versionName 不在 Rust 侧、最易漏——上面一条 grep 命令四处全查。

### 2.2 全量测试(打包前必须)

```bash
bash scripts/gate.sh all   # = core + shell + relay(必须串行,端口竞争) + ui 四泳道
```

> 测试进程卡住/exe 被占用时:`tasklist | grep localtrans` 查 PID → `taskkill //F //PID <pid>`。
> 发布客户端**绝不带 `test-api` feature**(测试后门不入发布物;仅 E2E 部署构建带,见 docs/e2e-harness.md)。

### 2.3 发版产物统一布局(固定,勿改)

```
dist/localtrans-vX.Y.Z/
├── README-先读我.txt        # 总索引:每文件用途+快速开始+升级注意(必含)
├── CHANGELOG.md             # 从仓库根拷入
├── PC-Windows/
│   ├── localtrans.exe
│   ├── localtrans-relay.exe # Windows 版中继,一般只用于本机测试
│   └── usage.md             # PC 使用说明(发版前更新版本相关内容)
├── Android/
│   ├── localtrans-vX.Y.Z-android.apk
│   └── usage-android.md
└── Relay-Ubuntu/
    ├── localtrans-relay-vX.Y.Z-ubuntu-x86_64.tar.gz  # 内含 relay-deploy.md
    ├── localtrans-relay.service
    └── localtrans-relay.toml                          # 占位符模板,严禁真实配置
```

打总包:`cd dist && powershell -Command "Compress-Archive -Path localtrans-vX.Y.Z -DestinationPath localtrans-vX.Y.Z-all.zip -Force"`。
旧版清理:发版完成后 `dist/` 只留当前版文件夹与总 zip,历史版本删除(需要时从 git tag 重建)。

### 2.4 构建 PC 包(PC-Windows/)

```bash
taskkill //F //IM localtrans.exe 2>/dev/null   # 运行中的实例锁 exe → 链接 os error 5(0.13.0 踩过)
cd ui && npm run build && cd ..
cargo build --release -p localtrans --features tauri/custom-protocol   # ★漏了这个 feature = webview 去
cargo build --release -p localtrans-relay                              #  连 devUrl(localhost:1420)
cp target/release/localtrans.exe target/release/localtrans-relay.exe dist/localtrans-vX.Y.Z/PC-Windows/
```

> **★ `tauri/custom-protocol` 是发布构建的命门**:不带它,exe 启动后 webview 加载
> devUrl →"localhost 拒绝连接 ERR_CONNECTION_REFUSED"白窗(0.13.0 发版踩过——
> 测试部署构建带 test-api 时顺带带了它,裸 cargo build 不带,见 AGENTS.md E2E 节)。
> 发布构建带 custom-protocol、**不带** test-api。
> 自检 exe 内嵌版本:`powershell -Command "(Get-Item '...localtrans.exe').VersionInfo.FileVersion"`。
> 注意 cargo 链接失败被管道吞掉的情况:`ls -la target/release/` 核对产物时间戳再继续。

**启动冒烟(必做,且必须目检 UI)**:拷 exe 到临时目录启动 → 进程存活 + 应用日志出现
`ui: logBridge attached`(前端加载成功的标志)→ **截图目检窗口渲染的是真实页面**,
不是 localhost 错误页/白窗 → taskkill 收尾。只在临时目录跑,别在发布目录里跑(会产生
data/ 运行时垃圾混进发布包)。

### 2.5 构建 Android 包(Android/)

**gradle 的 Rust 自动构建是禁用状态**(`build.gradle.kts` 中 preBuild dependsOn 被注释,"manual integration")——必须手动重编 .so,光跑 gradle 会拿旧 .so 出包(0.13.0 发版踩过,.so 落后 HEAD 两个功能提交):

```bash
export ANDROID_NDK_HOME="$(grep ndk.dir android/local.properties | cut -d= -f2)"
cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release
export JAVA_HOME="<JDK17>"
<gradle>/bin/gradle.bat -p android :app:genUniffi :app:assembleRelease
# 产物 android/app/build/outputs/apk/release/app-release.apk → 拷入 Android/ 并改名 localtrans-vX.Y.Z-android.apk
```

- `genUniffi` 重生成绑定后 `git status` 应**零 diff**;有 diff = ffi 接口变了而绑定没跟上,必须先提交绑定再发版
- 核对 APK 版本:`android/app/build/outputs/apk/release/output-metadata.json` 的 versionName/versionCode
- jniLibs/*.so 与 build.gradle.kts 版本号变更随发版提交(.so 是构建产物,由 cargo ndk 生成属正常,非手改)

### 2.6 构建 Linux 中继包(Relay-Ubuntu/)

```bash
bash scripts/build-relay-linux.sh   # WSL Ubuntu-22.04 原生构建(唯一路径) → target-relay-linux/localtrans-relay
wsl -d Ubuntu-22.04 -- bash -c 'strip /mnt/c/Users/<user>/Desktop/work/localTrans/target-relay-linux/localtrans-relay'
# tar.gz 四件套:strip 后二进制 + crates/localtrans-relay/deploy/{localtrans-relay.service,localtrans-relay.toml} + docs/relay-deploy.md
# → dist/localtrans-vX.Y.Z/Relay-Ubuntu/localtrans-relay-vX.Y.Z-ubuntu-x86_64.tar.gz
```

> tar 的目标路径用 POSIX 形式(`/c/...`),`C:\...` 会被 tar 当远程主机名报 "Cannot connect to C:"。
> config 必须放 deploy/ 目录的**占位符模板**,严禁把服务器真实配置(真实 PSK/公网 IP)打进包。

**WSL 里跑 Linux 测试**(强烈建议发版前跑一遍,Linux 平台的真实回归):

```bash
wsl -d Ubuntu-22.04 -- bash -lc 'source ~/.cargo/env && cd /mnt/c/Users/<user>/Desktop/work/localTrans && CARGO_TARGET_DIR=~/relaytarget cargo test -p localtrans-relay -- --test-threads=1'
```

> 注:跨平台编译坑——`crates/localtrans-core` 的 Windows/Linux cfg 分支只在各自平台编译。
> 已修过一例(`engine.rs` 的 `io::Write` 导入);若 Linux 构建报错,优先查 `#[cfg(target_os)]` 分支的 trait 导入。

### 2.7 出包前敏感信息检查(必做,对外发布最后一道门)

```bash
grep -rn "<真实IP>\|<PSK>\|<密码>\|192\.168\.\|relay.example.com\|Users..asus\|test-api.key" dist/localtrans-vX.Y.Z/
grep -ac "X-LocalTrans-Test" dist/localtrans-vX.Y.Z/PC-Windows/localtrans.exe   # 应为 0
```

清单:服务器真实 IP/域名、中继 PSK、SSH 凭据、E2E token、内网 IP、个人路径(用户名)、keystore——一律不得出现在包内文本或二进制;文档示例地址用 `1.2.3.4:9443` 类占位。扫描后**重打 zip 并从 zip 内抽样解包复核**。

### 2.8 提交 + 打 tag

```bash
git add -A
git commit -m "chore: vX.Y.Z 版本收尾"
git tag vX.Y.Z        # tag 已存在则先 git tag -d vX.Y.Z
```

### 2.5 提交 + 打 tag

```bash
git add -A
git commit -m "chore: vX.Y.Z 版本收尾"
git tag vX.Y.Z        # tag 已存在则先 git tag -d vX.Y.Z
```

---

## 3. 本地中继测试(Windows 单机)

在不部署服务器的情况下验证中继链路。**限制**:客户端带单实例插件,同一台机器只能开一个 GUI 实例,所以单机只能验证"客户端→中继"控制面(注册/心跳/重连);**两台设备经中继互传**的完整链路由 E2E 测试覆盖(见 §3.4),或用两台真实机器。

### 3.1 起本地中继

```bash
mkdir -p .superpowers/relay-test
cat > .superpowers/relay-test/relay.toml <<'EOF'
control_port = 19443
data_port_start = 19000
data_port_end = 19010
public_ip = "127.0.0.1"
psk = "test-psk-local"
lease_ttl_secs = 45
auth_max_per_min = 5
EOF
./dist/localtrans-vX.Y.Z/localtrans-relay.exe .superpowers/relay-test/relay.toml
```

看到 `中继启动: 控制面 :19443 (udp)` 即就绪。

### 3.2 预写客户端配置再启动

客户端的数据目录 = exe 同级 `data/`,预写 `data/config.json` 让它启动即连中继:

```bash
mkdir -p .superpowers/relay-test/instA/data
cat > .superpowers/relay-test/instA/data/config.json <<'EOF'
{
  "device_name": "测试机A",
  "download_dir": "downloads",
  "hidden": true,
  "quic_port": 47601,
  "discovery_port": 47600,
  "shares": [],
  "relay_enabled": true,
  "relay_server": "127.0.0.1:19443",
  "relay_psk": "test-psk-local"
}
EOF
cp dist/localtrans-vX.Y.Z/localtrans.exe .superpowers/relay-test/instA/
.superpowers/relay-test/instA/localtrans.exe
```

要点:
- `hidden: true`(设备页"隐身"开关的持久化形态)停发局域网发现广播——测试中继时不受局域网设备干扰
- `relay_*` 三项即设置页"中继"区块的持久化形态

### 3.3 验证

| 验证点 | 怎么看 |
|---|---|
| 控制面连接 | 客户端日志 `data/logs/localtrans.log.*` 出现 `中继连接成功` |
| UI 状态 | 设置页 → 中继卡片 → "已连接" |
| 重连能力 | 杀掉客户端进程重启,日志再次出现 `中继连接成功`(配置从 data/ 自动读取) |
| 中继侧 | 中继进程 stdout 的 tracing 日志 |

### 3.4 完整链路(两设备经中继互传)

单机 GUI 受单实例限制,**用 E2E 测试覆盖**(进程内双设备+真中继,真 UDP 回环):

```bash
cargo test -p localtrans-relay --test e2e -- --test-threads=1 --nocapture
```

三个测试分别验证:
1. `two_devices_connect_and_exchange_over_relay` — 双设备注册→punch→虚拟端点→内层 QUIC 握手→ControlMsg 交换
2. `client_leave_notifies_peer` — 一方优雅下线,对端名册秒级移除
3. `relay_restart_recovers` — 中继暴力重启,客户端自动重连+名册重建

真实双机测试(可选):两台机器分别装便携版,都开"隐身"+配置同一台中继,设备页出现"远程"徽标设备,浏览/下载/推送与局域网体验一致。

---

## 4. 服务器部署(Ubuntu)

详见 `docs/relay-deploy.md`(Ubuntu 包内也附了一份)。核心三步:

1. 解压 tar.gz → 二进制装 `/usr/local/bin/`、TOML 装 `/etc/`(**填公网 IP 和 PSK**)、unit 装 systemd
2. `systemctl enable --now localtrans-relay`
3. 阿里云安全组放行 **UDP 9443(控制面)+ 9000-9100(数据面)**

客户端:设置页 → 中继 → 开开关、填 `服务器IP:9443`、填同一 PSK → "已连接"后设备页出现远程设备。
