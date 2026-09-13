# lan-drop 阶段一(局域网 P2P)实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 构建可日常使用的 Windows 绿色便携版局域网加密文件互传工具(发现/配对/加密传输/断点续传/共享区/推送/UI);中继属阶段二,另行计划。

**Architecture:** Cargo workspace:`crates/landrop-core`(全部网络与业务逻辑,纯库,可独立测试)+ `src-tauri`(Tauri 2 壳:commands/events)+ `ui`(Vue 3 前端)。传输走 QUIC(quinn,TLS 1.3 强制,自签证书 + 指纹锁定),发现走 UDP 广播(Ed25519 签名)。

**Tech Stack:** Rust(stable, edition 2021)、tokio、quinn 0.11、rustls 0.23(ring 后端)、rcgen 0.13、ed25519-dalek 2、serde/serde_json、sha2、hkdf;Tauri 2、Vue 3 + TypeScript + Vite 5 + Pinia。

**Spec:** `docs/superpowers/specs/2026-08-20-lan-drop-design.md`(执行者需同时阅读规格与本计划)

## Global Constraints

- 仅 Windows(Win11 开发机);`landrop-core` 测试必须在 Windows 全绿
- 端口:发现 UDP `47600`,QUIC UDP `47601`(中继 `47602` 属阶段二)
- 分块 `CHUNK_SIZE = 4 MiB`;全局缓冲池 32 块 = 128 MiB 硬上限
- 并发流数 4 起步、上限 16,每 500ms 探测;丢包 >5% 回落
- TLS 1.3 only;rustls 统一用 **ring** provider(禁 aws-lc-rs,避免 Windows cmake 依赖)
- 一切持久化数据在 **exe 同级 `data\`**(绿色便携);除用户主动点防火墙按钮(netsh + UAC)外不写注册表/系统目录
- 设备身份 = Ed25519 密钥对 + 自签证书;`指纹 = SHA-256(cert DER)`;**不在信任列表一律触发配对(双向 SAS)**
- 验证码错 3 次冷却 5 分钟;共享区路径必须 canonicalize + 前缀校验 + 拒绝重解析点
- UI 文案全部中文;错误提示包含"发生了什么 + 能怎么办"
- Conventional Commits;每任务以测试全绿后提交收尾
- 依赖版本以各任务 Cargo.toml 片段为准

## File Structure(阶段一终态)

```
D:\localTrans\
├── Cargo.toml                  # workspace: crates/landrop-core, src-tauri
├── crates/
│   └── landrop-core/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs          # 模块声明与重导出
│           ├── store.rs        # 便携数据目录 + config.json
│           ├── identity.rs     # 密钥/证书/指纹 + 信任列表
│           ├── protocol.rs     # 控制消息 + 块流帧
│           ├── discovery.rs    # 广播包格式 + UDP 服务 + 设备缓存
│           ├── share.rs        # 共享区注册/路径安全/分页
│           ├── session.rs      # QUIC endpoint + 会话管理 + 配对状态机
│           ├── pairing.rs      # SAS 验证码派生
│           └── transfer/
│               ├── mod.rs      # 任务调度
│               ├── manifest.rs # 分块清单/位图/对账
│               ├── engine.rs   # 发送/接收引擎 + 缓冲池
│               └── adapt.rs    # 自适应流数
├── src-tauri/                  # Tauri 壳
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/
│       ├── main.rs             # 入口/单实例/WebView2 检测
│       ├── commands.rs         # 全部 Tauri command
│       ├── events.rs           # 事件节流推送
│       └── firewall.rs         # netsh 一键放行
└── ui/                         # Vue 3
    ├── package.json
    ├── vite.config.ts
    └── src/
        ├── App.vue / main.ts / api.ts
        ├── stores/{devices,transfers,settings}.ts
        ├── pages/{Devices,Transfers,Browse,Settings}.vue
        └── components/{DeviceCard,TransferItem,PairingDialog,CodeBadge}.vue
```

---

### Task 1: Workspace 脚手架

**Files:**
- Create: `Cargo.toml`、`crates/landrop-core/Cargo.toml`、`crates/landrop-core/src/lib.rs`、`.gitignore`

**Interfaces:**
- Produces: crate 名 `landrop-core`,lib 目标;后续所有任务的模块都挂在这个 crate 下

- [ ] **Step 1: 写 workspace 根 Cargo.toml**

```toml
[workspace]
resolver = "2"
members = ["crates/landrop-core", "src-tauri"]

[workspace.package]
edition = "2021"
version = "0.1.0"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
quinn = "0.11"
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }
rcgen = { version = "0.13", default-features = false, features = ["pem", "ring", "x509"] }
ed25519-dalek = { version = "2", features = ["rand_core", "pkcs8"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
hkdf = "0.12"
rand = "0.8"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

注意 `src-tauri` 此时尚不存在,先在 members 里注释掉:`members = ["crates/landrop-core"]  # , "src-tauri"`(Task 16 再启用)。

- [ ] **Step 2: 写 crates/landrop-core/Cargo.toml**

```toml
[package]
name = "landrop-core"
edition.workspace = true
version.workspace = true

[dependencies]
tokio = { workspace = true }
quinn = { workspace = true }
rustls = { workspace = true }
rcgen = { workspace = true }
ed25519-dalek = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
hkdf = { workspace = true }
rand = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
rustls-pki-types = "1"

[dev-dependencies]
tempfile = "3"
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3: 写 lib.rs 与首个冒烟测试**

```rust
// crates/landrop-core/src/lib.rs
pub mod store;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() { assert_eq!(2 + 2, 4); }
}
```

```rust
// crates/landrop-core/src/store.rs —— 本任务先放一个空占位模块
```
(占位文件内容仅一行注释 `// implemented in Task 2`,保证 lib.rs 可编译。)

- [ ] **Step 4: 写 .gitignore 并验证编译+测试**

`.gitignore`:
```
target/
node_modules/
dist/
data/
*.part
```

Run: `cargo test -p landrop-core`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ .gitignore
git commit -m "chore: cargo workspace 脚手架(landrop-core)"
```

---

### Task 2: store — 便携数据目录与 config.json

**Files:**
- Modify: `crates/landrop-core/src/store.rs`(替换占位)
- Test: `crates/landrop-core/src/store.rs`(内嵌 `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub fn data_dir() -> std::io::Result<PathBuf>` —— exe 同级 `data\`,不存在则创建
  - `pub struct Config { pub device_name: String, pub download_dir: PathBuf, pub hidden: bool, pub quic_port: u16, pub discovery_port: u16, pub shares: Vec<ShareDef> }`(全字段 `Serialize/Deserialize`,`Default` 派生;默认 `quic_port=47601, discovery_port=47600, hidden=false, device_name=COMPUTERNAME 环境变量或 "我的电脑", download_dir=exe同级\downloads`)
  - `pub struct ShareDef { pub id: String, pub alias: String, pub path: PathBuf }`
  - `pub fn load_config(dir: &Path) -> Config`(缺文件/损坏 → 返回默认并重写)
  - `pub fn save_config(dir: &Path, cfg: &Config) -> std::io::Result<()>`(原子写:先 `config.json.tmp` 再 `rename`)

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn config_roundtrip() {
        let dir = tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.device_name = "测试机".into();
        cfg.shares.push(ShareDef { id: "s1".into(), alias: "电影".into(), path: "D:\\video".into() });
        save_config(dir.path(), &cfg).unwrap();
        let loaded = load_config(dir.path());
        assert_eq!(loaded.device_name, "测试机");
        assert_eq!(loaded.shares[0].alias, "电影");
        assert_eq!(loaded.quic_port, 47601);
        assert_eq!(loaded.discovery_port, 47600);
    }

    #[test]
    fn corrupted_config_falls_back_to_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{invalid json").unwrap();
        let cfg = load_config(dir.path());
        assert_eq!(cfg.quic_port, 47601); // 默认值
        // 且已自动重写为合法文件
        assert!(serde_json::from_str::<Config>(&std::fs::read_to_string(
            dir.path().join("config.json")).unwrap()).is_ok());
    }

    #[test]
    fn data_dir_is_exe_relative() {
        let d = data_dir().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(d, exe.parent().unwrap().join("data"));
        assert!(d.exists());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p landrop-core store`
Expected: 编译失败(`Config` 等未定义)

- [ ] **Step 3: 实现 store.rs**

```rust
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ShareDef { pub id: String, pub alias: String, pub path: PathBuf }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub device_name: String,
    pub download_dir: PathBuf,
    pub hidden: bool,
    pub quic_port: u16,
    pub discovery_port: u16,
    pub shares: Vec<ShareDef>,
}

impl Default for Config {
    fn default() -> Self {
        let exe_dir = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "我的电脑".into());
        Config {
            device_name: name,
            download_dir: exe_dir.join("downloads"),
            hidden: false,
            quic_port: 47601,
            discovery_port: 47600,
            shares: vec![],
        }
    }
}

pub fn data_dir() -> io::Result<PathBuf> {
    let dir = std::env::current_exe()?
        .parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no exe parent"))?
        .join("data");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn load_config(dir: &Path) -> Config {
    let path = dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!("config 损坏({e}),重置为默认");
            let c = Config::default();
            let _ = save_config(dir, &c);
            c
        }),
        Err(_) => { let c = Config::default(); let _ = save_config(dir, &c); c }
    }
}

pub fn save_config(dir: &std::path::Path, cfg: &Config) -> io::Result<()> {
    let tmp = dir.join("config.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).unwrap())?;
    std::fs::rename(&tmp, dir.join("config.json")) // Windows 上 rename 到已存在目标会失败
        .or_else(|_| std::fs::copy(&tmp, dir.join("config.json")).map(|_| ()))?;
    Ok(())
}
```

- [ ] **Step 4: 跑测试全绿**

Run: `cargo test -p landrop-core store`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add crates/landrop-core/src/store.rs
git commit -m "feat(core): 便携数据目录与 config 持久化"
```

---

### Task 3: identity — 密钥、证书与指纹

**Files:**
- Modify: `crates/landrop-core/src/identity.rs`(新建)、`lib.rs`(加 `pub mod identity;`)
- Test: `crates/landrop-core/src/identity.rs` 内嵌

**Interfaces:**
- Produces:
  - `pub type Fingerprint = [u8; 32];`
  - `pub struct Identity { pub signing: ed25519_dalek::SigningKey, pub cert: rustls_pki_types::CertificateDer<'static>, pub pkcs8: Vec<u8> }`
  - `impl Identity { pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError>; pub fn fingerprint(&self) -> Fingerprint; pub fn short_code(&self) -> String; }` —— `short_code` = 指纹前 4 字节大写 hex,格式 `A3F2-91BC`
  - `pub fn fingerprint_of(cert: &CertificateDer<'_>) -> Fingerprint`
  - 持久化:`data\identity.key`(PKCS#8 DER)、`data\cert.der`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_persist_reload_same_fingerprint() {
        let dir = tempdir().unwrap();
        let id1 = Identity::load_or_create(dir.path()).unwrap();
        let fp1 = id1.fingerprint();
        let id2 = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(fp1, id2.fingerprint(), "重载后指纹必须一致");
        assert!(dir.path().join("identity.key").exists());
        assert!(dir.path().join("cert.der").exists());
    }

    #[test]
    fn fingerprint_is_sha256_of_cert_der() {
        use sha2::{Digest, Sha256};
        let dir = tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(id.fingerprint().to_vec(),
            Sha256::digest(id.cert.as_ref()).to_vec());
    }

    #[test]
    fn short_code_format_and_uniqueness() {
        let a = Identity::load_or_create(&tempfile::tempdir().unwrap().path()).unwrap();
        let b = Identity::load_or_create(&tempfile::tempdir().unwrap().path()).unwrap();
        assert_eq!(a.short_code().len(), 9); // XXXX-XXXX
        assert_ne!(a.short_code(), b.short_code());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p landrop-core identity`
Expected: 编译失败

- [ ] **Step 3: 实现 identity.rs**

```rust
use ed25519_dalek::SigningKey;
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub type Fingerprint = [u8; 32];

#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("IO 错误: {0}")] Io(#[from] std::io::Error),
    #[error("密钥/证书解析错误: {0}")] Parse(String),
}

pub struct Identity {
    pub signing: SigningKey,
    pub cert: CertificateDer<'static>,
    pub pkcs8: Vec<u8>,
}

pub fn fingerprint_of(cert: &CertificateDer<'_>) -> Fingerprint {
    Sha256::digest(cert.as_ref()).into()
}

impl Identity {
    pub fn load_or_create(dir: &std::path::Path) -> Result<Self, IdentityError> {
        let key_path = dir.join("identity.key");
        let cert_path = dir.join("cert.der");
        if key_path.exists() && cert_path.exists() {
            let pkcs8 = std::fs::read(&key_path)?;
            let cert = CertificateDer::from(std::fs::read(&cert_path)?);
            let signing = SigningKey::from_pkcs8_der(&PrivatePkcs8KeyDer::from(pkcs8.clone()))
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            return Ok(Identity { signing, cert, pkcs8 });
        }
        // 生成:dalek 生成密钥 → rcgen 用同一密钥签自签证书 → 落盘
        let mut csprng = rand::rngs::OsRng;
        let signing = SigningKey::generate(&mut csprng);
        let pkcs8 = signing.to_pkcs8_der()
            .map_err(|e| IdentityError::Parse(e.to_string()))?
            .to_bytes().to_vec();
        let kp = rcgen::KeyPair::try_from(&pkcs8[..])
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        let params = rcgen::CertificateParams::new(vec!["lan-drop".to_string()])
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        let cert = params.self_signed(&kp)
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        let cert_der = CertificateDer::from(cert.der().to_vec());
        std::fs::write(&key_path, &pkcs8)?;
        std::fs::write(&cert_path, cert_der.as_ref())?;
        Ok(Identity { signing, cert: cert_der, pkcs8 })
    }

    pub fn fingerprint(&self) -> Fingerprint { fingerprint_of(&self.cert) }

    pub fn short_code(&self) -> String {
        let fp = self.fingerprint();
        format!("{}-{}",
            hex_upper(&fp[0..2]), hex_upper(&fp[2..4]))
            .chars().take(9).collect::<String>()
    }
}

fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{X:02X}", x = x)).collect()
}
```

注意:`short_code` 按"指纹前 4 字节"实现,上面 `hex_upper(&fp[0..2])` 拼接后恰好 4 字节;若实现时调整,以测试 `len()==9` 为准。

- [ ] **Step 4: 跑测试全绿**

Run: `cargo test -p landrop-core identity`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add crates/landrop-core/src/identity.rs crates/landrop-core/src/lib.rs
git commit -m "feat(core): 设备身份——Ed25519 密钥+自签证书+指纹"
```

---

### Task 4: identity — 信任列表 trusted_peers.json

**Files:**
- Modify: `crates/landrop-core/src/identity.rs`(追加)
- Test: 同文件内嵌

**Interfaces:**
- Produces:
  - `pub enum PushPolicy { Ask, Auto, Deny }`(serde 小写标签)
  - `pub struct Perms { pub browse: bool, pub download: bool, pub push: PushPolicy }`(默认 `browse=true, download=true, push=Ask`)
  - `pub struct TrustedPeer { pub fingerprint: Fingerprint, pub name: String, pub paired_at: u64, pub perms: Perms }`(serde:fingerprint 用 hex 字符串,写 `#[serde(with = "fp_hex")]` 模块)
  - `pub struct TrustStore`(内部 `Vec<TrustedPeer>` + 所在目录)
  - `impl TrustStore { pub fn load(dir: &Path) -> Self; pub fn save(&self) -> std::io::Result<()>; pub fn is_trusted(&self, fp: &Fingerprint) -> bool; pub fn upsert(&mut self, TrustedPeer); pub fn remove(&mut self, fp: &Fingerprint) -> bool; pub fn get(&self, fp) -> Option<&TrustedPeer>; pub fn set_perms(&mut self, fp, Perms) -> bool; }`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn trust_store_crud_and_default_perms() {
    let dir = tempdir::tempdir().unwrap();
    let mut ts = TrustStore::load(dir.path());
    let fp = [7u8; 32];
    assert!(!ts.is_trusted(&fp));
    ts.upsert(TrustedPeer { fingerprint: fp, name: "同事电脑".into(), paired_at: 1000, perms: Perms::default() });
    assert!(ts.is_trusted(&fp));
    assert!(matches!(ts.get(&fp).unwrap().perms.push, PushPolicy::Ask));
    ts.save().unwrap();
    let ts2 = TrustStore::load(dir.path());   // 从磁盘恢复
    assert_eq!(ts2.get(&fp).unwrap().name, "同事电脑");
    assert!(ts2.remove(&fp));
    assert!(!ts2.is_trusted(&fp));
}

#[test]
fn fingerprint_serializes_as_hex() {
    let p = TrustedPeer { fingerprint: [1u8; 32], name: "x".into(), paired_at: 0, perms: Perms::default() };
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains("01010101")); // hex 可读,而非数组
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test -p landrop-core trust`
Expected: 编译失败

- [ ] **Step 3: 实现**(追加到 identity.rs;`fp_hex` 模块:`serialize` = `hex::encode` 输出 String,`deserialize` 反解;`hex` 用 `hex = "0.4"` 依赖,加入 Cargo.toml)

`Perms::default()` 手写 impl(derive Default 无法给 enum 字段给 Ask):`Perms { browse: true, download: true, push: PushPolicy::Ask }`。
`TrustStore::load` 读 `trusted_peers.json`,损坏则重置为空(同 config 策略);`save` 原子写(复用 store.rs 的 tmp+rename 模式,把该模式提取为 `pub(crate) fn atomic_write(path, bytes)`,放 store.rs 并回填 store.rs 改用它)。

- [ ] **Step 4: 跑测试全绿**(含 Task 2 的测试仍绿,验证 atomic_write 重构无回归)

Run: `cargo test -p landrop-core`
Expected: all passed

- [ ] **Step 5: Commit**

```bash
git add crates/landrop-core/src
git commit -m "feat(core): 信任列表与每设备权限持久化"
```

---

### Task 5: protocol — 控制消息与块流帧

**Files:**
- Create: `crates/landrop-core/src/protocol.rs`;Modify: `lib.rs`

**Interfaces:**
- Produces:
  - `pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;`
  - `pub enum ControlMsg {...}`(下方完整定义,serde tag="type" 小写蛇形)
  - `pub fn encode_control(m: &ControlMsg) -> Vec<u8>` —— 4 字节大端长度前缀 + JSON
  - `pub fn decode_control(bytes: &[u8]) -> Result<ControlMsg, ProtocolError>`(长度不符报错)
  - `pub struct ChunkStreamHeader { pub job_id: u64, pub chunk: u32 }` + `pub const CHUNK_HEADER_LEN: usize = 12;` + `encode()/decode()`(小端定长)
  - 消息最大 4 MiB,超过拒绝

```rust
pub enum ControlMsg {
    Hello { name: String, fingerprint: String },          // hex 指纹
    PairCodeSubmit { code: String },
    PairResult { ok: bool },
    ListReq { share_id: String, cursor: u64 },
    ListResp { entries: Vec<FileEntry>, next_cursor: Option<u64> },
    SharesReq,
    SharesResp { shares: Vec<ShareInfo> },
    MetaReq { share_id: String, path: String },
    MetaResp { job_id: u64, file_name: String, total_size: u64, chunk_hashes: Vec<String> },
    FetchReq { job_id: u64, chunk: u32 },
    OfferReq { job_id: u64, files: Vec<OfferFile> },
    OfferResp { accepted: bool, save_dir: Option<String> },
    BitmapReq { job_id: u64 },
    BitmapResp { job_id: u64, bits: Vec<u8> },
    TransferCtl { job_id: u64, action: TransferAction },  // Pause/Resume/Cancel
    Goodbye,
}
pub enum TransferAction { Pause, Resume, Cancel }
pub struct FileEntry { pub name: String, pub is_dir: bool, pub size: u64, pub mtime: u64 }
pub struct ShareInfo { pub id: String, pub alias: String }
pub struct OfferFile { pub name: String, pub size: u64, pub rel_dir: String }
```

- [ ] **Step 1: 写失败测试**(round-trip 每种消息 + 帧头 + 超限拒绝 + 截断报错)

```rust
#[test]
fn control_roundtrip_all_variants() {
    let msgs = vec![
        ControlMsg::Hello { name: "甲".into(), fingerprint: "ab".repeat(32) },
        ControlMsg::ListReq { share_id: "s1".into(), cursor: 0 },
        ControlMsg::MetaResp { job_id: 9, file_name: "a.iso".into(), total_size: 1 << 40,
            chunk_hashes: vec!["cd".repeat(32)] },
        ControlMsg::BitmapResp { job_id: 1, bits: vec![0b1010_0001] },
        ControlMsg::TransferCtl { job_id: 2, action: TransferAction::Pause },
        // ...其余变体各来一条,确保全覆盖
    ];
    for m in &msgs {
        assert_eq!(&decode_control(&encode_control(m)).unwrap(), m);
    }
}

#[test]
fn chunk_header_roundtrip() {
    let h = ChunkStreamHeader { job_id: u64::MAX, chunk: 42 };
    let b = h.encode();
    assert_eq!(b.len(), CHUNK_HEADER_LEN);
    assert_eq!(ChunkStreamHeader::decode(&b).unwrap(), h);
}

#[test]
fn oversize_and_truncated_rejected() {
    let big = vec![0u8; 5 * 1024 * 1024];
    assert!(decode_control(&big).is_err());
    let enc = encode_control(&ControlMsg::Goodbye);
    assert!(decode_control(&enc[..enc.len() - 1]).is_err());
}
```

(测试里给 `ControlMsg` 等补上 `PartialEq, Debug` derive。)

- [ ] **Step 2: 确认失败** → Run: `cargo test -p landrop-core protocol`,Expected: 编译失败

- [ ] **Step 3: 实现 protocol.rs**(serde derive + 定长头;`decode` 先校验 `u32::from_be_bytes` 长度与实际一致、≤4MiB;`ChunkStreamHeader::encode` 用 `to_le_bytes` 拼接)

- [ ] **Step 4: 跑测试全绿** → `cargo test -p landrop-core protocol`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 控制协议消息与块流帧编解码"`

---

### Task 6: discovery — 广播包格式、签名与防重放

**Files:**
- Create: `crates/landrop-core/src/discovery.rs`(第一部分:包格式);Modify: `lib.rs`

**Interfaces:**
- Produces:
  - `pub enum PacketKind { Presence, Probe, ProbeResp }`
  - `pub struct DiscoveryPacket { pub v: u8, pub kind: PacketKind, pub name: String, pub fingerprint: [u8; 32], pub quic_port: u16, pub ts_ms: u64, pub nonce: [u8; 12] }`
  - `pub fn encode_and_sign(pkt: &DiscoveryPacket, key: &SigningKey) -> Vec<u8>` —— 规范化 JSON(字段定序)+ 64 字节签名追加
  - `pub fn verify_and_parse(bytes: &[u8], now_ms: u64, replay: &mut ReplayGuard) -> Result<DiscoveryPacket, DiscoveryError>`
  - `pub struct ReplayGuard`(内部 `HashMap<[u8;12], u64>`,清理 ts 窗口外的条目;容量上限 4096,超限整体清空重建)
  - 错误:`DiscoveryError::{BadSignature, Stale, Replay, Malformed}`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn sign_verify_roundtrip() {
    let mut csprng = rand::rngs::OsRng;
    let key = SigningKey::generate(&mut csprng);
    let pkt = DiscoveryPacket { v: 1, kind: PacketKind::Presence, name: "甲".into(),
        fingerprint: [2; 32], quic_port: 47601, ts_ms: 1_000_000, nonce: [9; 12] };
    let mut guard = ReplayGuard::new();
    let parsed = verify_and_parse(&encode_and_sign(&pkt, &key), 1_000_000, &mut guard).unwrap();
    assert_eq!(parsed.name, "甲");
}

#[test]
fn tampered_packet_rejected() {
    // ...签好包后翻转 payload 一个字节
    let mut raw = encode_and_sign(&pkt, &key);
    let i = raw.len() / 2; raw[i] ^= 1;
    assert!(matches!(verify_and_parse(&raw, now, &mut guard),
        Err(DiscoveryError::BadSignature)));
}

#[test]
fn stale_and_replay_rejected() {
    // ts 偏离 now ±30s → Stale
    // 同一 nonce 二次提交 → Replay(第一次 Ok)
}

#[test]
fn unsigned_or_short_rejected() {
    assert!(matches!(verify_and_parse(b"{}", now, &mut guard), Err(DiscoveryError::Malformed)));
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core discovery`

- [ ] **Step 3: 实现**:规范化 JSON = `serde_json::to_vec`(serde_json 的 struct 字段序即声明序,稳定);签名 = `ed25519_dalek::Signer::sign` 覆盖 JSON 字节;`verify_and_parse`:长度 ≥64 → 拆签名与 JSON → `VerifyingKey::from_bytes(&pkt.fingerprint)`(**用包内声明指纹反推公钥验签** —— 身份与签名绑定,防冒名)→ 验 ts → 查 nonce → Ok。注意:`VerifyingKey::from_bytes` 需要 `VerifyingKey`,从 SigningKey 用 `verifying_key()` 生成测试数据。

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core discovery`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 发现包签名/验签/防重放"`

---

### Task 7: discovery — UDP 服务、隐身与设备缓存

**Files:**
- Modify: `crates/landrop-core/src/discovery.rs`(追加服务部分)

**Interfaces:**
- Consumes: Task 6 的包格式;Task 3 的 `Identity`
- Produces:
  - `pub struct DiscoveryConfig { pub bind_port: u16, pub target: SocketAddr, pub hidden: Arc<AtomicBool>, pub name: String, pub quic_port: u16 }`(`target` 生产=广播 `255.255.255.255:47600`,测试=对方单播地址 —— 解决回环无法广播的问题)
  - `pub struct DeviceInfo { pub name: String, pub fingerprint: [u8; 32], pub addr: SocketAddr, pub last_seen: Instant }`(serde 用时转 `secs_since_seen`)
  - `pub struct DiscoveryHandle { pub devices: tokio::sync::watch::Receiver<Vec<DeviceInfo>>, pub cmd: mpsc::Sender<DiscoveryCmd>, pub shutdown: mpsc::Sender<()> }`
  - `pub enum DiscoveryCmd { ProbeNow }`
  - `pub fn spawn(cfg: DiscoveryConfig, key: Arc<SigningKey>) -> io::Result<DiscoveryHandle>`
  - 行为:启动连发 3 个 Presence;之后每 5s±1s 一个;收到 Probe 且 `!hidden` → 单播 ProbeResp;收到 Presence/ProbeResp → 更新缓存(watch 发布快照);15s 未更新移除;`SO_BROADCAST` + `SO_REUSEADDR`

- [ ] **Step 1: 写失败测试**(集成风格,单机回环)

```rust
#[tokio::test]
async fn two_services_discover_each_other() {
    let (k1, k2) = (Arc::new(SigningKey::generate(&mut OsRng)), Arc::new(SigningKey::generate(&mut OsRng)));
    let a = spawn(DiscoveryConfig { bind_port: 14760, target: "127.0.0.1:14761".parse().unwrap(),
        hidden: Default::default(), name: "甲".into(), quic_port: 24761 }, k1.clone()).unwrap();
    let b = spawn(DiscoveryConfig { bind_port: 14761, target: "127.0.0.1:14760".parse().unwrap(),
        hidden: Default::default(), name: "乙".into(), quic_port: 24762 }, k2.clone()).unwrap();
    a.cmd.send(DiscoveryCmd::ProbeNow).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let list = b.devices.borrow().clone();
    assert!(list.iter().any(|d| d.name == "甲"), "乙 应看到甲");
    let _ = (a.shutdown.send(()), b.shutdown.send(()));
}

#[tokio::test]
async fn hidden_device_not_listed_but_can_see() {
    // hidden=true 的乙:甲 probe 后,乙的列表能看到甲;甲的列表没有乙
}

#[tokio::test]
async fn stale_entries_expire() {
    // 起乙后立刻停掉;用 tokio::time::pause 或注入 offlines_secs 配置,断言 15s 后从甲列表消失
}
```
(第三个测试:给 `DiscoveryConfig` 加 `pub offline_secs: u64`(默认 15,测试设 1)使测试可等待真实时间 1.5s,不引入时钟 mock。)

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core discovery`

- [ ] **Step 3: 实现**:`tokio::net::UdpSocket::bind(("0.0.0.0", bind_port))`,`set_broadcast(true)`;单任务 `select!` 三事件:接收循环 / 5s interval / cmd;接收→`verify_and_parse(自维护 ReplayGuard, now = SystemTime)`;缓存 `HashMap<fp, DeviceInfo>` 每次变更后发布 watch 快照(按 name 排序)。

- [ ] **Step 4: 全绿**(注意并发测试端口固定互不冲突;必要时 `--test-threads=1` 或每测试随机端口)

Run: `cargo test -p landrop-core discovery`
- [ ] **Step 5: Commit** `git commit -m "feat(core): UDP 发现服务——广播/探测/隐身/缓存过期"`

---

### Task 8: share — 共享区注册与路径安全

**Files:**
- Create: `crates/landrop-core/src/share.rs`;Modify: `lib.rs`

**Interfaces:**
- Consumes: Task 2 `ShareDef`;Task 5 `FileEntry`
- Produces:
  - `pub struct ShareRegistry { shares: Vec<ShareDef> }`
  - `impl ShareRegistry { pub fn new(shares: Vec<ShareDef>) -> Self; pub fn list(&self) -> &[ShareDef]; pub fn resolve(&self, share_id: &str, rel: &str) -> Result<PathBuf, ShareError>; pub async fn list_dir(&self, share_id: &str, rel: &str, cursor: u64, limit: usize) -> Result<(Vec<FileEntry>, Option<u64>), ShareError>; pub fn share_id_of_alias(&self, alias: &str) -> Option<&str>; }`
  - `ShareError::{UnknownShare, IllegalPath, NotFound}`(Display 中文)
  - `resolve` 规则:拒绝含 `..` 组件/绝对路径/Windows 盘符前缀(`rel` 含 `:`);`symlink_metadata` 拒绝重解析点;canonicalize 后 `starts_with(canonical_root)` 二次校验
  - `list_dir`:读目录项(跳过隐藏/系统属性项),按名字排序,`cursor` 为偏移,`limit` 上限 500,返回 `next_cursor`

- [ ] **Step 1: 写失败测试**

```rust
fn reg(tmp: &Path) -> ShareRegistry {
    let root = tmp.join("share"); std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), b"hi").unwrap();
    ShareRegistry::new(vec![ShareDef { id: "s1".into(), alias: "分享".into(), path: root }])
}

#[test]
fn resolve_accepts_normal_and_rejects_traversal() {
    let tmp = tempdir().unwrap();
    let r = reg(tmp.path());
    assert!(r.resolve("s1", "sub").is_ok());
    assert!(matches!(r.resolve("s1", ".."), Err(ShareError::IllegalPath)));
    assert!(matches!(r.resolve("s1", "a/../../b"), Err(ShareError::IllegalPath)));
    assert!(matches!(r.resolve("s1", "C:/Windows"), Err(ShareError::IllegalPath)));
    assert!(matches!(r.resolve("s9", "x"), Err(ShareError::UnknownShare)));
}

#[test]
#[cfg(windows)]
fn junction_escape_rejected() {
    // std::process::Command::new("cmd").args(["/c","mklink","/J", link, "C:\\Windows"])
    // 链接放在共享区内,resolve(link) 必须 IllegalPath
}

#[test]
fn list_dir_paginates_sorted() {
    // 建 3 文件,limit=2 → 第一次 2 项+next_cursor=Some(2),第二次从 cursor=2 取剩 1 项+None
    // 每项 name/is_dir/size 正确
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core share`

- [ ] **Step 3: 实现**(路径检查用 `std::path::Component` 迭代拒绝 `ParentDir`;`list_dir` 用 `tokio::fs::read_dir`;mtime 用 `metadata.modified().duration_since(UNIX_EPOCH)`)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core share`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 共享区注册/路径安全/分页列表"`

---

### Task 9: transfer/manifest — 分块清单与对账

**Files:**
- Create: `crates/landrop-core/src/transfer/mod.rs`(先 `pub mod manifest;`)、`transfer/manifest.rs`;Modify: `lib.rs`(`pub mod transfer;`)

**Interfaces:**
- Consumes: Task 5 `CHUNK_SIZE`
- Produces:
  - `pub struct Manifest { pub file_name: String, pub total_size: u64, pub chunk_hashes: Vec<String>, /*hex*/ pub received: Vec<bool> }`(chunk_size 固定 CHUNK_SIZE 不序列化;serde 直接派生)
  - `impl Manifest { pub fn build(path: &Path) -> Result<Self, ManifestError>; pub fn chunk_count(&self) -> u32; pub fn missing_chunks(&self) -> Vec<u32>; pub fn chunk_offset(&self, i: u32) -> u64; pub fn chunk_len(&self, i: u32) -> u64; pub fn save(&self, dir: &Path) -> io::Result<()>; pub fn load(dir: &Path) -> Result<Self, ManifestError>; }`
  - 存储位置:`<下载目录>\.landrop-parts\<job_id:016x>\manifest.json`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn build_hashes_and_geometry() {
    let tmp = tempdir().unwrap();
    let p = tmp.path().join("f.bin");
    std::fs::write(&p, vec![7u8; CHUNK_SIZE as usize * 2 + 100]).unwrap();
    let m = Manifest::build(&p).unwrap();
    assert_eq!(m.chunk_count(), 3);
    assert_eq!(m.chunk_len(0), CHUNK_SIZE as u64);
    assert_eq!(m.chunk_len(2), 100);
    assert_eq!(m.chunk_offset(2), 2 * CHUNK_SIZE as u64);
    assert_eq!(m.missing_chunks(), vec![0, 1, 2]);
    // 空/单块文件几何
    let p2 = tmp.path().join("empty.bin"); std::fs::write(&p2, b"").unwrap();
    assert_eq!(Manifest::build(&p2).unwrap().chunk_count(), 0);
}

#[test]
fn save_load_roundtrip_with_partial_bitmap() {
    // m.received[1]=true;save→load 后 received 一致,chunk_hashes 一致
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core manifest`

- [ ] **Step 3: 实现**(`build` 用 `std::fs::File` + `seek`/`read_exact` 逐块读,`Sha256::digest` 转 hex;大文件顺序读不并发——构建期吞吐非瓶颈)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core manifest`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 传输清单——分块哈希/位图/持久化"`

---

### Task 10: transfer/engine(接收侧)— PartWriter 乱序落盘

**Files:**
- Create: `crates/landrop-core/src/transfer/engine.rs`;Modify: `transfer/mod.rs`

**Interfaces:**
- Consumes: Task 9 `Manifest`
- Produces:
  - `pub struct PartWriter { part_path: PathBuf, file: std::fs::File, pub manifest: Manifest }`
  - `impl PartWriter { pub fn open(dir: &Path, m: Manifest) -> Result<Self, EngineError>; pub fn write_chunk(&mut self, idx: u32, data: &[u8]) -> Result<(), EngineError>; pub fn finalize(self, dest_dir: &Path) -> Result<PathBuf, EngineError>; }`
  - `write_chunk`:校验 `sha256(data)==manifest.chunk_hashes[idx]`(失败 `EngineError::HashMismatch{chunk}`)、长度==chunk_len;用 `std::os::windows::fs::FileExt::seek_write` 写到 `chunk_offset(idx)`;成功后 `received[idx]=true` 并节流落盘 manifest(每块或每秒)
  - `finalize`:全部 received → 若目标存在命名 `名字 (1).ext`;`rename` part→目标;删除任务目录;返回最终路径

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn out_of_order_write_then_finalize_matches_source() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    let data: Vec<u8> = (0..CHUNK_SIZE as u32 * 2 + 50).map(|i| i as u8).collect();
    std::fs::write(&src, &data).unwrap();
    let m = Manifest::build(&src).unwrap();
    let parts = tmp.path().join(".landrop-parts").join("0000000000000001");
    std::fs::create_dir_all(&parts).unwrap();
    let mut w = PartWriter::open(&parts, m).unwrap();
    let c1 = read_chunk(&src, 1); let c0 = read_chunk(&src, 0); let c2 = read_chunk(&src, 2);
    w.write_chunk(1, &c1).unwrap();   // 乱序:先 1
    w.write_chunk(0, &c0).unwrap();
    w.write_chunk(2, &c2).unwrap();
    let dest = w.finalize(tmp.path().join("out")).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!parts.exists(), "任务目录应清理");
}

#[test]
fn corrupted_chunk_rejected() {
    // write_chunk(0, 篡改数据) → Err(HashMismatch{chunk:0}),received[0] 仍 false,可重写成功
}

#[test]
fn finalize_before_complete_rejected() {
    // 只写 1/3 块就 finalize → Err(EngineError::Incomplete)
}
```
(`read_chunk` 为测试辅助:`File::seek+read_exact_exact` 取第 i 块。)

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core engine`

- [ ] **Step 3: 实现**(manifest 节流落盘:记录 `last_save: Instant`,距上次 >1s 或全部完成才写盘;`finalize` 前强制写盘一次)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core engine`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 接收引擎——乱序落盘/哈希校验/收尾改名"`

---

### Task 11: session — QUIC endpoint 与证书锁定

**Files:**
- Create: `crates/landrop-core/src/session.rs`(本任务:endpoint 构造 + 自定义验证器);Modify: `lib.rs`

**Interfaces:**
- Consumes: Task 3 `Identity/Fingerprint/fingerprint_of`
- Produces:
  - `pub fn server_config(id: &Identity) -> Result<quinn::ServerConfig, SetupError>` —— rustls `ServerConfig`,**客户端证书验证器 = 接受任何自签**(捕获证书 DER 供应用层校验)
  - `pub fn client_config(expected_peer: Option<Fingerprint>) -> quinn::ClientConfig` —— 自定义 `ServerCertVerifier`:`expected_peer=Some(fp)` → 证书指纹必须相等,否则握手失败;`None` → 接受任何自签(用于首连,指纹随后在配对流程核对)
  - `pub async fn connect(ep: &quinn::Endpoint, addr: SocketAddr, id: &Identity, expected: Option<Fingerprint>) -> Result<(quinn::Connection, Fingerprint), SessionError>` —— 返回连接 + 对端指纹(从 peer_identity 证书算)
  - `pub fn bind_endpoint(port: u16, id: &Identity) -> Result<quinn::Endpoint, SetupError>`
  - `pub struct SessionError::{Connect(quinn::ConnectError), Connection(quinn::ConnectionError), NoPeerCert, Setup(SetupError)}`(Display 中文)
  - 两个自定义 verifier 都实现 `rustls::client::danger::ServerCertVerifier` / `rustls::server::danger::ClientCertVerifier` 的必需方法:`verify_server_cert`/`verify_client_cert` 只查 `end_entity` 自签一致性(比对 cert 内签名可被该证书公钥验证),`verify_tls12_signature` 返回 `Ok(unsupported)` 之外的拒绝、`verify_tls13_signature` 接受 rustls 默认方案,`server_intermediates`/`client_intermediates` 返回 `&[]`,`auth_scheme = Certificate`。TLS1.2 offer 关闭(builder `with_protocol_versions(&[&rustls::version::TLS13])`)。

- [ ] **Step 1: 写失败测试**(回环双 endpoint)

```rust
#[tokio::test]
async fn loopback_connect_yields_peer_fingerprints() {
    let a = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
    let b = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
    let ep_a = bind_endpoint(0, &a).unwrap();           // port 0 = 随机
    let ep_b = bind_endpoint(0, &b).unwrap();
    let b_local = ep_b.local_addr().unwrap();
    tokio::spawn({ let ep_b = ep_b.clone(); async move {
        if let Some(incoming) = ep_b.accept().await {
            let _ = incoming.await.unwrap();            // server 接受
        }
    }});
    let (conn, peer_fp) = connect(&ep_a, b_local, &a, None).await.unwrap();
    assert_eq!(peer_fp, b.fingerprint());
    // b 侧可从 conn.peer_identity() 拿到 a 的证书 → 指纹 == a.fingerprint()
}

#[tokio::test]
async fn wrong_expected_fingerprint_fails_handshake() {
    // connect(..., Some([9u8;32])) → Err(SessionError::Connect(_))
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core session`

- [ ] **Step 3: 实现**(quinn `Endpoint::client/config with `Arc<rustls::ClientConfig>`;server 侧 `with_client_cert_verifier(Arc<AnySelfSignedClientVerifier>)`;`connect` 后 `conn.peer_identity()` → 第一个证书 → `fingerprint_of`)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core session`

- [ ] **Step 5: Commit** `git commit -m "feat(core): QUIC endpoint 与自签证书锁定握手"`

---

### Task 12: pairing — SAS 双向验证码派生

**Files:**
- Create: `crates/landrop-core/src/pairing.rs`;Modify: `lib.rs`

**Interfaces:**
- Consumes: Task 11 连接
- Produces:
  - `pub fn derive_sas_code(conn: &quinn::Connection, local_fp: &Fingerprint, remote_fp: &Fingerprint) -> Result<String, PairingError>`
  - 规则:label = `b"lan-drop-sas-v1"`;info = 两个指纹**排序后拼接**(min,max → 32+32 字节,顺序无关,双方一致);`conn.with_crypto(|cc| cc.export_keying_material(&mut [0u8;32], label, Some(&info)))`;再 `HKDF-SHA256(ikm=exporter, info=b"sas")` 取 4 字节 → `u32 % 1_000_000` → 零填充 6 位十进制字符串
  - `pub struct PairingMachine { own_code: String, submitted_ok: bool, fails: u8 }`
  - `impl PairingMachine { pub fn new(own_code: String) -> Self; pub fn submit_remote(&mut self, remote_code: &str) -> bool; /* remote_code == own_code → true 并 submitted_ok=true;否则 fails+=1 */ pub fn is_complete(&self) -> bool { self.submitted_ok } pub fn failed_out(&self) -> bool { self.fails >= 3 } }`
  - 冷却常量 `pub const COOLDOWN_SECS: u64 = 300;`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn both_ends_derive_same_code() {
    // 复用 Task 11 回环建立 a↔b 一条连接(server 端保存 conn)
    let code_a = derive_sas_code(&conn_a, &a.fingerprint(), &b.fingerprint()).unwrap();
    let code_b = derive_sas_code(&conn_b, &b.fingerprint(), &a.fingerprint()).unwrap();
    assert_eq!(code_a, code_b);
    assert_eq!(code_a.len(), 6);
    assert!(code_a.chars().all(|c| c.is_ascii_digit()));
}

#[tokio::test]
async fn different_session_different_code() {
    // 两条不同连接派生码不同(概率 1e-6,固定种子身份+两条连接)
}

#[test]
fn pairing_machine_logic() {
    let mut m = PairingMachine::new("123456".into());
    assert!(!m.submit_remote("000000"));
    assert!(!m.submit_remote("111111"));
    assert!(!m.submit_remote("222222"));
    assert!(m.failed_out());                     // 3 次失败
    let mut m2 = PairingMachine::new("654321".into());
    assert!(m2.submit_remote("654321"));
    assert!(m2.is_complete());
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core pairing`

- [ ] **Step 3: 实现**(`export_keying_material` 通过 `quinn::Connection::with_crypto` 访问 `rustls::ConnectionCommon`)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core pairing`

- [ ] **Step 5: Commit** `git commit -m "feat(core): SAS 验证码派生与配对状态机"`

---

### Task 13: session — 会话管理器与配对全流程

**Files:**
- Modify: `crates/landrop-core/src/session.rs`(追加)

**Interfaces:**
- Consumes: Task 11 connect/accept、Task 12 PairingMachine、Task 4 TrustStore、Task 5 protocol(Hello/PairCodeSubmit/PairResult)
- Produces:
  - `pub struct SessionCtx { pub identity: Arc<Identity>, pub trust: Arc<Mutex<TrustStore>>, pub config: Arc<RwLock<Config>> }`
  - `pub enum SessionEvent { PairingRequested { fingerprint: Fingerprint, addr: SocketAddr, own_code: String }, PairingResult { fingerprint: Fingerprint, ok: bool, reason: Option<String> }, SessionUp { fingerprint: Fingerprint, name: String, conn: quinn::Connection }, SessionDown { fingerprint: Fingerprint } }`
  - `pub struct SessionManager::spawn(ctx: SessionCtx) -> (Arc<Self>, mpsc::Receiver<SessionEvent>)`
  - `pub async fn connect(&self, addr: SocketAddr) -> Result<Fingerprint, SessionError>` —— 主动连接:建连→对端指纹→trusted? `SessionUp`+Hello : `PairingRequested` 事件(等待 UI 双向输码)
  - `pub async fn submit_pair_code(&self, fingerprint: &Fingerprint, peer_code: &str) -> Result<bool, SessionError>` —— 本地校验 peer_code==own_code;同时把本地用户输入经控制流 `PairCodeSubmit` 发给对方
  - 内部:每会话首双向流 = 控制流;`PairCodeSubmit` 到达 → 对端 PairingMachine.submit_remote → 双方 `submitted_ok` → 互发 `PairResult{ok:true}` → 双方 upsert TrustStore → `SessionUp`;任一方 failed_out → 关连接 + 冷却该指纹 300s(`cooldown: HashMap<Fingerprint, Instant>`,connect 前检查)
  - `pub fn session(&self, fingerprint) -> Option<quinn::Connection>`;`pub async fn disconnect(&self, fingerprint)`
  - 控制流断开 → `SessionDown`,连接对象移除(断点续传由 Task 15 处理)

- [ ] **Step 1: 写失败测试**(回环全流程,不起 UI)

```rust
#[tokio::test]
async fn full_pairing_flow_both_sides_trusted_after() {
    let (ctx_a, mut ev_a) = setup_ctx("甲");   // 辅助:临时目录 identity+trust+config,随机端口
    let (ctx_b, mut ev_b) = setup_ctx("乙");
    let b_addr = start_listener(ctx_b.clone());  // spawn accept 循环
    let fp_b = ctx_a.sm.connect(b_addr).await.unwrap_err_fingerprint_or_event(); // 触发 PairingRequested
    // a 侧事件拿到 own_code_a;b 侧事件拿到 own_code_b
    // 互相提交对方码(模拟用户看到对方屏幕)
    let ok_a = ctx_a.sm.submit_pair_code(&fp_b, &own_code_b).await.unwrap();
    let ok_b = ctx_b.sm.submit_pair_code(&fp_a, &own_code_a).await.unwrap();
    assert!(ok_a && ok_b);
    // 双方 SessionUp;双方 TrustStore 里互相存在
    assert!(ctx_a.trust.lock().await.is_trusted(&fp_b));
    assert!(ctx_b.trust.lock().await.is_trusted(&fp_a));
}

#[tokio::test]
async fn trusted_reconnect_is_silent() {
    // 预置互信;connect → 直接 SessionUp,无 PairingRequested
}

#[tokio::test]
async fn three_wrong_codes_cooldown() {
    // 连错 3 次 → PairingResult{ok:false};立刻再 connect → Err(含冷却信息)
}
```
(`setup_ctx`/`start_listener` 写成 `#[cfg(test)]` 公共辅助,放 `session.rs` 测试模块;随机端口用 `bind_endpoint(0)` 取 `local_addr()`。)

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core session`

- [ ] **Step 3: 实现**(会话表 `HashMap<Fingerprint, Session>`;`Session { conn, ctrl_tx: mpsc::Sender<ControlMsg>, pairing: Option<PairingMachine> }`;accept 循环与 connect 共用 `handshake()` 内部函数;所有事件经 mpsc 发出)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 会话管理器与双向 SAS 配对全流程"`

---

### Task 14: transfer — 发送引擎、控制 RPC 路由与缓冲池

**Files:**
- Modify: `crates/landrop-core/src/transfer/engine.rs`(追加)、`transfer/mod.rs`(调度器骨架)

**Interfaces:**
- Consumes: Task 9-10 Manifest/PartWriter、Task 5 协议、Task 8 ShareRegistry、Task 13 SessionManager
- Produces:
  - `pub struct BufferPool { sem: Arc<Semaphore> }` —— 32 份许可,`pub async fn acquire(&self) -> BytesMut`(4MiB),`pub fn release(BytesMut)`(内部回收复用)
  - `pub struct SendJob { pub job_id: u64, pub src_path: PathBuf, pub dest_hint: Option<(String,String)> /*(share_id,rel) 仅拉取模式*/ }`
  - `pub async fn run_sender(conn: quinn::Connection, ctrl: mpsc::Sender<ControlMsg>, job: SendJob, pool: Arc<BufferPool>, streams: Arc<AdaptiveStreams>, progress: mpsc::Sender<ProgressEvent>) -> Result<CompletedInfo, EngineError>`
    - 拉取模式接收方驱动:对端发 `MetaResp`+`FetchReq{chunk}` → 我方为每个 FetchReq 开一条单向流:写 `ChunkStreamHeader` + 数据 + fin
  - `pub enum ProgressEvent { Started { job_id, name, total }, ChunkDone { job_id, chunk, bytes }, Speed { job_id, bps }, Done { job_id }, Failed { job_id, reason } }`
  - `pub fn spawn_rpc_router(ctx: SessionCtx, reg: Arc<ShareRegistry>, ctrl_rx: mpsc::Receiver<(Fingerprint, ControlMsg)>)` —— 会话控制流入站消息分发:SharesReq/ListReq(查 perms.browse)/MetaReq(Manifest::build 现算)/FetchReq(查 perms.download)/OfferReq(查 perms.push:Ask→UI 事件,Auto→自动 Accept,Deny→拒)
  - 接收侧 `pub async fn run_receiver(conn, ctrl, job_id, manifest, parts_dir, download_dir, progress) -> Result<PathBuf, EngineError>` —— 按缺失块发 FetchReq,收块流→PartWriter
  - 小文件批流:`pub const SMALL_FILE_LIMIT: u64 = 1024*1024;` 多文件任务里 ≤1MiB 的文件走单条双向流逐个直传(文件名定长头+数据),不切块、不建 part

- [ ] **Step 1: 写失败测试**(回环拉取一个 10MB 文件)

```rust
#[tokio::test]
async fn pull_file_over_loopback() {
    // 复用 Task 13 setup:甲乙互信;乙共享区放 10MB 随机文件
    // 甲侧调 start_pull(fp_b, share_id, rel) —— 由 rpc/调度器暴露的入口函数
    // 等 Done 事件;比对下载文件 sha256 与源一致
}
```
(`start_pull` 本任务先作为 `transfer::mod.rs` 的 `pub async fn start_pull(sm: &SessionManager, reg: &ShareRegistry, peer: &Fingerprint, share_id: &str, rel: &str, cfg: &Config, progress: mpsc::Sender<ProgressEvent>) -> Result<u64, EngineError>` —— 协商 job_id(原子计数器)、MetaReq/MetaResp、本地 PartWriter、逐块 FetchReq。)

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core pull`

- [ ] **Step 3: 实现**(发送侧块流:`conn.open_uni` → `write_all(header)` → `write_all(data)` → `finish()`;接收侧 `accept_uni`;窗口:每流一次一个 FetchReq,同时 in-flight 块数 = streams.current();进度事件每块发)

- [ ] **Step 4: 全绿** → `cargo test -p landrop-core`

- [ ] **Step 5: Commit** `git commit -m "feat(core): 发送/接收引擎与控制面 RPC(拉取全流程)"`

---

### Task 15: transfer — 自适应流数 + 断点续传 + 小文件批 + 推送

**Files:**
- Create: `crates/landrop-core/src/transfer/adapt.rs`;Modify: `transfer/{mod.rs,engine.rs}`

**Interfaces:**
- Consumes: Task 14 全部
- Produces:
  - `pub struct AdaptiveStreams { current: usize, last_bps: u64 }`(start=4,MAX=16)
  - `impl AdaptiveStreams { pub fn on_probe(&mut self, bps_now: u64, loss_ratio: f64) -> usize; }`
    规则:`loss_ratio > 0.05 → current = max(2, current-2)`;`bps_now > last_bps*105/100 → current+1(≤16)`;否则不变;返回新值。探测周期 500ms,bps 来源:两次 ProgressEvent::Speed 差;loss 来源:`conn.stats().path.loss_packets as f64 / sent as f64`(quinn `Connection::stats()`)
  - 断点续传:`start_pull` 开始时先发 `BitmapReq{job_id}`,对端回 `BitmapResp`(接收方已存清单);无存档 → 新建;有 → 只对缺失块 FetchReq。**本机重启恢复**:`transfer::mod.rs` 维护 `pub fn pending_jobs(parts_root: &Path) -> Vec<(u64, Manifest)>`(扫描 `.landrop-parts/*/manifest.json`),UI 启动时询问后逐个重连续传
  - 推送:`pub async fn push_files(sm, peer, files: Vec<PathBuf>, save_dir_hint: Option<PathBuf>, progress) -> Result<u64, EngineError>` —— 发 `OfferReq{job_id, files}`;对端 Ask → UI 事件 → `OfferResp{accepted, save_dir}`;accepted 后发送方把每个文件作为 SendJob 逐个传(≤1MiB 走批流;>1MiB 切块,接收端先 MetaResp 协商)——接收端反向实现(发送方主动推 Manifest,接收方按拉取逻辑驱动,复用同一引擎)
  - `pub enum TransferAction2 { Pause, Resume, Cancel }` 处理:Pause=停止发新 FetchReq(挂起信号量),Cancel=删任务目录+中止

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn adapt_rules() {
    let mut a = AdaptiveStreams::default();
    assert_eq!(a.current, 4);
    a.on_probe(100, 0.0); assert_eq!(a.current, 5);      // 增长>5% → +1
    let cur = a.on_probe(101, 0.0); assert_eq!(cur, 5);  // 增长<5% → 保持
    a.on_probe(101, 0.10); assert_eq!(a.current, 3);     // 丢包10% → -2
    for _ in 0..20 { a.on_probe(1_000_000, 0.0); }
    assert_eq!(a.current, 16);                            // 封顶
}

#[tokio::test]
async fn resume_after_connection_drop() {
    // 甲拉乙 30MB;传到 ~50%(收 N 块后 drop conn)→ 断言任务冻结
    // 重连(互信,静默)→ 续传 → Done → 全文件哈希一致
    // 且统计:第二次会话乙侧发送的块数 == missing 数(可用计数 channel 验证无重传)
}

#[tokio::test]
async fn small_files_batched() {
    // 100 个 100KB 文件推送;断言总耗时内控制流消息数 = 1(OfferReq)+数据流 ≤ 8 条
    // (记录 open_uni 次数,通过 progress 事件的 started 计数近似)
}

#[tokio::test]
async fn push_offer_flow_with_ask_policy() {
    // 乙对甲 push=Ask:甲发 OfferReq → 甲侧收到 UI 事件(offer-request)
    // 甲 respond(accept, save_dir=Some(tmp)) → 传输完成落盘正确
    // respond(reject) → EngineError::OfferRejected,无文件写入
}
```

- [ ] **Step 2: 确认失败** → `cargo test -p landrop-core`

- [ ] **Step 3: 实现**

- [ ] **Step 4: 全绿**(本任务是阶段一核心收口,跑全 crate 测试)

Run: `cargo test -p landrop-core`
Expected: all passed(用例数较 Task 14 增)

- [ ] **Step 5: Commit** `git commit -m "feat(core): 自适应流数/断点续传/小文件批流/推送流程"`

---

### Task 16: src-tauri — 壳、commands、事件与单实例

**Files:**
- Create: `src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、`src-tauri/src/{main.rs,commands.rs,events.rs,firewall.rs}`、`src-tauri/icons/icon.ico`;Modify: 根 `Cargo.toml`(members 启用 src-tauri)、`src-tauri/build.rs`

**Interfaces:**
- Consumes: landrop-core 全部公共接口
- Produces(Tauri command 名,前端 Task 17+ 依赖):
  - 设备:`list_devices() -> Vec<DeviceDto{id,name,fingerprint,addr,online}>`、`set_hidden(hidden: bool)`、`probe_now()`、`add_manual_device(addr: String)`、`connect(fingerprint: String)`(按指纹找 addr)
  - 配对:`get_pairing_pending() -> Vec<PairingDto{fingerprint,name,own_code}>`、`submit_pair_code(fingerprint: String, peer_code: String) -> bool`、`reject_pairing(fingerprint: String)`
  - 浏览/传输:`list_shares_remote(fingerprint) -> Vec<ShareInfo>`、`list_dir_remote(fingerprint, share_id, path, cursor) -> ListResp`、`start_download(fingerprint, share_id, path) -> u64`、`push_files(fingerprint, local_paths: Vec<String>) -> u64`、`respond_offer(job_id: u64, accepted: bool, save_dir: Option<String>)`、`transfer_action(job_id: u64, action: String)`
  - 队列:`list_transfers() -> Vec<TransferDto{job_id,name,total,done,state,speed_bps,peer}>`、`pending_resume_jobs() -> Vec<(u64,String)>`、`resume_pending(job_id)`
  - 设置:`get_settings() -> ConfigDto`、`save_settings(config: ConfigDto)`、`add_share(alias, path) -> ShareDef`、`remove_share(id)`、`list_trusted() -> Vec<TrustedPeerDto>`、`set_perms(fingerprint, browse, download, push)`、`remove_trusted(fingerprint)`
  - 系统:`add_firewall_rule() -> Result<String>`、`get_device_fingerprint() -> {fingerprint_hex, short_code, name}`
- 事件(Rust→UI):`device-list`、`pairing-request{fingerprint,name,own_code}`、`pairing-result{fingerprint,ok,reason}`、`offer-request{job_id,peer,files}`、`transfer-progress{jobs:[...]}`(**4Hz 节流**,events.rs 用 tokio interval 聚合批量发)、`connection-state{fingerprint,up:bool}`、`toast{level,text}`
- `main.rs`:初始化 tracing→data_dir→Identity/Config/TrustStore→discovery.spawn→SessionManager::spawn→rpc router→单实例锁(`tauri-plugin-single-instance`,第二实例退出并让首实例置前)→WebView2 检测(注册表 `reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv`,失败弹系统对话框给下载链接后退出)
- `firewall.rs`:`powershell -Command "Start-Process netsh -ArgumentList 'advfirewall firewall add rule name=\"lan-drop\" dir=in action=allow protocol=UDP localport=47600-47601' -Verb RunAs"`(触发 UAC;返回成功/用户取消文案)
- `tauri.conf.json` 关键:`"identifier": "com.landrop.app"`、`"bundle": {"active": false, ...}`、窗口 1000×680 标题"lan-drop 局域网互传"、`"app": {"security": {"csp": null}}`(本地资产)、frontendDistEmbed 由 Tauri 构建注入

- [ ] **Step 1: 脚手架编译通过**(先 `cargo check -p landrop-app`;crate 名 `landrop-app`,`lib = false`;deps:tauri 2 + plugins single-instance/dialog/notification/opener、landrop-core path 依赖、tokio、serde、serde_json;`build.rs` = `tauri_build::build()`;无 UI 前端时 `frontendDist` 指向空 `../ui/dist` 占位目录 —— 本任务先 `mkdir ui/dist` 放一个 `index.html` 占位)

- [ ] **Step 2: 集成自测**——写 `#[tokio::test]` 不适用(Tauri 依赖窗口),改为:`cargo run -p landrop-app --no-default-features` 不可行;用**手动冒烟**:`cargo build -p landrop-app` 成功 + `main.rs` 里提供 `#[cfg(feature="headless-test")] fn smoke()` 在 data 目录生成 identity/config(调用 core),`cargo test -p landrop-app` 验证核心装配(identity 创建、config 读写、firewall 命令字符串构造正确性 —— 不真正执行 netsh)

```rust
// src-tauri/src/main.rs 关键装配(节选)
fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let dir = landrop_core::store::data_dir().expect("数据目录初始化失败:请确认 exe 目录可写");
    let identity = Arc::new(Identity::load_or_create(&dir).expect("身份初始化失败"));
    let config = Arc::new(RwLock::new(load_config(&dir)));
    let trust = Arc::new(Mutex::new(TrustStore::load(&dir)));
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") { let _ = w.set_focus(); }
        }))
        .manage(AppState { dir, identity, config, trust, /* sm, discovery, transfers… */ })
        .invoke_handler(tauri::generate_handler![ /* 上述全部 command */ ])
        .run(tauri::generate_context!())
        .expect("lan-drop 启动失败");
}
```

- [ ] **Step 3: 全部 command 实现并 `cargo check` 通过**(命令体均为对 core 对应接口的薄封装;状态放 `struct AppState(Mutex 内部各组件句柄>)`,用 `State<AppState>` 注入)

- [ ] **Step 4: 冒烟构建** `cargo build -p landrop-app` → 成功;`cargo test -p landrop-app` → 绿

- [ ] **Step 5: Commit** `git commit -m "feat(app): Tauri 壳——commands/事件节流/单实例/防火墙放行"`

---

### Task 17: ui — Vue 脚手架、api 封装与 stores

**Files:**
- Create: `ui/package.json`、`ui/vite.config.ts`、`ui/tsconfig.json`、`ui/index.html`、`ui/src/{main.ts,App.vue,api.ts}`、`ui/src/stores/{devices,transfers,settings}.ts`、`ui/src/pages/{Devices,Transfers,Browse,Settings}.vue`(先占位空页)、`ui/src/components/{DeviceCard,TransferItem,PairingDialog,CodeBadge}.vue`(占位)

**Interfaces:**
- Consumes: Task 16 的 command 名与事件名(严格一致)
- Produces:
  - `ui/src/api.ts`:`export const api = { devices: () => invoke<DeviceDto[]>('list_devices'), … }`(每个 command 一个方法,类型 `types.ts` 集中定义 Dto) + `export function onEvent<T>(name: string, cb: (e: T) => void)`(封装 `listen`)
  - Pinia stores:`devices`(设备列表 + pairing pending 列表,订阅 `device-list`/`pairing-request`)、`transfers`(任务列表,订阅 `transfer-progress`,4Hz 批量更新防抖写入)、`settings`(config/trusted/relay 预留,加载于启动)
  - 路由:vue-router 4 页 tab(设备/传输/浏览/设置);App.vue 底部 tab 栏 + toast 容器

- [ ] **Step 1: 脚手架**(`npm create vue@latest` 最小模板或手写;deps:`vue@^3.4, vue-router@^4, pinia@^2, @tauri-apps/api@^2`;vite config 加 `server: { port: 1420, strictPort: true }`,`clearScreen: false`,`envPrefix: ['VITE_','TAURI_']`,build target chrome105)

- [ ] **Step 2: 类型与 api.ts 全量命令封装**(每个 Task 16 command 都要有对应方法与 Dto 类型;`types.ts` 字段名与 Rust serde 输出一致 —— Rust 默认蛇形,前端统一 `camelCase: true` 不开,**Dto 直接用蛇形字段**避免配置漂移)

- [ ] **Step 3: stores 订阅事件**(devices/transfers/settings 分别 mount 时 `onEvent`;`transfer-progress` 事件负载已是数组,直接整表替换 + Vue 响应式 diff)

- [ ] **Step 4: 构建验证** `npm run build` → 成功;`cargo build -p landrop-app`(前端 dist 已接上)→ exe 可启动出窗口(四个空 tab)

- [ ] **Step 5: Commit** `git commit -m "feat(ui): Vue 脚手架/api 封装/pinia stores/事件订阅"`

---

### Task 18: ui — 设备页与配对弹窗

**Files:**
- Modify: `ui/src/pages/Devices.vue`、`ui/src/components/{DeviceCard,PairingDialog,CodeBadge}.vue`

**Interfaces:**
- Consumes: `api.ts`、stores
- Produces: 完整设备页交互(其他页面不改):

```
┌ 设备页 ────────────────────────────────┐
│ [隐身开关] [手动+ IP:端口] [刷新=probe_now] │
│ ┌DeviceCard────────────┐ ┌DeviceCard──┐ │
│ │ 同事电脑  A3F2-91BC  │ │ …          │ │
│ │ 已配对● / 待配对○    │ │            │ │
│ │ [连接] [浏览→] [⋮权限]│ │            │ │
│ └──────────────────────┘ └────────────┘ │
└────────────────────────────────────────┘
PairingDialog:大字显示 own_code(CodeBadge)|
  输入对方 6 位码 [确认] [拒绝] | 双向提示文案
```

- [ ] **Step 1: DeviceCard**:props=DeviceDto;`[连接]`→`api.connect` 后跳浏览页;`[⋮]` 下拉:权限三开关(browse/download/push=每次询问|自动|拒绝,调 `set_perms`)、移除信任(`remove_trusted`,二次确认);拖拽目标:`@dragover.prevent @drop` 拿文件路径数组 → `push_files`(本任务先 console.log + 调 api,弹窗在 Task 19 offer-request 事件处理)
- [ ] **Step 2: PairingDialog**:监听 `pairing-request` 弹模态;展示 `own_code`(CodeBadge 放大字)+ 6 位输入框(仅数字,6 位后可提交)→ `submit_pair_code`;`pairing-result` 事件 → 成功 toast"配对成功"/失败显示原因(`reason` 字段);拒绝按钮 → `reject_pairing`
- [ ] **Step 3: 隐身开关/手动添加**:开关→`set_hidden`(本地立即反馈+失败回滚);手动添加→输入 `IP:端口`→`add_manual_device`→列表出现(设备来自 discovery 单播回应)
- [ ] **Step 4: 手动验证**:`npm run tauri dev` 起两份(改第二个实例端口参数或两台真机)→ 能互相看到、连接、配对全流程可走通(依赖 Task 13-16 已就绪)
- [ ] **Step 5: Commit** `git commit -m "feat(ui): 设备页——卡片/配对弹窗/隐身/手动添加/拖拽投放"`

---

### Task 19: ui — 传输页、浏览页与推送确认

**Files:**
- Modify: `ui/src/pages/{Transfers,Browse}.vue`、`ui/src/components/TransferItem.vue`

**Interfaces:**
- Consumes: stores.transfers、`offer-request` 事件、api

- [ ] **Step 1: Transfers 页**:任务表(名称/对方/进度条/实时速度/状态);操作按钮按状态:进行中→暂停/取消,暂停→继续/取消,失败→重试(清 manifest 后重新 `start_download`/`push_files`),完成→"打开所在文件夹"(tauri opener 插件);启动时 `pending_resume_jobs` 有存档 → 顶部横幅"发现 N 个未完成任务 [全部续传] [忽略]"
- [ ] **Step 2: Browse 页**:左栏对方共享区列表(`list_shares_remote`)→ 目录树(懒加载 `list_dir_remote`,cursor 翻页"加载更多")→ 右栏文件多选 + `[下载选中]`(`start_download` 逐个);面包屑导航;路径变化自动刷新按钮
- [ ] **Step 3: 推送确认弹窗**(App.vue 全局):监听 `offer-request` → 模态列出文件名/大小/来源设备 → [接收](默认下载目录)/[另存到…](tauri dialog 选目录)/[拒绝] → `respond_offer`
- [ ] **Step 4: 手动验证**:双实例互拉 10MB+文件、互推文件夹(含小文件批量)、中途断开再续、暂停/恢复/取消
- [ ] **Step 5: Commit** `git commit -m "feat(ui): 传输队列/远程浏览/推送确认"`

---

### Task 20: ui — 设置页;便携打包、冒烟与吞吐基准

**Files:**
- Modify: `ui/src/pages/Settings.vue`;Create: `README.md`、`crates/landrop-core/examples/bench_loopback.rs`

**Interfaces:**
- Consumes: 全部
- Produces:
  - 设置页四卡片:**共享区**(增删:别名+选目录;来自 `get_settings/add_share/remove_share`)、**下载目录**(展示+修改)、**本机身份**(指纹/短码/设备名,设备名可改)、**信任设备**(列表:名称/短码/配对时间/权限,移除)、**防火墙**(`add_firewall_rule` 按钮+说明文案)、中继卡片置灰"阶段二提供"
  - `bench_loopback.rs`:本机起 server+client(随机端口),传输 1GB 合成数据(临时文件),打印 `MB/s` 与流数轨迹;断言 `> 300 MB/s`(回环下限,真实千兆验证见 README 手册项)
  - README:双机使用说明、防火墙首启放行指引、iperf3 对比方法(千兆有线要求 ≥ iperf3 的 90%)、便携说明(数据在 exe 旁 data\,换目录规则失效需重新放行防火墙)

- [ ] **Step 1: 设置页四卡片实现**(对话框用 tauri dialog 插件选目录)
- [ ] **Step 2: bench_loopback 编写与运行**

Run: `cargo run --release -p landrop-core --example bench_loopback`
Expected: 输出 ≥300 MB/s;若低于,检查缓冲池回收与流并发是否生效,调 `AdaptiveStreams` 初始值后重跑

- [ ] **Step 3: 便携构建**:`npm run tauri build -- --no-bundle` → 产出单 exe(约 8-12MB);把 exe 拷到全新临时目录运行 → 验证:自动生成 `data\`、首启防火墙弹窗、两份便携 exe 双机全流程冒烟(发现→配对→互拉→断线续传→隐身→推送)
- [ ] **Step 4: 双机真实吞吐**(手动,有真机时):iperf3 基线 vs lan-drop 传 1GB,记录到 README"基准记录"表
- [ ] **Step 5: Commit** `git commit -m "feat: 设置页/便携打包/回环基准/README"`(阶段一收口)

---

## 计划自检记录(writing-plans Self-Review)

**Spec 覆盖对照**(规格 §→任务):§3 架构/端口→T1/T2/T16;§4 发现与隐身→T6/T7/T18;§5 身份/配对/SAS→T3/T4/T11/T12/T13;§6 传输引擎(分块/并发/自适应/续传/小文件)→T9/T10/T14/T15;§8 共享区/权限/推送→T8/T14/T15/T19;§9 UI→T17-T20;§10 错误处理→分散在各任务错误枚举+T19 重试/横幅+T20 防火墙;§11 测试→各任务 TDD+T15 续传集成+T20 基准与冒烟;§7 中继=阶段二(本计划不含,规格已标注)。无缺口。
**类型一致性**:Fingerprint=[u8;32] 统一;serde 序列化到前端统一蛇形字段(T17 约定);job_id 统一 u64;ProgressEvent/TransferDto 在 T14 定义、T16/T17 消费,命名已对齐。
**已知取舍**:UI 任务(17-20)以"手动验证步骤"替代自动化 TDD(Vue 组件自动化测试性价比低,核心逻辑全部在 core 已被覆盖);若需要,可在执行时为 stores 补 vitest。





