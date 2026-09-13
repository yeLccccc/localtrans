# Task 22 审计报告:v0.11.0 三端打包与隐私核查

日期:2026-08-27。基线 commit:bfd2407 之后(HEAD=392ab76 版本对齐)。

## 一、PC release 编译

- `cargo build --release`(workspace,Windows MSVC):成功。
- 产物:
  - `target/release/localtrans.exe` — 21,271,040 B(Aug 27 05:06)
  - `target/release/localtrans-relay.exe` — 5,466,112 B(Aug 27 05:06)

## 二、relay 双平台

### Windows
同次 build 产物,见上。

### Ubuntu(WSL)
- 注:本机 WSL(Ubuntu 20.04,user huss)此前无 Linux Rust 工具链(rustup + build-essential 本轮新装)。构建目录 `/home/huss/build`(源码仅 core+relay,Cargo.toml 去除 src-tauri 与 localtrans-ffi 成员)。
- `cargo build --release -p localtrans-relay`:成功(29.9s)。
- 产物:`localtrans-relay` ELF x86-64 动态链接,7,044,072 B(BuildID 8975043e…)。

### relay tar.gz
`dist/localtrans-relay-v0.11.0-ubuntu-x86_64.tar.gz` — 2,616,631 B,内容:
- `./localtrans-relay`(新编译)
- `./localtrans-relay.toml`(沿用 v0.10.3 模板)
- `./localtrans-relay.service`
- `./relay-deploy.md`(**用 docs/relay-deploy.md 新版**,150 行 > 旧包 135 行,含第 10 节"日志保留策略")

## 三、Android

- so 时间戳核验:旧 so(Aug 27 03:58)**早于** bfd2407 提交时间(05:02),按指令重编。
- `cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release`(PowerShell + ANDROID_NDK_HOME=ndk/27.0.12077973):成功。
  - arm64-v8a/liblocaltrans_ffi.so — 10,094,496 B(Aug 27 05:10)
  - x86_64/liblocaltrans_ffi.so — 9,904,480 B(Aug 27 05:10)
- `gradle assembleRelease`(JDK17 / gradle 8.10.2):BUILD SUCCESSFUL。
- APK:`dist/localtrans-android-v0.11.0/localtrans-v0.11.0-android.apk` — 23,679,558 B。
- 验签(apksigner verify --print-certs):
  ```
  Signer #1 certificate DN: CN=LocalTrans, OU=Dev, O=LocalTrans, C=CN
  Signer #1 certificate SHA-256 digest: 2e8519e22bdae68274e898478cc55e33ec0a7a7f43134d1cfbc6293d4b28925d
  ```
- versionCode=15、versionName=0.11.0(output-metadata.json 与 build.gradle.kts 双确认)。

## 四、dist 组装 + 隐私核查

### 包清单
| 包 | 大小 | 内容 |
|---|---|---|
| dist/localtrans-v0.11.0-win-x64.zip | 11,603,657 B | localtrans.exe + localtrans-relay.exe + relay ubuntu tar.gz + usage.md |
| dist/localtrans-android-v0.11.0.zip | 14,541,404 B | localtrans-v0.11.0-android.apk + usage-android.md |
| dist/localtrans-relay-v0.11.0-ubuntu-x86_64.tar.gz | 2,616,631 B | 同上第二节 |

文档更新:
- 两份 usage 文档更新为 v0.11.0 重点(审计 76 项修复摘要)+ **硬切换三端同步升级提醒**(顶部引用块)。
- PC usage.md 并入 docs/received-files-location.md 存储位置节(默认 inbox/%APPDATA%、parts 临时区、安卓 Download/LocalTrans 公共目录与回落私有目录说明)。

### 隐私核查输出(PowerShell ZipFile OpenRead 遍历 entries)
```
== .../dist/localtrans-v0.11.0-win-x64.zip entries=4
PASS: no data/ / identity.key / cert.der entries
  localtrans-relay-v0.11.0-ubuntu-x86_64.tar.gz
  localtrans-relay.exe
  localtrans.exe
  usage.md
== .../dist/localtrans-android-v0.11.0.zip entries=2
PASS: no data/ / identity.key / cert.der entries
  localtrans-v0.11.0-android.apk
  usage-android.md
```

## 备注 / concerns
- WSL 无既有 Rust 工具链,本轮安装了 rustup(stable minimal)+ build-essential(user huss);构建目录 /home/huss/build。
- relay tar 打包用 Windows tar 遇 "Cannot connect to C:" 盘符解析问题,改经 $HOME 相对路径生成后拷入 dist。
- cargo ndk 后台命令 exit code 1 系链上 grep 版本号命令的退出码误报,构建本体 Finished 成功且 so 已更新。
- gradle assembleRelease 复用 up-to-date 缓存(23 executed),APK 为新 so 重新打包(packageRelease 执行于 05:12)。
