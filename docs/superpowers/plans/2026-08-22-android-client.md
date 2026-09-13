# LocalTrans Android 客户端 v0.6.0 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 LocalTrans 构建安卓客户端 v0.6.0——Kotlin+Compose UI,uniFFI 桥复用 localtrans-core,实现配对/互传/文件浏览器/相册备份,与桌面 v0.5.0 互通。

**Architecture:** 新增 `crates/localtrans-ffi`(uniFFI facade 包装 core)+ `android/` Kotlin 工程(Gradle Kotlin DSL)。core 唯一实质改动:share 协议加 Rename/Delete/Mkdir 三消息。UI 四页(设备/文件/传输/设置),事件流单向:core→ffi callback→EventRouter→ViewModel StateFlow→Compose。

**Tech Stack:** Rust(uniffi 0.28/cargo-ndk 4.1.2/NDK r27)、Kotlin 2.0.21、Jetpack Compose(BOM 2024.10.01)、AGP 8.7.3、compileSdk 35/minSdk 26、JUnit+turbine。

## Global Constraints

- 工具链路径(已装好,勿重装):JDK17=`C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8`;Gradle=`C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat`;SDK=`C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk`(含 NDK 27.0.12077973);adb=`C:/leidian/LDPlayer9/adb.exe`;Rust android targets(aarch64/armv7/x86_64)+cargo-ndk 4.1.2 已装
- 命令行环境是 Windows PowerShell(注意:Bash 工具实际是 Git Bash,路径用正斜杠,设置环境变量用 `export JAVA_HOME=...` 前缀方式)
- **安全硬约束**:配对码只在被连接方屏幕显示,`PairingCodeShown` 事件只发本机 UI,协议/日志永不含明文码;明文文件名/路径不进 tracing 日志(只打数量与 job_id)
- 兼容口径:手机 v0.6.0 ⇄ 桌面 v0.5.0 配对/推拉/确认/中继全部可用;远程文件操作(重命名/删除/新建)对老桌面返回「不支持」错误;老版本收到 ShareOp* 新消息按未知消息忽略不崩
- core 版本三段:本版 0.6.0(workspace Cargo.toml `[workspace.package]` version + src-tauri + tauri.conf.json 三处对齐——**本版桌面功能零变化,只升版本号**)
- 提交信息中文,格式 feat:/fix:/chore:/docs:/test:
- 权限三档语义与桌面一致:每次询问(默认)/自动接受/拒绝
- 推送确认超时 core `clamp(1,600)`、壳层/Android 侧 `clamp(15,600)`、默认 60s
- App 数据目录:`context.getFilesDir()/localtrans/`(配置/身份/传输记录),对应桌面 `data/`
- 备份目录名清洗:剔 `< > : " / \ | ? *` 与首尾空格点,清洗后为空回退指纹前 8 位
- YAGNI(不做):双向同步/内置编辑器/前台服务保活/另存顺延/Google Play/手动 IP/iOS/桌面端浏览器 UI

---

### Task 1: core — share 协议三个文件操作消息

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs`(ControlMsg 枚举加 3 变体)
- Modify: `crates/localtrans-core/src/share.rs`(执行层:rename/delete/mkdir)
- Test: `crates/localtrans-core/src/share.rs`(tests 模块内)

**Interfaces:**
- Consumes: 现有 `ShareRegistry::resolve(share_id, rel) -> Result<PathBuf, ShareError>`(share.rs:61)
- Produces(后续 Task 4 的 ffi 层与 rpc 路由依赖):
  - `ControlMsg::ShareRename { share_id: String, path: String, new_name: String }`
  - `ControlMsg::ShareDelete { share_id: String, path: String }`
  - `ControlMsg::ShareMkdir { share_id: String, path: String }`
  - `ControlMsg::ShareOpResult { ok: bool, error: Option<String> }`
  - `ShareRegistry::op_rename(&self, share_id: &str, rel: &str, new_name: &str) -> Result<(), ShareError>`
  - `ShareRegistry::op_delete(&self, share_id: &str, rel: &str) -> Result<(), ShareError>`
  - `ShareRegistry::op_mkdir(&self, share_id: &str, rel: &str) -> Result<(), ShareError>`

- [ ] **Step 1: 写失败测试(share.rs tests 模块)**

```rust
// share.rs tests 模块内追加
fn test_reg() -> (ShareRegistry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("share")).unwrap();
    std::fs::write(dir.path().join("share/a.txt"), "hello").unwrap();
    let reg = ShareRegistry::new(vec![crate::store::ShareDef {
        id: "s1".into(),
        alias: "默认共享".into(),
        path: dir.path().join("share"),
        recursive: true,
    }]);
    (reg, dir)
}

#[test]
fn op_rename_renames_file() {
    let (reg, dir) = test_reg();
    reg.op_rename("s1", "a.txt", "b.txt").unwrap();
    assert!(dir.path().join("share/b.txt").exists());
    assert!(!dir.path().join("share/a.txt").exists());
}

#[test]
fn op_rename_rejects_traversal_and_bad_name() {
    let (reg, _dir) = test_reg();
    // new_name 含路径分隔符 → 拒绝(防逃逸)
    assert!(reg.op_rename("s1", "a.txt", "../escape").is_err());
    assert!(reg.op_rename("s1", "a.txt", "sub/c.txt").is_err());
    // rel 逃逸 → 拒绝
    assert!(reg.op_rename("s1", "../a.txt", "b.txt").is_err());
}

#[test]
fn op_rename_missing_file_fails() {
    let (reg, _dir) = test_reg();
    assert!(reg.op_rename("s1", "nope.txt", "b.txt").is_err());
}

#[test]
fn op_delete_removes_file_and_dir() {
    let (reg, dir) = test_reg();
    std::fs::create_dir_all(dir.path().join("share/sub")).unwrap();
    std::fs::write(dir.path().join("share/sub/c.txt"), "x").unwrap();
    reg.op_delete("s1", "a.txt").unwrap();
    reg.op_delete("s1", "sub").unwrap();
    assert!(!dir.path().join("share/a.txt").exists());
    assert!(!dir.path().join("share/sub").exists());
}

#[test]
fn op_delete_missing_fails_but_traversal_rejected() {
    let (reg, _dir) = test_reg();
    assert!(reg.op_delete("s1", "nope.txt").is_err());
    assert!(reg.op_delete("s1", "../share").is_err());
}

#[test]
fn op_mkdir_creates_nested() {
    let (reg, dir) = test_reg();
    reg.op_mkdir("s1", "x/y").unwrap();
    assert!(dir.path().join("share/x/y").is_dir());
    // 已存在 → Err
    assert!(reg.op_mkdir("s1", "x/y").is_err());
}

#[test]
fn share_op_messages_serde_roundtrip() {
    use crate::protocol::ControlMsg;
    let msgs = vec![
        ControlMsg::ShareRename { share_id: "s1".into(), path: "a.txt".into(), new_name: "b.txt".into() },
        ControlMsg::ShareDelete { share_id: "s1".into(), path: "a.txt".into() },
        ControlMsg::ShareMkdir { share_id: "s1".into(), path: "x/y".into() },
        ControlMsg::ShareOpResult { ok: true, error: None },
    ];
    for m in msgs {
        let json = serde_json::to_string(&m).unwrap();
        let back: ControlMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}

#[test]
fn share_op_result_omits_error_when_none() {
    // 与 v0.5.0 reason 字段同款兼容手法:None 不序列化,老 JSON 能读
    let json = serde_json::to_string(&crate::protocol::ControlMsg::ShareOpResult { ok: true, error: None }).unwrap();
    assert!(!json.contains("error"));
    let back: crate::protocol::ControlMsg = serde_json::from_str("{\"share_op_result\":{\"ok\":true}}").unwrap();
    assert!(matches!(back, crate::protocol::ControlMsg::ShareOpResult { ok: true, error: None }));
}
```

注意:`ControlMsg` 的 serde tag 格式以现有代码为准——先读 protocol.rs 里 `#[serde(...)]` 属性(当前是 `#[serde(rename_all = "snake_case")]` 的 enum,带 `tag` 与否决定 JSON 形状)。若枚举用了 `#[serde(rename_all= "snake_case")]` 无 tag,则变体名序列化为 `{"ShareRename":{...}}` 还是 `["share_rename",{...}]` 要以现有测试 `offer_resp_reason_omitted_when_none` 的写法为准——**实现时先跑一个现有变体的 serde_json 序列化打印确认形状,再写断言**。上面最后一个测试的 JSON 字面量按实际形状调整。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib share::tests::op_ -- --nocapture 2>&1 | tail -5; cargo test -p localtrans-core --lib share::tests::share_op_ -- --nocapture 2>&1 | tail -5`
Expected: 编译错误(方法不存在/变体不存在)

- [ ] **Step 3: 实现**

protocol.rs ControlMsg 枚举追加(Goodbye 之前):

```rust
/// v0.6.0 远程文件操作请求(安卓端文件浏览器;老版本收到按未知消息忽略)
ShareRename {
    share_id: String,
    path: String,
    new_name: String,
},
ShareDelete {
    share_id: String,
    path: String,
},
ShareMkdir {
    share_id: String,
    path: String,
},
/// 文件操作结果(ok=false 时 error 给用户可读原因)
ShareOpResult {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
},
```

share.rs ShareRegistry impl 追加:

```rust
/// v0.6.0 远程文件操作——浏览方请求,数据方执行。
/// 安全边界:resolve 已挡 rel 逃逸;new_name 只允许纯文件名
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "." && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

pub fn op_rename(&self, share_id: &str, rel: &str, new_name: &str) -> Result<(), ShareError> {
    if !valid_name(new_name) {
        return Err(ShareError::Invalid("新名称只能是文件名".into()));
    }
    let old = self.resolve(share_id, rel)?;
    let parent = old.parent().ok_or_else(|| ShareError::Invalid("无父目录".into()))?;
    let target = parent.join(new_name);
    if target.exists() {
        return Err(ShareError::Invalid("目标名称已存在".into()));
    }
    std::fs::rename(&old, &target).map_err(|e| ShareError::Io(e))
}

pub fn op_delete(&self, share_id: &str, rel: &str) -> Result<(), ShareError> {
    let p = self.resolve(share_id, rel)?;
    if p.is_dir() {
        std::fs::remove_dir_all(&p)
    } else {
        std::fs::remove_file(&p)
    }.map_err(ShareError::Io)
}

pub fn op_mkdir(&self, share_id: &str, rel: &str) -> Result<(), ShareError> {
    let p = self.resolve(share_id, rel)?;
    if p.exists() {
        return Err(ShareError::Invalid("目录已存在".into()));
    }
    std::fs::create_dir_all(&p).map_err(ShareError::Io)
}
```

注意:以 share.rs 现有的 `ShareError` 变体实际名称为准(先读文件,若是 `ShareError::InvalidPath` 之类就用实际名;Io 变体若是 `From<io::Error>` derive 则用 `?`)。`ShareDef` 字段名以 store.rs 实际为准(测试里 `id/alias/path/recursive` 需核对)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-core --lib share:: 2>&1 | tail -3`
Expected: 全部 PASS(原有 share 测试不回归)

- [ ] **Step 5: 全量回归 + 提交**

```bash
cargo test -p localtrans-core 2>&1 | tail -3
git add crates/localtrans-core/src/protocol.rs crates/localtrans-core/src/share.rs
git commit -m "feat(core): share 协议新增 Rename/Delete/Mkdir 文件操作消息"
```

---

### Task 2: core — rpc 路由处理文件操作消息

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(spawn_rpc_router 的入站控制消息分发处)
- Test: `crates/localtrans-core/src/transfer/engine.rs`(tests 模块)

**Interfaces:**
- Consumes: Task 1 的 `ControlMsg::ShareRename/ShareDelete/ShareMkdir/ShareOpResult` + `ShareRegistry::op_rename/op_delete/op_mkdir`
- Produces: 数据方收到 ShareOp* 消息自动执行并回 `ShareOpResult`;浏览方(ffi 层)发消息后从 inbound_resp_rx 收 ShareOpResult。老对端收到未知 ShareOp* 变体时 serde 反序列化失败 → 该控制消息被丢弃/整连接控制流受影响的问题在本任务验证(若 ControlMsg 无 `#[serde(other)]` 兜底,验证老版本收到新变体的行为并记录结论到报告)

- [ ] **Step 1: 写失败测试(engine.rs tests 模块,模仿现有两实例测试的基建)**

先读 engine.rs 里现有的 `set_auto_offer_hook` 测试(约 3313 行附近)了解两实例测试如何搭建(bootstrap 双方 + 通道)。然后追加:

```rust
// engine.rs tests 模块内追加(基建函数名以现有测试为准,下面用 bootstrap_ab 代称)
#[tokio::test]
async fn share_op_rename_roundtrip_between_peers() {
    // 搭双方(参照现有两实例测试基建):a 是浏览方,b 是数据方
    // b 侧共享区放 a.txt
    // a 侧:take_inbound_resp_rx → 发 ControlMsg::ShareRename{s1, "a.txt", "b.txt"}
    // 断言:收到 ShareOpResult{ok:true};b 共享区出现 b.txt
}

#[tokio::test]
async fn share_op_delete_missing_returns_error_result() {
    // 同上,a 发 ShareDelete{"nope.txt"} → 断言收到 ShareOpResult{ok:false, error:Some(_)}
}

#[tokio::test]
async fn share_op_mkdir_creates_dir_on_peer() {
    // a 发 ShareMkdir{"x/y"} → ok:true + b 侧目录存在
}
```

**实现者注意**:上面三测试的搭建代码必须参照同文件现有测试(如 `auto_offer_hook_fires_with_job_and_peer` 或 `offer_ask_timeout_*`)的真实基建函数逐字模仿——两实例如何互相 bootstrap、共享 ShareRegistry 怎么注入、resp 通道怎么 take/return。不许凭空发明基建。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib transfer::engine::tests::share_op_ 2>&1 | tail -5`
Expected: FAIL(路由不认识新消息,收不到 ShareOpResult 或超时)

- [ ] **Step 3: 实现(spawn_rpc_router 分发处)**

在 engine.rs `spawn_rpc_router`(约 1348 行)入站消息 match 里加分支(位置与写法模仿现有 ListReq 分支):

```rust
ControlMsg::ShareRename { share_id, path, new_name } => {
    let result = reg.op_rename(&share_id, &path, &new_name)
        .map_err(|e| e.to_string());
    let resp = match result {
        Ok(()) => ControlMsg::ShareOpResult { ok: true, error: None },
        Err(e) => ControlMsg::ShareOpResult { ok: false, error: Some(e) },
    };
    let _ = sm.send_ctrl(&from, resp).await;
}
ControlMsg::ShareDelete { share_id, path } => { /* 同构,调 op_delete */ }
ControlMsg::ShareMkdir { share_id, path } => { /* 同构,调 op_mkdir */ }
```

变量名 `reg`/`from`/`sm` 以 spawn_rpc_router 实际签名为准(先读函数)。**日志不打印 path 明文**(安全约束),打 `tracing::debug!(share_id, "share op rename handled")` 即可。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-core --lib transfer::engine::tests::share_op_ 2>&1 | tail -3`
Expected: 3 PASS

- [ ] **Step 5: 老版本兼容验证 + 全量回归 + 提交**

验证:写一个一次性测试(或临时用 serde_json 手工构造)确认——老版本(v0.5.0 的 ControlMsg 枚举无 ShareRename)收到 `{"ShareRename":{...}}` JSON 时 `serde_json::from_str::<ControlMsg>` 报 unknown variant 错误。**把这个行为记录进实现报告**(结论影响 Task 4 的 ffi 层错误提示文案:如果老对端会导致反序列化错误而非优雅忽略,ffi 层的超时/错误文案要写「对端版本过旧,不支持文件操作」)。

```bash
cargo test -p localtrans-core 2>&1 | tail -3
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): rpc 路由处理 ShareRename/Delete/Mkdir 并回执结果"
```

---

### Task 3: localtrans-ffi crate 骨架 + uniFFI 构建链跑通(hello 级,最大风险前置)

**Files:**
- Create: `crates/localtrans-ffi/Cargo.toml`
- Create: `crates/localtrans-ffi/src/lib.rs`
- Create: `crates/localtrans-ffi/build.rs`
- Create: `crates/localtrans-ffi/README.md`(构建链说明)
- Modify: `Cargo.toml`(workspace members 加 "crates/localtrans-ffi")
- Test: `crates/localtrans-ffi/src/lib.rs`(tests 模块)

**Interfaces:**
- Consumes: `localtrans_core::identity::Identity::load_or_create(dir)`
- Produces(Task 5+ 的 Android 工程依赖):
  - Kotlin 侧入口 `uniffi.localtrans.LocalTrans`(`LocalTransApp` object 的伴生函数)
  - `fn new(data_dir: String, callback: LocalTransCallback) -> LocalTransApp`(constructor)
  - `fn hello(&self) -> String`(链路验证用,Task 9 后删)
  - `fn my_fingerprint(&self) -> String`
  - callback interface `LocalTransCallback { fun onEvent(event: AppEvent) }`
  - enum `AppEvent { HELLO(String) }`(先只这一个变体,Task 5 扩全)
  - 错误类型 `AppException`(uniffi::Error)

- [ ] **Step 1: 建 crate(先让 cargo test 红)**

`crates/localtrans-ffi/Cargo.toml`:

```toml
[package]
name = "localtrans-ffi"
edition.workspace = true
version.workspace = true

[lib]
crate-type = ["cdylib", "lib"]
name = "localtrans_ffi"

[dependencies]
localtrans-core = { path = "../localtrans-core" }
uniffi = { version = "0.28", features = ["cli"] }
tokio = { workspace = true }
thiserror = { workspace = true }

[build-dependencies]
uniffi-build = { version = "0.28" }

[dev-dependencies]
tempfile = "3"
```

workspace 根 Cargo.toml members 数组加 `"crates/localtrans-ffi"`。

`build.rs`:
```rust
fn main() {
    uniffi_build::generate_scaffolding("./udl/app.udl").unwrap();
}
```

**注意:uniFFI 0.28 有两种模式——proc-macro(推荐,无 UDL 文件)与 UDL。本计划用 proc-macro 模式**,build.rs 改为:

```rust
fn main() {
    // proc-macro 模式无需 scaffolding;留空仅为将来链 gsb
}
```

`src/lib.rs`:

```rust
use std::sync::Arc;

/// 事件回执(先只放链路验证变体,Task 5 扩全)
#[derive(uniffi::Enum)]
pub enum AppEvent {
    Hello { message: String },
}

#[uniffi::export(callback_interface)]
pub trait LocalTransCallback: Send + Sync {
    fn on_event(&self, event: AppEvent);
}

#[derive(uniffi::Error)]
pub enum AppException {
    Io { message: String },
    Internal { message: String },
}

impl From<std::io::Error> for AppException {
    fn from(e: std::io::Error) -> Self { AppException::Io { message: e.to_string() } }
}

#[derive(uniffi::Object)]
pub struct LocalTransApp {
    runtime: tokio::runtime::Runtime,
    identity: Arc<localtrans_core::identity::Identity>,
}

#[uniffi::export]
impl LocalTransApp {
    #[uniffi::constructor]
    pub fn new(data_dir: String, callback: Box<dyn LocalTransCallback>) -> Arc<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let dir = std::path::PathBuf::from(&data_dir);
        std::fs::create_dir_all(&dir).ok();
        let identity = Arc::new(
            localtrans_core::identity::Identity::load_or_create(&dir)
                .expect("identity load_or_create"),
        );
        // 链路验证事件
        callback.on_event(AppEvent::Hello { message: "ffi up".into() });
        Arc::new(Self { runtime, identity })
    }

    pub fn hello(&self) -> String { "localtrans-ffi".into() }

    pub fn my_fingerprint(&self) -> String {
        self.identity.fingerprint().0.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hello_and_fingerprint_work() {
        let dir = tempfile::tempdir().unwrap();
        struct Cb;
        impl crate::LocalTransCallback for Cb {
            fn on_event(&self, event: crate::AppEvent) {
                assert!(matches!(event, crate::AppEvent::Hello { .. }));
            }
        }
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().to_string(), Box::new(Cb));
        assert_eq!(app.hello(), "localtrans-ffi");
        assert!(app.my_fingerprint().len() >= 16);
    }
}
```

**实现者注意**:`Fingerprint` 类型的 Display/内部字段以 identity.rs 实际为准(`fingerprint()` 返回 `Fingerprint`,看它是 newtype 还是 struct,取其 hex 字符串;若已有 `short_code()`/`to_string()` 用现成的)。

- [ ] **Step 2: 跑测试**

Run: `cargo test -p localtrans-ffi 2>&1 | tail -5`
Expected: 若 uniffi 0.28 与本 workspace 依赖冲突(ring/rustls 版本),报告冲突并锁 `uniffi = "0.28.3"` 或降到 "0.27" 重试——**版本以能编译为准,记录最终版本到报告**
Expected: PASS

- [ ] **Step 3: 交叉编译验证(cargo-ndk 产 .so)**

```bash
export ANDROID_NDK_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973"
cargo ndk -t arm64-v8a -t x86_64 -o ./target/android cargo build -p localtrans-ffi --release 2>&1 | tail -5
ls target/android/arm64-v8a/liblocaltrans_ffi.so target/android/x86_64/liblocaltrans_ffi.so
```
Expected: 两个 .so 存在。若链接错误,常见原因:socket2/ring 需要 NDK 的 clang——cargo-ndk 自动处理;报 `linker not found` 则确认 ANDROID_NDK_HOME 路径。**这是本计划最高风险步骤**,卡住超过 20 分钟报 BLOCKED 并附完整错误输出。

- [ ] **Step 4: 生成 Kotlin 绑定验证**

```bash
cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/android/x86_64/liblocaltrans_ffi.so --language kotlin --out-dir ../android-uniffi-tmp 2>&1 | tail -3
ls ../android-uniffi-tmp
```

注意:proc-macro 模式下 uniffi-bindgen 从 .so 提取元数据。`--library` 路径相对当前目录(cwd=crates/localtrans-ffi 时 .so 在 ../../target/...)。bin uniffi-bindgen 需要 `[features] default` 里加 `"uniffi/cli"` 或单独 dev-dependency uniffi(以 0.28 文档为准:在 Cargo.toml 加 `[[bin]] name = "uniffi-bindgen" path = "uniffi-bindgen.rs"`,uniffi-bindgen.rs 内容 `fn main() { uniffi::uniffi_bindgen_main() }`)。
Expected: 生成 `uniffi/localtrans/localtrans.kt` 等文件。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock crates/localtrans-ffi
git commit -m "feat(ffi): localtrans-ffi crate 骨架,uniFFI 桥构建链跑通"
```

---

### Task 4: ffi — AppEvent 全量事件 + 设备/配对/设置 API

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`
- Create: `crates/localtrans-ffi/src/state.rs`(AppState 内部态:设备表/传输表/待配对表)
- Create: `crates/localtrans-ffi/src/dto.rs`(uniFFI Record 类型)
- Test: `crates/localtrans-ffi/src/lib.rs`(tests)

**Interfaces:**
- Consumes: core 的 `SessionManager::spawn(ctx) -> (Arc<Self>, mpsc::Receiver<SessionEvent>)`、`DiscoveryHandle`/`discovery::spawn(cfg, key)`、`SessionEvent` 七变体、`store::Config/load_config/save_config`、Task 1-2 无直接依赖
- Produces(Task 5-8 Kotlin 侧调用的最终 API 面):
  - AppEvent 全量:`DevicesChanged`、`ConsentRequested { fingerprint, name }`、`PairingCodeShown { fingerprint, code }`、`PairingWaitConsent { fingerprint, name }`、`PairingCodeEntry { fingerprint, name }`、`PairingResult { fingerprint, ok, reason }`、`SessionUp { fingerprint, name }`、`SessionDown { fingerprint }`、`TransferUpdated { transfer: TransferDto }`、`TransferDone { job_id, ok, fail_reason }`、`OfferRequested { job_id, peer_name, file_count, total_size, deadline_epoch_ms }`、`BackupProgress { done, total }`
  - Record:`DeviceDto { fingerprint, name, online, connected, via_relay }`、`TransferDto { job_id, direction, peer_name, file_name, state, progress_percent, speed_bps, eta_secs, fail_reason }`、`SettingsDto { device_name, hidden, offer_timeout_secs, consent_timeout_secs, relay_enabled, relay_addr, relay_psk, backup_enabled, backup_target_fp, backup_photos, backup_videos }`
  - 方法:`start(&self)`、`shutdown(&self)`、`devices(&self) -> Vec<DeviceDto>`、`set_hidden(bool)`、`connect_device(fp: String)`、`respond_consent(fp: String, accept: bool)`、`submit_pairing_code(fp: String, code: String)`、`cancel_wait(fp: String)`、`settings(&self) -> SettingsDto`、`save_settings(s: SettingsDto)`、`disconnect(fp: String)`

- [ ] **Step 1: 写失败测试**

```rust
// lib.rs tests 模块(用 core 的 test_support 起最小闭环)
#[test]
fn start_emits_devices_and_settings_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    struct Cb(std::sync::Mutex<Vec<crate::AppEvent>>);
    impl crate::LocalTransCallback for Cb {
        fn on_event(&self, e: crate::AppEvent) { self.0.lock().unwrap().push(e); }
    }
    let cb = std::sync::Arc::new(Cb(std::sync::Mutex::new(vec![])));
    let app = crate::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(CbClone(cb.clone())));
    // CbClone: 包一层 Arc 克隆回调(实现时写个简单 wrapper struct)
    app.start();
    // 保存设置→读回
    let mut s = app.settings();
    s.device_name = "我的手机".into();
    s.offer_timeout_secs = 90;
    app.save_settings(s);
    let s2 = app.settings();
    assert_eq!(s2.device_name, "我的手机");
    assert_eq!(s2.offer_timeout_secs, 90);
    assert!(!app.devices().is_empty() || true); // 空表也合法,只验不炸
    app.shutdown();
}
```

(回调克隆 wrapper 的实际写法实现者自定义,比如 `struct SharedCb(Arc<Cb>)` + impl LocalTransCallback for SharedCb 转发。)

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi 2>&1 | tail -3`
Expected: 编译错(start/settings 等方法不存在)

- [ ] **Step 3: 实现**

lib.rs 拆成三文件:
- `dto.rs`:`#[derive(uniffi::Record)]` 的 DeviceDto/TransferDto/SettingsDto(字段全 pub,uniffi::Record 不支持 Option<String> 时 fail_reason 用空串代替——uniFFI 0.28 Record 支持 Option,先试 Option,不行降级空串并记录)
- `state.rs`:内部 AppState(模仿桌面壳 src-tauri/src/main.rs:25 的 AppState 字段裁剪:dir/identity/config/trust/sm/discovery/reg/hidden/devices/transfers/pending_pairing/connected_fps)
- `lib.rs`:LocalTransApp 方法。事件桥:`start()` 里 `runtime.spawn` 三个循环任务——
  1. session 事件循环:`while let Some(ev) = session_rx.recv().await` 把 SessionEvent 七变体逐一映射到 AppEvent 回调(映射表见 Interfaces;`PairingCodeShown` 映射时**只回调本机**,天然满足安全约束)
  2. discovery 包循环(handle 的 receiver,以 discovery.rs `DiscoveryHandle` 实际字段为准):更新 devices 表 + `DevicesChanged`
  3. 心跳/SessionUp/Down 维护 connected_fps(参照桌面壳 main.rs 的做法)

  `save_settings` 里 `offer_timeout_secs/consent_timeout_secs` 走 `clamp(15,600)`(壳层钳制,core 已有 clamp(1,600) 兜底)。
  发现层启动参数(组播端口/协议)照抄桌面壳 main.rs 的 DiscoveryConfig 构造(找到它 copy,不发明)。

**实现者注意**:这是最大的实现任务,允许 700-900 行。桌面壳 src-tauri/src/main.rs 是唯一权威参照——每个字段/循环的初始化都去那边找对应物。禁止发明桌面壳没有的行为。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-ffi 2>&1 | tail -3 && cargo ndk -t arm64-v8a -t x86_64 -o ./target/android cargo build -p localtrans-ffi --release 2>&1 | tail -2`
Expected: 测试 PASS + 交叉编译成功

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-ffi
git commit -m "feat(ffi): AppEvent 全量事件桥 + 设备/配对/设置 API"
```

---

### Task 5: ffi — 传输 API(push/pull/offer 应答/断点续传)+ 备份入口

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`、`crates/localtrans-ffi/src/state.rs`
- Test: `crates/localtrans-ffi/src/lib.rs`(tests)

**Interfaces:**
- Consumes: core `transfer::engine::{push_files_rel, spawn_rpc_router}` 相关公共入口(以 engine.rs pub fn 为准)、`ProgressEvent` 11 变体、`set_inbound_recv_hook`/`set_auto_offer_hook`、`ControlMsg::OfferResp{reason}`;桌面壳 main.rs 的传输表更新/xfer_lock/占位任务模式
- Produces(Kotlin Task 6-8 调用):
  - `push_files(fp: String, paths: Vec<String>) -> u64`(返回占位 job_id;异步,结果走事件)
  - `pull_files(fp: String, remote_share_id: String, remote_paths: Vec<String>) -> u64`
  - `respond_offer(job_id: u64, accept: bool)`
  - `transfers(&self) -> Vec<TransferDto>`
  - `pause/resume/cancel(job_id: u64)`、`retry_transfer(job_id: u64) -> u64`
  - `list_remote(fp: String, share_id: String, path: String) -> Vec<FileEntryDto>`、`remote_shares(fp: String) -> Vec<ShareDto>`
  - `share_op(fp: String, share_id: String, op: FileOp, path: String, new_name: String) -> Result<String, AppException>`(FileOp enum: Rename/Delete/Mkdir;同步等待 ShareOpResult,超时 10s 报「对端版本过旧或离线」)
  - `list_local(dir: String) -> Vec<FileEntryDto>`(本地浏览,Rust 侧直接 std::fs)
  - `local_op(op: FileOp, path: String, new_name: String) -> Result<(), AppException>`
  - `backup_push(fp: String, paths: Vec<String>, batch_tag: String)`(= push_files_rel 包装,rel_dir=`LocalTransBackup/<清洗后设备名>/`,逐批事件 BackupProgress)
  - Record:`FileEntryDto { name, is_dir, size, modified_ms }`、`ShareDto { share_id, alias }`
  - `clean_backup_dir_name(name: String) -> String`(pub,Task 8 Kotlin 侧也要用同一规则;剔 `< > : " / \ | ? *` 与首尾空格点,空则指纹前 8 位由调用方处理)

- [ ] **Step 1: 写失败测试**

```rust
// 用 core test_support 的两实例基建(参照 Task 2 的搭建)写 ffi 层闭环:
#[tokio::test]
async fn push_and_offer_flow_through_ffi() {
    // app_a/app_b 各一个 LocalTransApp(不同 tempdir)
    // a.start() + b.start(),手工互信(直接往 trust 里 upsert 对方指纹——参照 core test_support 的做法)
    // a.connect_device(b_fp) → 走 QUIC 连接(桌面壳 connect 命令的调用序列照抄)
    // a.push_files(b_fp, vec![文件]) → b 收到 OfferRequested 事件(cb 记录)
    // b.respond_offer(job_id, true) → a 收到 TransferUpdated→TransferDone{ok:true}
    // b 侧落盘目录出现文件
}

#[tokio::test]
async fn share_op_through_ffi_rename_on_peer() {
    // 同基建:a share_op(FileOp::Rename, "a.txt", "b.txt") → Ok;b 共享区改名生效
}

#[test]
fn clean_backup_dir_name_rules() {
    assert_eq!(crate::clean_backup_dir_name("我的电脑".into()), "我的电脑");
    assert_eq!(crate::clean_backup_dir_name("a<b>c:d".into()), "ac d".replace(' ', "") /* 按实现定,断言写实现后核实 */);
    assert_eq!(crate::clean_backup_dir_name("  ..  ".into()), ""); // 空 → 调用方回退
}
```

第一、二个测试是 E2E 级,搭建繁重——**若 core test_support 没有可直接复用的两实例 helper,允许把这两个测试降级为「单实例 + 本地回环 connect」**(a 连 127.0.0.1 自己的 listener 端口),报告里注明。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi 2>&1 | tail -3`
Expected: 编译错(方法不存在)

- [ ] **Step 3: 实现**

照抄桌面壳的传输编排(src-tauri/src/main.rs + commands.rs):
- push/pull:占位任务(u64::MAX 递减 id)→ 真实 job 事件(SOURCE_STARTED/Started)取代占位 → TransferUpdated 500ms 节流(state.rs 里做节流:每 job_id 记 last_emit Instant,进度类事件 <500ms 丢弃,终态事件立即发)
- OfferReq 到达:ask 循环(参照桌面壳 main.rs ask_rx 循环,含看门狗 deadline+3s 清理)→ `OfferRequested` 事件;respond_offer 发 OfferResp{accepted, reason: None}
- xfer_lock(入站响应通道串行锁)照抄桌面壳——**并发传输必须排队,否则「通道已被占用」失败**
- pause/resume/cancel:ControlMsg::TransferCtl 映射
- 断点续传:冷启动 interrupted 检测+retry 走桌面壳 commands.rs 的 resume 路径(找到对应函数照抄调用)
- list_remote/remote_shares:发 ListReq/SharesReq 走 inbound 通道,10s 超时
- share_op:发 Task 1 消息,resp 通道等 ShareOpResult,10s 超时文案「对端版本过旧或离线,不支持此操作」
- backup_push:rel_dir 参数化 `LocalTransBackup/{name}/`

**实现者注意**:这个任务照抄桌面壳最多的一个。每个子功能先在 src-tauri/src/{main,commands}.rs 找到对应物再动手。约 600-800 行。

- [ ] **Step 4: 跑测试确认通过 + 交叉编译**

Run: `cargo test -p localtrans-ffi 2>&1 | tail -3 && cargo ndk -t arm64-v8a -t x86_64 -o ./target/android cargo build -p localtrans-ffi --release 2>&1 | tail -2`
Expected: PASS + .so 产出

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-ffi
git commit -m "feat(ffi): 传输/浏览/文件操作/备份 API 全量落地"
```

---

### Task 6: Android 工程骨架 + 构建集成(cargo-ndk 进 Gradle)

**Files:**
- Create: `android/settings.gradle.kts`
- Create: `android/build.gradle.kts`
- Create: `android/gradle.properties`
- Create: `android/local.properties`(gitignore;内容 sdk.dir)
- Create: `android/app/build.gradle.kts`
- Create: `android/app/src/main/AndroidManifest.xml`
- Create: `android/app/src/main/java/com/localtrans/app/MainActivity.kt`
- Create: `android/app/src/main/java/com/localtrans/app/LocalTransApp.kt`(进程级单例持有 ffi object)
- Create: `android/app/src/main/java/com/localtrans/app/bridge/EventRouter.kt`
- Create: `android/app/src/main/java/com/localtrans/app/ui/{theme,nav}.kt`
- Create: `android/app/proguard-rules.pro`
- Modify: `.gitignore`(android/local.properties、build/、.gradle/、uniffi 生成物策略:生成物提交)
- Test: 手动 `gradle assembleDebug` + adb 安装启动(hello 显示)

**Interfaces:**
- Consumes: Task 3-5 的 Kotlin 绑定(uniffi-bindgen 生成到 android/app/src/main/java/uniffi/)
- Produces(Task 7-8 的骨架):
  - `LocalTransApp.kt`:`object LocalTransBridge { lateinit var app: uniffi.localtrans.LocalTransApp; fun init(context: Context); val events: MutableSharedFlow<AppEvent> }`
  - `EventRouter.kt`:把 callback interface 适配成 SharedFlow 发射
  - MainActivity + Compose NavController,四目的地(devices/files/transfers/settings)空页面占位
  - Gradle task `buildRustSo`(Exec 任务跑 cargo ndk)+ `genUniffi`(跑 uniffi-bindgen),preBuild dependsOn 二者
  - debug 签名即可(release 签名 Task 10)

- [ ] **Step 1: 写工程文件**

`settings.gradle.kts`(阿里云镜像,照抄 asr_app 模板——路径 C:/Users/<user>/Desktop/work/asr_app/settings.gradle.kts 读它 copy):pluginManagement/dependencyResolutionManagement 镜像块 + `rootProject.name = "localtrans-android"` + `include(":app")`。

`build.gradle.kts`(根):照抄 asr_app/build.gradle.kts 的 plugins 块(AGP 8.7.3 + Kotlin 2.0.21 apply false)。

`gradle.properties`:照抄 asr_app/gradle.properties(jvmargs/AndroidX/nonTransitiveRClass)。

`app/build.gradle.kts`:

```kotlin
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.localtrans.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.localtrans.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.6.0"
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.ui:ui:1.7.5")
    implementation(platform("androidx.compose:compose-bom:2024.10.01"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.navigation:navigation-compose:2.8.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("app.cash.turbine:turbine:1.2.0")
}

// Rust 构建集成
val ndkDir = providers.exec { commandLine("bash","-c","echo $ANDROID_NDK_HOME") }.standardOutput.asText.get()
// 实际实现:直接读 local.properties 或环境变量,写死路径兜底:
val rustRoot = rootProject.projectDir.parentFile // 仓库根
tasks.register<Exec>("buildRustSo") {
    workingDir(rustRoot)
    commandLine("bash", "-c",
        "export ANDROID_NDK_HOME=\"C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973\" && " +
        "cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs cargo build -p localtrans-ffi --release")
}
tasks.register<Exec>("genUniffi") {
    dependsOn("buildRustSo")
    workingDir(rustRoot)
    commandLine("bash", "-c",
        "cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/release/liblocaltrans_ffi.dll --language kotlin --out-dir android/app/src/main/java/uniffi 2>&1 || " +
        "cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/cargo-ndk/android/x86_64/liblocaltrans_ffi.so --language kotlin --out-dir android/app/src/main/java/uniffi")
}
// 注:uniffi --library 需要能 dlopen 的产物;Windows 宿主用 host dll(先 cargo build -p localtrans-ffi --release 产 dll)。
// 实现时先试 host dll 路线,不行用 .so 路线,以能生成为准并记录。
tasks.named("preBuild") { dependsOn("genUniffi") }
```

(**上面的 Gradle 代码是骨架**——abiFilters/jniLibs 路径/命令引号转义在 Windows 上容易踩坑,实现者以「gradle assembleDebug 跑通」为唯一标准,允许调整命令写法,但不许砍掉 buildRustSo→genUniffi→preBuild 依赖链。)

`AndroidManifest.xml`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <uses-permission android:name="android.permission.INTERNET" />
    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
    <uses-permission android:name="android.permission.ACCESS_WIFI_STATE" />
    <uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
    <uses-permission android:name="android.permission.READ_MEDIA_IMAGES" />
    <uses-permission android:name="android.permission.READ_MEDIA_VIDEO" />
    <uses-permission android:name="android.permission.READ_EXTERNAL_STORAGE" android:maxSdkVersion="32" />
    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />
    <application
        android:label="LocalTrans"
        android:hardwareAccelerated="true"
        android:usesCleartextTraffic="false"
        android:theme="@style/Theme.Material3.DayNight.NoActionBar">
        <activity android:name=".MainActivity" android:exported="true"
            android:configChanges="orientation|screenSize|keyboardHidden">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
    </application>
</manifest>
```

`LocalTransApp.kt`:

```kotlin
package com.localtrans.app

import android.content.Context
import com.localtrans.app.bridge.EventRouter
import kotlinx.coroutines.flow.MutableSharedFlow
import uniffi.localtrans.AppEvent
import uniffi.localtrans.LocalTransCallback
import uniffi.localtrans.LocalTransApp as FfiApp

object LocalTransBridge {
    lateinit var app: FfiApp
        private set
    val events = MutableSharedFlow<AppEvent>(extraBufferCapacity = 256)

    fun init(context: Context) {
        if (::app.isInitialized) return
        val dataDir = context.filesDir.resolve("localtrans").absolutePath
        app = FfiApp(dataDir, Callback())
        app.start()
    }

    class Callback : LocalTransCallback {
        override fun onEvent(event: AppEvent) {
            events.tryEmit(event)
        }
    }
}
```

`MainActivity.kt`:setContent { MaterialTheme { AppNav() } },AppNav 四占位页(NavHost + BottomNavigation,图标用 Material Icons 内建)。

`EventRouter.kt`:暂为 SharedFlow 直通(Task 7 扩)。

- [ ] **Step 2: 构建 APK**

```bash
cd android
export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8"
"C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat" assembleDebug --no-daemon 2>&1 | tail -8
```
Expected: BUILD SUCCESSFUL,产 app/build/outputs/apk/debug/app-debug.apk。**首次跑会下载依赖(阿里云镜像),若 20 分钟无进展报 BLOCKED 附 gradle 日志。**

- [ ] **Step 3: 模拟器验证**

```bash
"C:/leidian/LDPlayer9/ldconsole.exe" launch --index 0
"C:/leidian/LDPlayer9/adb.exe" install -r android/app/build/outputs/apk/debug/app-debug.apk
"C:/leidian/LDPlayer9/adb.exe" shell am start -n com.localtrans.app/.MainActivity
"C:/leidian/LDPlayer9/adb.exe" shell screencap -p /sdcard/s.png && "C:/leidian/LDPlayer9/adb.exe" pull /sdcard/s.png tmp/android-skeleton.png
```
Expected: 截图里四个 Tab 可见,无崩溃(`adb logcat -d | grep -i "FATAL\|localtrans" | tail -5` 无 FATAL)。

- [ ] **Step 4: Kotlin 单测(桥初始化可测部分)**

EventRouter/SharedFlow 的单测(纯 JVM):发事件→收事件。`gradle.bat app/testDebugUnitTest` PASS。

- [ ] **Step 5: 提交**

```bash
git add android .gitignore
git commit -m "feat(android): 工程骨架+cargo-ndk/uniFFI 构建链集成,四页导航跑通"
```

---

### Task 7: Android — 设备页 + 配对流程 UI

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesViewModel.kt`
- Create: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt`
- Create: `android/app/src/main/java/com/localtrans/app/ui/pairing/PairingDialogs.kt`(同意门/配对码/输码三态弹窗)
- Test: `android/app/src/test/java/com/localtrans/app/ui/devices/DevicesViewModelTest.kt`

**Interfaces:**
- Consumes: Task 6 的 `LocalTransBridge.app`(devices/connect_device/respond_consent/submit_pairing_code/cancel_wait/disconnect/set_hidden/my_fingerprint)与 `LocalTransBridge.events`(DevicesChanged/ConsentRequested/PairingCodeShown/PairingCodeEntry/PairingWaitConsent/PairingResult/SessionUp/SessionDown)
- Produces(Task 8 传输页复用):`DevicesViewModel.devices: StateFlow<List<DeviceUi>>`(`DeviceUi{fingerprint,name,online,connected,viaRelay}`)

- [ ] **Step 1: 写失败测试(DevicesViewModelTest.kt)**

```kotlin
class DevicesViewModelTest {
    // 直接 new ViewModel,注入 fake LocalTransBridge(把 Bridge 抽成 interface DevicesRepo,
    // 生产实现包 ffi 调用,测试用 FakeDevicesRepo 发事件)
    @Test fun `consent event shows dialog`() { /* events.emit(ConsentRequested) → uiState.pairing 可见 */ }
    @Test fun `code shown event displays code`() { /* PairingCodeShown → uiState.pairing.code == "123456" */ }
    @Test fun `session up marks connected`() { /* SessionUp → devices 里 connected=true */ }
    @Test fun `devices changed refreshes list`() { /* DevicesChanged → devices 从 repo 快照拉 */ }
}
```

(repo 接口抽法:`interface DevicesRepo { fun devices(): List<DeviceDto>; fun connect(fp: String); ...; val events: SharedFlow<AppEvent> }`,ViewModel 只依赖接口——ffi object 不好直接 mock。)

- [ ] **Step 2: 跑测试确认失败**

```bash
cd android && "C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat" app:testDebugUnitTest 2>&1 | tail -5
```
Expected: 编译错(类不存在)

- [ ] **Step 3: 实现**

- DevicesScreen:LazyColumn 设备卡(名称/指纹前 8 位/在线点/远程徽标/已连接标记),顶部我的指纹+隐身 Switch,卡片点击→connect,菜单(浏览文件=导航 files 页带 fp 参数/推送文件=SAF picker/断开)
- PairingDialogs:三分支——a) ConsentRequested→「XX 请求连接」+同意/拒绝;b) PairingCodeShown→显示 6 位码(大字)+结束等待;c) PairingCodeEntry→输码框+提交
- SAF picker:`rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments)`,选中 URI → FileResolver(本任务先建 `util/FileResolver.kt` 骨架:content URI → 复制到 cacheDir 返回绝对路径;实现+单测放 Task 8)

- [ ] **Step 4: 测试通过 + 模拟器冒烟**

```bash
gradle.bat app:testDebugUnitTest && gradle.bat assembleDebug
adb install -r ... && adb shell am start ... && screencap → tmp/devices-page.png
```
Expected: 单测 4+ PASS;截图显示设备页(无设备空态文案「等待发现设备…」)

- [ ] **Step 5: 提交**

```bash
git add android
git commit -m "feat(android): 设备页+配对三态弹窗(同意门/显码/输码)"
```

---

### Task 8: Android — 传输页 + 接收确认 + 文件浏览器 + 设置页

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/transfers/{TransfersViewModel.kt,TransfersScreen.kt,OfferSheet.kt}`
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/{FilesViewModel.kt,FilesScreen.kt}`
- Create: `android/app/src/main/java/com/localtrans/app/ui/settings/{SettingsViewModel.kt,SettingsScreen.kt}`
- Create: `android/app/src/main/java/com/localtrans/app/backup/{BackupCoordinator.kt,MediaScanner.kt}`
- Create: `android/app/src/main/java/com/localtrans/app/util/{FileResolver.kt,Formatters.kt}`
- Test: `android/app/src/test/java/com/localtrans/app/ui/transfers/TransfersViewModelTest.kt`、`.../files/FilesViewModelTest.kt`、`.../backup/MediaScannerTest.kt`、`.../util/FileResolverTest.kt`

**Interfaces:**
- Consumes: Task 5-7 全部 ffi API + DevicesViewModel;`clean_backup_dir_name`(Kotlin 侧从生成绑定调)
- Produces: 完整四页可用 App;`BackupCoordinator.start()/stop()`(生命周期挂 MainActivity onStart/onStop)

- [ ] **Step 1: 写失败测试(选核心 6 个)**

```kotlin
// TransfersViewModelTest
@Test fun `offer requested shows sheet with countdown`() { /* OfferRequested(deadline=now+60s) → uiState.offer!=null && remainingSecs<=60 */ }
@Test fun `offer timeout deadline passed dismisses sheet`() { /* advanceTimeBy(61s) → offer==null */ }
@Test fun `transfer done ok updates state`() { /* TransferDone(ok=true) → 列表项 state=done */ }
@Test fun `failed with reason shows fail reason`() { /* TransferDone(ok=false,reason="对方超时未确认") → failReason 显示 */ }
// FilesViewModelTest
@Test fun `entries filter by query`() { /* list 返回3项,query="a" → 1项 */ }
@Test fun `local remote switch keeps breadcrumb`() { /* 切位置清 path 到根 */ }
// MediaScannerTest(纯 JVM:游标逻辑用接口注入 fake)
@Test fun `scan returns new media beyond cursor`() { /* fake 游标 id 5,6,7;上次游标 5 → 返回 6,7;新游标 7 */ }
@Test fun `cursor not advanced when push fails`() { /* push 抛错 → cursor 不动 */ }
// FileResolverTest(Robolectric 或手写 fake ContentResolver——若引 Robolectric 成本高,改成测纯函数部分:文件名清洗/大小格式化,URI 解析部分留模拟器手动验证并注明)
```

- [ ] **Step 2: 跑测试确认失败**

Run: `gradle.bat app:testDebugUnitTest`
Expected: 编译错

- [ ] **Step 3: 实现**

- **TransfersScreen**:任务列表(方向图标/名称/进度条/速度/ETA/状态徽章),操作按钮(pause/resume/cancel/retry——retry 只对 failed+拒绝/超时显示,与桌面一致)
- **OfferSheet**:ModalBottomSheet——文件数/总大小/倒计时(`LaunchedEffect` 每秒 tick,deadline 到自动关;最后 10s 红色),接收/拒绝两钮
- **FilesScreen**:位置切换(本机/远程设备 Tab)、面包屑、列表(长按多选→底部操作栏 推送/重命名/删除)、FAB 新建文件夹、搜索框(当前列表按名过滤)、文件点击→Intent.ACTION_VIEW(FileProvider 提供下载区文件)
- **SettingsScreen**:设备名/隐身/共享区路径显示/中继配置/超时两输入(15-600 钳制)/备份卡片(开关+目标设备下拉+照片/视频两开关+「立即备份」按钮)
- **BackupCoordinator**:onStart 时若 enabled→MediaScanner 扫描(MediaStore.Images/Videos,游标=`max(_id)`,持久化到 SharedPreferences;大小上限单文件 500MB 跳过并记数)→逐批(每批 ≤20 个)`backup_push` → BackupProgress 事件 → 全部成功前移游标
- **FileResolver**:OpenMultipleDocuments 返回的 content URI → `_data` 列试取真实路径,取不到复制到 cacheDir;SAF 选文件夹用 OpenDocumentTree

- [ ] **Step 4: 测试通过 + assembleDebug**

Run: `gradle.bat app:testDebugUnitTest 2>&1 | tail -3 && gradle.bat assembleDebug 2>&1 | tail -3`
Expected: 全 PASS + APK 产出

- [ ] **Step 5: 提交**

```bash
git add android
git commit -m "feat(android): 传输页+接收确认+文件浏览器+设置页+相册备份"
```

---

### Task 9: E2E 互通验证(模拟器 ⇄ 桌面)+ 修补

**Files:**
- Create: `docs/e2e-android-manual.md`(手动验证清单脚本)
- Modify: 发现的任何 bug 对应文件(预计少量)

**Interfaces:**
- Consumes: 全部前序任务产物
- Produces: 验证清单全绿的证据(logcat 摘录/截图存 tmp/);修补提交

- [ ] **Step 1: 写验证清单**

docs/e2e-android-manual.md 覆盖(每项:步骤/预期/证据):
1. 模拟器装 APK,启动无崩溃,生成指纹
2. 桌面 exe(v0.5.0)同网启动 → 双方互见(模拟器网络用桥接模式)
3. 手机→桌面发起连接:桌面同意门+显码,手机输码,配对成功
4. 桌面→手机发起连接:手机同意门+显码,桌面输码
5. 手机推 3 个小文件到桌面:桌面确认弹窗→接收→完成+落盘
6. 桌面推文件到手机:手机 OfferSheet 倒计时→接收→下载区落盘
7. 拒绝路径:手机推→桌面拒→手机任务显示「已拒绝」+可重发
8. 超时路径:手机推→桌面不点→60s 超时→手机显示「已超时」
9. 手机浏览桌面共享区:列表/进入子目录/下载一个文件
10. 手机远程重命名桌面文件:生效(桌面 v0.6.0)或报「不支持」(桌面 v0.5.0——预期报错,验证不崩)
11. 相册备份:模拟器放 2 张图(dcim push)→开备份→桌面收到 LocalTransBackup/<手机名>/
12. 断点续传:推大文件中途杀 app→重开→恢复横幅→续传完成
13. 中继路径(可选,有服务器才测):两边配中继→远程徽标→互推

- [ ] **Step 2: 逐项执行+记录**

按清单跑,每项截图/logcat 存 `tmp/e2e/`。LDPlayer 网络切桥接(设置→网络→桥接网卡)保证与桌面同网段;若桥接不可用,降级验证:桌面跑中继 localtrans-relay.exe,两端走中继路径覆盖发现+传输。

- [ ] **Step 3: 修补发现的问题**

每个 bug:定位→最小修复→对应层测试补充→提交(fix: 前缀)。**共性问题(如 JNI 回调线程安全、发现包在模拟器丢)优先修。**

- [ ] **Step 4: 提交清单+证据**

```bash
git add docs/e2e-android-manual.md
git commit -m "docs: 安卓端 E2E 手动验证清单与结果"
```

---

### Task 10: 版本收尾 0.6.0 + APK 打包分发

**Files:**
- Modify: `Cargo.toml`(workspace version 0.5.0→0.6.0)
- Modify: `src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`(0.6.0)
- Modify: `CHANGELOG.md`
- Create: `dist/localtrans-android-v0.6.0/usage-android.md`
- Create: release 签名配置(keystore 生成本地不提交,keystore.properties 模板进 .gitignore)

**Interfaces:**
- Consumes: Task 9 全绿
- Produces: `dist/localtrans-android-v0.6.0/localtrans-v0.6.0-android.apk`(arm64-v8a+x86_64,release 签名)+ usage-android.md;git tag v0.6.0

- [ ] **Step 1: 版本对齐**

workspace Cargo.toml `[workspace.package]` version、src-tauri/Cargo.toml、src-tauri/tauri.conf.json 三处 → `0.6.0`。`cargo build --release -p localtrans-core 2>&1 | tail -2`(验证 workspace 编译,Cargo.lock 随动)。

- [ ] **Step 2: CHANGELOG**

```markdown
## v0.6.0 — 安卓客户端首发 - 2026-08-22

### 新增
- Android 客户端(Compose + uniFFI 复用 core):配对/互传/接收确认与桌面 v0.5.0 互通
- 移动端文件浏览器:本地+远程浏览/重命名/删除/新建/搜索/系统打开方式
- 相册自动备份(前台增量,MediaStore 游标,存对方共享区 LocalTransBackup/)
- core:share 协议新增 ShareRename/ShareDelete/ShareMkdir/ShareOpResult(老版本优雅降级)
- crates/localtrans-ffi:uniFFI facade(AppEvent 事件流+全量 API)
```

- [ ] **Step 3: release 签名 + APK**

```powershell
# keystore 一次性生成(密码用户自己定,妥善备份)
& "C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8/bin/keytool.exe" -genkeypair -v -keystore android/keystore/release.keystore -alias localtrans -keyalg RSA -keysize 2048 -validity 10950 -storepass <密码> -keypass <密码> -dname "CN=LocalTrans, OU=Dev, O=LocalTrans, C=CN"
```

android/keystore.properties(gitignore):storeFile/storePassword/keyAlias/keyPassword。app/build.gradle.kts 补 signingConfigs read(照抄 asr_app 模板 §5.2)。`gradle.bat assembleRelease` → apksigner verify。

- [ ] **Step 4: dist 组装**

```
dist/localtrans-android-v0.6.0/
├── localtrans-v0.6.0-android.apk
└── usage-android.md   # 安装(允许未知来源)/配对/推送确认/备份开关说明/桌面端需 v0.5.0+ 与文件操作需 v0.6.0+
```

桌面版产物不重打(v0.5.0 已在 dist;本版桌面功能零变化只升版本号——**如果用户要求桌面也重打,在此步骤补**)。

- [ ] **Step 5: 提交+tag**

```bash
git add -A && git commit -m "chore: v0.6.0 版本收尾——安卓客户端首发"
git tag v0.6.0
```

---

## Self-Review 结论(写完计划后自查)

1. **Spec 覆盖**:spec §0 十一项 → T3(路线7/8)、T4(发现9/设置)、T5(备份4/推送5)、T6-8(UI/权限/SAF)、T9(E2E)、T10(分发11)均有对应;spec §2.3 三消息 → T1-2;spec §5.1 四层测试 → 各任务内嵌+T9;spec §6 YAGNI 无违例。
2. **占位符扫描**:Task 2/3/5 的测试骨架标注了「基建照抄现有测试」并指明参照物(行号/测试名),这属于指向真实存在的代码而非 TBD;Task 6 Gradle 块标注「以跑通为准允许调整命令写法」——边界清楚,非空指令。
3. **类型一致性**:AppEvent 变体名在 T3(Hello)→T4(全量)演进,T4 Interfaces 块是权威清单;DeviceDto/TransferDto/SettingsDto/FileEntryDto 字段在 T4/T5 Interfaces 各自定义且不重叠;`clean_backup_dir_name` T5 产出 T8 消费,签名一致(String→String)。
