// Step 1: 写失败测试
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rustls_pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use thiserror::Error;
use serde::{Serialize, Deserialize};

pub type Fingerprint = [u8; 32];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PushPolicy {
    Ask,
    Auto,
    Deny,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Perms {
    pub browse: bool,
    pub download: bool,
    pub push: PushPolicy,
}

impl Default for Perms {
    fn default() -> Self {
        Perms {
            browse: true,
            download: true,
            push: PushPolicy::Ask,
        }
    }
}

impl Perms {
    /// P0-5 fail-closed:不在信任表的对端一律全拒。
    /// 与 Default(browse/download 默认true,配对初始授权用)相反,仅用于查不到记录的回退。
    pub fn denied() -> Self {
        Perms { browse: false, download: false, push: PushPolicy::Deny }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrustedPeer {
    #[serde(with = "fp_hex")]
    pub fingerprint: Fingerprint,
    pub name: String,
    /// 本地别名:用户给这台设备起的名字,仅本机展示用。
    /// 空 = 未设置,展示层回退用 name(对方广播的最新名)。
    /// 与 name 分离——对方改名后本机自动跟随,别名不受影响。
    #[serde(default)]
    pub alias: String,
    pub paired_at: u64,
    pub perms: Perms,
}

mod fp_hex {
    use super::Fingerprint;
    use serde::{Serializer, Deserializer, de::Deserialize};

    pub fn serialize<S>(fp: &Fingerprint, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex = hex::encode(fp);
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Fingerprint, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(|e| Error::custom(format!("invalid hex: {}", e)))?;
        if bytes.len() != 32 {
            return Err(Error::custom(format!("invalid fingerprint length: {}", bytes.len())));
        }
        let mut fp = [0u8; 32];
        fp.copy_from_slice(&bytes);
        Ok(fp)
    }
}

pub struct TrustStore {
    peers: Vec<TrustedPeer>,
    dir: std::path::PathBuf,
}

impl TrustStore {
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("trusted_peers.json");
        let peers = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("trusted_peers.json 损坏({e}),重置为空");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        TrustStore {
            peers,
            dir: dir.to_path_buf(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = self.dir.join("trusted_peers.json");
        let json = serde_json::to_string_pretty(&self.peers)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(&path, json.as_bytes())
    }

    pub fn is_trusted(&self, fp: &Fingerprint) -> bool {
        self.peers.iter().any(|p| &p.fingerprint == fp)
    }

    pub fn upsert(&mut self, peer: TrustedPeer) {
        if let Some(existing) = self.peers.iter().position(|p| p.fingerprint == peer.fingerprint) {
            self.peers[existing] = peer;
        } else {
            self.peers.push(peer);
        }
    }

    pub fn remove(&mut self, fp: &Fingerprint) -> bool {
        if let Some(pos) = self.peers.iter().position(|p| &p.fingerprint == fp) {
            self.peers.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn get(&self, fp: &Fingerprint) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| &p.fingerprint == fp)
    }

    /// 全部信任对端快照（T16 UI 列表用）
    pub fn all_peers(&self) -> Vec<TrustedPeer> {
        self.peers.clone()
    }

    pub fn set_perms(&mut self, fp: &Fingerprint, perms: Perms) -> bool {
        if let Some(peer) = self.peers.iter_mut().find(|p| &p.fingerprint == fp) {
            peer.perms = perms;
            true
        } else {
            false
        }
    }

    /// 设置本地别名(空串=清除)。别名仅本机展示用,不影响广播名。
    pub fn set_alias(&mut self, fp: &Fingerprint, alias: String) -> bool {
        if let Some(peer) = self.peers.iter_mut().find(|p| &p.fingerprint == fp) {
            peer.alias = alias;
            true
        } else {
            false
        }
    }
}

#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("密钥/证书解析错误: {0}")]
    Parse(String),
}

pub struct Identity {
    pub signing: SigningKey,
    pub cert: CertificateDer<'static>,
    pub pkcs8: Vec<u8>,
}

pub fn fingerprint_of(cert: &CertificateDer<'_>) -> Fingerprint {
    Sha256::digest(cert.as_ref()).into()
}

/// 从证书 DER 解析出 ed25519 验证公钥(S2 服务端验签用;x509-parser 提取 SPKI)
pub fn verifying_key_from_cert_der(
    der: &[u8],
) -> Result<ed25519_dalek::VerifyingKey, IdentityError> {
    use x509_parser::prelude::*;
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| IdentityError::Parse(e.to_string()))?;
    let raw: &[u8] = &cert.public_key().subject_public_key.data;
    if raw.len() != 32 {
        return Err(IdentityError::Parse(format!(
            "非 Ed25519 公钥, subjectPublicKey 长度 {}",
            raw.len()
        )));
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(raw);
    ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| IdentityError::Parse(e.to_string()))
}

/// S2 占有证明验证:声明指纹 == sha256(证书),且证书公钥可验 sign(msg)。
/// 三者(证书/指纹/签名)来自同一把私钥才可能全过。
/// 注:不做"证书自签"校验——指纹绑定已锁死证书与声明身份,
/// 自签校验不增加占有证明强度(证书本就由本机 Identity 自签)。
pub fn verify_possession(
    declared_fp: &Fingerprint,
    cert_der: &[u8],
    msg: &[u8],
    sig: &[u8; 64],
) -> Result<(), IdentityError> {
    use ed25519_dalek::Verifier;
    // 1. 解析证书
    let der = CertificateDer::from(cert_der.to_vec());
    // 2. 指纹绑定:声明的 fp 必须是这张证书的哈希
    let actual_fp = fingerprint_of(&der);
    if &actual_fp != declared_fp {
        return Err(IdentityError::Parse("指纹与证书不符".into()));
    }
    // 3. 签名验证:证书内公钥对 msg 的签名成立
    let vk = verifying_key_from_cert_der(cert_der)?;
    vk.verify(msg, &ed25519_dalek::Signature::from_bytes(sig))
        .map_err(|_| IdentityError::Parse("签名验证失败".into()))
}

// Re-export atomic_write from store
use crate::store::atomic_write;

impl Identity {
    pub fn load_or_create(dir: &std::path::Path) -> Result<Self, IdentityError> {
        let key_path = dir.join("identity.key");
        let cert_path = dir.join("cert.der");

        // 三分支: 两文件都在 → 加载; 仅 key 在 → 重新签发证书; 都在缺 → 全新生成
        if key_path.exists() && cert_path.exists() {
            let pkcs8 = std::fs::read(&key_path)?;
            let cert = CertificateDer::from(std::fs::read(&cert_path)?);
            let signing = SigningKey::from_pkcs8_der(&pkcs8)
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            return Ok(Identity { signing, cert, pkcs8 });
        }

        if key_path.exists() && !cert_path.exists() {
            // 仅 key 在 → 用该 key 重新签发证书(确定性,指纹不变)并落盘 cert.der
            let pkcs8 = std::fs::read(&key_path)?;
            let signing = SigningKey::from_pkcs8_der(&pkcs8)
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            let kp = rcgen::KeyPair::try_from(&pkcs8[..])
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            let params = rcgen::CertificateParams::new(vec!["LocalTrans".to_string()])
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            // 设置固定序列号,确保同一密钥重新签发得到字节级相同的 DER
            let mut params = params;
            params.serial_number = Some(vec![1].into());
            let cert = params.self_signed(&kp)
                .map_err(|e| IdentityError::Parse(e.to_string()))?;
            let cert_der = CertificateDer::from(cert.der().to_vec());

            // 原子写入 cert.der
            atomic_write(&cert_path, cert_der.as_ref()).map_err(IdentityError::from)?;
            return Ok(Identity { signing, cert: cert_der, pkcs8 });
        }

        // 都在缺 → 全新生成
        let mut csprng = rand::rngs::OsRng;
        let signing = SigningKey::generate(&mut csprng);
        let pkcs8 = signing
            .to_pkcs8_der()
            .map_err(|e| IdentityError::Parse(e.to_string()))?
            .to_bytes()
            .to_vec();
        let kp = rcgen::KeyPair::try_from(&pkcs8[..])
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        let params = rcgen::CertificateParams::new(vec!["LocalTrans".to_string()])
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        // 设置固定序列号
        let mut params = params;
        params.serial_number = Some(vec![1].into());
        let cert = params
            .self_signed(&kp)
            .map_err(|e| IdentityError::Parse(e.to_string()))?;
        let cert_der = CertificateDer::from(cert.der().to_vec());

        // 原子写入: key 先写、cert 后写
        atomic_write(&key_path, &pkcs8).map_err(IdentityError::from)?;
        atomic_write(&cert_path, cert_der.as_ref()).map_err(IdentityError::from)?;

        Ok(Identity {
            signing,
            cert: cert_der,
            pkcs8,
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint_of(&self.cert)
    }

    /// S2 占有证明:对任意消息做 ed25519 签名(中继 Register 用)
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing.sign(msg).to_bytes()
    }

    pub fn short_code(&self) -> String {
        let fp = self.fingerprint();
        format!(
            "{}-{}",
            hex_upper(&fp[0..2]),
            hex_upper(&fp[2..4])
        )
    }
}

fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn verifying_key_from_cert_matches_own_key() {
        let dir = tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        let vk = verifying_key_from_cert_der(id.cert.as_ref()).unwrap();
        let expect = ed25519_dalek::VerifyingKey::from(&id.signing);
        assert_eq!(vk.as_bytes(), expect.as_bytes(), "证书公钥应等于自身私钥的公钥");
    }

    #[test]
    fn verify_possession_happy_and_forge() {
        let dir = tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        let fp = id.fingerprint();
        let msg = b"hello";
        let sig = id.sign(msg);
        assert!(verify_possession(&fp, id.cert.as_ref(), msg, &sig).is_ok());

        // 伪造:错误签名
        let bad_sig = [1u8; 64];
        assert!(verify_possession(&fp, id.cert.as_ref(), msg, &bad_sig).is_err());
        // 伪造:错误指纹(真证书+真签名)
        assert!(verify_possession(&[0u8; 32], id.cert.as_ref(), msg, &sig).is_err());
    }

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
        assert_eq!(
            id.fingerprint().to_vec(),
            Sha256::digest(id.cert.as_ref()).to_vec()
        );
    }

    #[test]
    fn short_code_format_and_uniqueness() {
        let a = Identity::load_or_create(&tempfile::tempdir().unwrap().path()).unwrap();
        let b = Identity::load_or_create(&tempfile::tempdir().unwrap().path()).unwrap();
        assert_eq!(a.short_code().len(), 9); // XXXX-XXXX
        assert_ne!(a.short_code(), b.short_code());
    }

    #[test]
    fn cert_recovery_from_key_preserves_fingerprint() {
        let dir = tempdir().unwrap();
        let id1 = Identity::load_or_create(dir.path()).unwrap();
        let fp1 = id1.fingerprint();

        // 删除 cert.der, 保留 identity.key
        std::fs::remove_file(dir.path().join("cert.der")).unwrap();

        // 重新加载应该从 key 恢复 cert,指纹必须不变
        let id2 = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(fp1, id2.fingerprint(), "从 key 恢复 cert 后指纹必须不变");
        assert!(dir.path().join("cert.der").exists(), "cert.der 应该被重新创建");
    }

    #[test]
    fn trust_store_crud_and_default_perms() {
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TrustStore::load(dir.path());
        let fp = [7u8; 32];
        assert!(!ts.is_trusted(&fp));
        ts.upsert(TrustedPeer { fingerprint: fp, name: "同事电脑".into(), alias: String::new(), paired_at: 1000, perms: Perms::default() });
        assert!(ts.is_trusted(&fp));
        assert!(matches!(ts.get(&fp).unwrap().perms.push, PushPolicy::Ask));
        ts.save().unwrap();
        let mut ts2 = TrustStore::load(dir.path());   // 从磁盘恢复
        assert_eq!(ts2.get(&fp).unwrap().name, "同事电脑");
        assert!(ts2.remove(&fp));
        assert!(!ts2.is_trusted(&fp));
    }

    #[test]
    fn fingerprint_serializes_as_hex() {
        let p = TrustedPeer { fingerprint: [1u8; 32], name: "x".into(), alias: String::new(), paired_at: 0, perms: Perms::default() };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("01010101")); // hex 可读,而非数组
    }

    #[test]
    fn all_peers_returns_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TrustStore::load(dir.path());
        ts.upsert(TrustedPeer { fingerprint: [1u8; 32], name: "a".into(), alias: String::new(), paired_at: 1, perms: Perms::default() });
        ts.upsert(TrustedPeer { fingerprint: [2u8; 32], name: "b".into(), alias: String::new(), paired_at: 2, perms: Perms::default() });
        let peers = ts.all_peers();
        assert_eq!(peers.len(), 2);
        assert!(peers.iter().any(|p| p.name == "a") && peers.iter().any(|p| p.name == "b"));
    }

    #[test]
    fn alias_set_clear_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut ts = TrustStore::load(dir.path());
        let fp = [9u8; 32];
        ts.upsert(TrustedPeer { fingerprint: fp, name: "广播名".into(), alias: String::new(), paired_at: 1, perms: Perms::default() });

        // 设置别名
        assert!(ts.set_alias(&fp, "我的手机".into()));
        assert_eq!(ts.get(&fp).unwrap().alias, "我的手机");
        // 广播名不受别名影响
        assert_eq!(ts.get(&(fp)).unwrap().name, "广播名");

        // 落盘恢复
        ts.save().unwrap();
        let ts2 = TrustStore::load(dir.path());
        assert_eq!(ts2.get(&fp).unwrap().alias, "我的手机");

        // 清除别名(空串)
        let mut ts3 = TrustStore::load(dir.path());
        assert!(ts3.set_alias(&fp, "".into()));
        assert_eq!(ts3.get(&fp).unwrap().alias, "");

        // 未知指纹返回 false
        let mut ts4 = TrustStore::load(dir.path());
        assert!(!ts4.set_alias(&[0xAA; 32], "x".into()));
    }

    /// 旧版 trusted_peers.json(无 alias 字段)加载不失败,alias 默认空
    #[test]
    fn old_trusted_file_without_alias_loads() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = r#"[{
            "fingerprint": "0909090909090909090909090909090909090909090909090909090909090909",
            "name": "旧设备",
            "paired_at": 123,
            "perms": { "browse": true, "download": true, "push": "ask" }
        }]"#;
        std::fs::write(dir.path().join("trusted_peers.json"), legacy).unwrap();
        let ts = TrustStore::load(dir.path());
        let fp = [9u8; 32];
        assert_eq!(ts.get(&fp).unwrap().name, "旧设备");
        assert_eq!(ts.get(&fp).unwrap().alias, "", "旧文件无 alias 应默认空");
    }
}
