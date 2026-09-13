use crate::protocol::CHUNK_SIZE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Seek};
use std::path::Path;

/// 分块传输清单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub file_name: String,
    pub total_size: u64,
    #[serde(with = "hex_vec")]
    pub chunk_hashes: Vec<String>,
    pub received: Vec<bool>,
    /// 对端指纹（hex编码）- R9-3: 任务溯源
    #[serde(default)]
    pub peer: Option<String>,
    /// 共享区ID - R9-3: 任务溯源
    #[serde(default)]
    pub share_id: Option<String>,
    /// 相对路径 - R9-3: 任务溯源
    #[serde(default)]
    pub rel: Option<String>,
    /// 卡片元数据(2026-08-30 传输域定案:manifest 唯一真相)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<TransferMeta>,
}

/// 传输卡片元数据（manifest 内唯一真相，2026-08-30 传输域定案）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TransferMeta {
    pub direction: String,
    pub local_role: String,
    pub display_name: String,
    pub peer_hex: String,
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub fail_reason: Option<String>,
    pub source_path: Option<String>,
    pub batch_label: Option<String>,
}

/// 状态迁移历史事件（history.jsonl 一行一条）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub ts_ms: i64,
    pub from: String,
    pub to: String,
    pub reason: Option<String>,
}

impl Manifest {
    /// 读取卡片元数据
    pub fn meta(&self) -> Option<&TransferMeta> {
        self.meta.as_ref()
    }

    /// 设置卡片元数据
    pub fn set_meta(&mut self, m: TransferMeta) {
        self.meta = Some(m);
    }
}

fn job_dir(parts_root: &Path, job_id: u64) -> std::path::PathBuf {
    parts_root
        .join(".localtrans-parts")
        .join(format!("{job_id:016x}"))
}

/// 终态后原地修改 manifest 元数据（load→改→save；manifest 不存在返回 false）
/// 注意：只在终态后调用，活动期间 PartWriter 节流写会覆盖
pub fn patch_manifest_meta(parts_root: &Path, job_id: u64, f: impl FnOnce(&mut TransferMeta)) -> bool {
    let dir = job_dir(parts_root, job_id);
    if !dir.join("manifest.json").exists() {
        return false;
    }
    let mut manifest = match Manifest::load(&dir) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if manifest.meta.is_none() {
        manifest.meta = Some(TransferMeta::default());
    }
    f(manifest.meta.as_mut().unwrap());
    manifest.save(&dir).is_ok()
}

/// 追加一条状态迁移历史到 history.jsonl
pub fn append_history(parts_root: &Path, job_id: u64, ev: &HistoryEvent) -> std::io::Result<()> {
    let dir = job_dir(parts_root, job_id);
    fs::create_dir_all(&dir)?;
    let line = serde_json::to_string(ev)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("history.jsonl"))?;
    writeln!(file, "{line}")
}

/// 读取全部状态迁移历史（损坏行跳过）
pub fn load_history(parts_root: &Path, job_id: u64) -> Vec<HistoryEvent> {
    let path = job_dir(parts_root, job_id).join("history.jsonl");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// 用于序列化 hex 字符串数组的辅助模块
mod hex_vec {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::vec::Vec;

    pub fn serialize<S>(vec: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(vec.iter())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<String>::deserialize(deserializer)
    }
}

/// 清单错误
#[derive(Debug)]
pub enum ManifestError {
    /// IO 错误
    Io(io::Error),
    /// 序列化错误
    Serde(serde_json::Error),
    /// 数据不一致：received 数组长度与 chunk_hashes 不匹配
    InconsistentLengths,
    /// 文件读取错误
    FileReadError(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(err) => write!(f, "IO 错误: {}", err),
            ManifestError::Serde(err) => write!(f, "序列化错误: {}", err),
            ManifestError::InconsistentLengths => write!(f, "数据不一致：received 数组长度与 chunk_hashes 不匹配"),
            ManifestError::FileReadError(msg) => write!(f, "文件读取错误: {}", msg),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<io::Error> for ManifestError {
    fn from(err: io::Error) -> Self {
        ManifestError::Io(err)
    }
}

impl From<serde_json::Error> for ManifestError {
    fn from(err: serde_json::Error) -> Self {
        ManifestError::Serde(err)
    }
}

impl Manifest {
    /// 构建文件分块清单
    pub fn build(path: &Path) -> Result<Self, ManifestError> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ManifestError::FileReadError("无法获取文件名".to_string()))?
            .to_string();

        let total_size = fs::metadata(path)?.len();

        let chunk_count = if total_size == 0 {
            0
        } else {
            ((total_size - 1) / CHUNK_SIZE as u64) + 1
        } as usize;

        let mut chunk_hashes = Vec::with_capacity(chunk_count);
        let received = vec![false; chunk_count];

        if chunk_count > 0 {
            let mut file = fs::File::open(path)?;
            let mut buffer = vec![0u8; CHUNK_SIZE];

            for i in 0..chunk_count {
                let offset = i as u64 * CHUNK_SIZE as u64;
                file.seek(io::SeekFrom::Start(offset))?;

                let remaining = total_size - offset;
                let chunk_size = std::cmp::min(CHUNK_SIZE as u64, remaining) as usize;

                buffer.truncate(chunk_size);
                file.read_exact(&mut buffer)?;

                let hash = hex::encode(Sha256::digest(&buffer));
                chunk_hashes.push(hash);

                buffer.resize(CHUNK_SIZE, 0);
            }
        }

        Ok(Manifest {
            file_name,
            total_size,
            chunk_hashes,
            received,
            peer: None,
            share_id: None,
            rel: None,
            meta: None,
        })
    }

    /// 从元数据构造清单（接收方收到 MetaResp 后使用，received 全 false）
    pub fn from_meta(file_name: String, total_size: u64, chunk_hashes: Vec<String>) -> Self {
        let received = vec![false; chunk_hashes.len()];
        Manifest {
            file_name,
            total_size,
            chunk_hashes,
            received,
            peer: None,
            share_id: None,
            rel: None,
            meta: None,
        }
    }

    /// 从元数据构造清单（R9-3: 带任务溯源信息）
    pub fn from_meta_with_source(
        file_name: String,
        total_size: u64,
        chunk_hashes: Vec<String>,
        peer: Option<String>,
        share_id: Option<String>,
        rel: Option<String>,
    ) -> Self {
        let received = vec![false; chunk_hashes.len()];
        Manifest {
            file_name,
            total_size,
            chunk_hashes,
            received,
            peer,
            share_id,
            rel,
            meta: None,
        }
    }

    /// 获取分块总数
    pub fn chunk_count(&self) -> u32 {
        self.chunk_hashes.len() as u32
    }

    /// 获取缺失的分块索引
    pub fn missing_chunks(&self) -> Vec<u32> {
        self.received
            .iter()
            .enumerate()
            .filter_map(|(i, &received)| if !received { Some(i as u32) } else { None })
            .collect()
    }

    /// 获取指定分块的文件偏移量
    pub fn chunk_offset(&self, i: u32) -> u64 {
        i as u64 * CHUNK_SIZE as u64
    }

    /// 获取指定分块的长度
    pub fn chunk_len(&self, i: u32) -> u64 {
        let offset = self.chunk_offset(i);
        let remaining = self.total_size.saturating_sub(offset);
        std::cmp::min(CHUNK_SIZE as u64, remaining)
    }

    /// 保存清单到指定目录
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let manifest_path = dir.join("manifest.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&manifest_path, json)
    }

    /// 从指定目录加载清单
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        let manifest_path = dir.join("manifest.json");
        let json = fs::read_to_string(&manifest_path)?;
        let manifest: Manifest = serde_json::from_str(&json)?;

        // 验证数据一致性
        if manifest.received.len() != manifest.chunk_hashes.len() {
            return Err(ManifestError::InconsistentLengths);
        }

        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_hashes_and_geometry() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![7u8; CHUNK_SIZE * 2 + 100]).unwrap();

        let m = Manifest::build(&p).unwrap();
        assert_eq!(m.chunk_count(), 3);
        assert_eq!(m.chunk_len(0), CHUNK_SIZE as u64);
        assert_eq!(m.chunk_len(2), 100);
        assert_eq!(m.chunk_offset(2), 2 * CHUNK_SIZE as u64);
        assert_eq!(m.missing_chunks(), vec![0, 1, 2]);

        // 空/单块文件几何
        let p2 = tmp.path().join("empty.bin");
        fs::write(&p2, b"").unwrap();
        assert_eq!(Manifest::build(&p2).unwrap().chunk_count(), 0);
    }

    #[test]
    fn save_load_roundtrip_with_partial_bitmap() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![42u8; CHUNK_SIZE * 2 + 50]).unwrap();

        let mut m = Manifest::build(&p).unwrap();
        // 模拟部分接收
        m.received[1] = true;

        let dir = tmp.path().join("parts");
        m.save(&dir).unwrap();

        let m2 = Manifest::load(&dir).unwrap();
        assert_eq!(m2.file_name, m.file_name);
        assert_eq!(m2.total_size, m.total_size);
        assert_eq!(m2.chunk_hashes, m.chunk_hashes);
        assert_eq!(m2.received, m.received);
        assert_eq!(m2.received[1], true);
        assert_eq!(m2.missing_chunks(), vec![0, 2]);
    }

    #[test]
    fn meta_roundtrip_and_default_absent() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![1u8; CHUNK_SIZE + 10]).unwrap();
        let mut m = Manifest::build(&p).unwrap();
        // 旧版 manifest 无 meta 字段 → None,加载不报错
        assert!(m.meta().is_none());

        let mut meta = TransferMeta::default();
        meta.direction = "pull".into();
        meta.local_role = "destination".into();
        meta.display_name = "电影合集".into();
        meta.peer_hex = "aabbccdd11223344".into();
        meta.created_at_ms = 1770000000000i64;
        m.set_meta(meta);

        let dir = tmp.path().join("parts");
        m.save(&dir).unwrap();
        let m2 = Manifest::load(&dir).unwrap();
        assert_eq!(m2.meta().unwrap().display_name, "电影合集");
        assert_eq!(m2.meta().unwrap().direction, "pull");
    }

    #[test]
    fn patch_manifest_meta_updates_in_place() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![2u8; 5]).unwrap();
        let mut m = Manifest::build(&p).unwrap();
        let mut meta = TransferMeta::default();
        meta.direction = "pull".into();
        meta.created_at_ms = 100;
        m.set_meta(meta);
        let dir = tmp.path().join(".localtrans-parts/00000000000000ff");
        m.save(&dir).unwrap();

        let ok = patch_manifest_meta(tmp.path(), 0xff, |mt| {
            mt.fail_reason = Some("对端拒绝".into());
            mt.finished_at_ms = Some(999);
        });
        assert!(ok);
        let m2 = Manifest::load(&dir).unwrap();
        assert_eq!(m2.meta().unwrap().fail_reason.as_deref(), Some("对端拒绝"));
        assert_eq!(m2.meta().unwrap().finished_at_ms, Some(999));
        assert!(!patch_manifest_meta(tmp.path(), 0xdead, |_| {}), "不存在应返回 false");
    }

    #[test]
    fn history_append_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000007");
        fs::create_dir_all(&job).unwrap();
        append_history(tmp.path(), 7, &HistoryEvent {
            ts_ms: 1, from: "active".into(), to: "paused".into(), reason: None,
        }).unwrap();
        append_history(tmp.path(), 7, &HistoryEvent {
            ts_ms: 2, from: "paused".into(), to: "failed".into(), reason: Some("对端断开".into()),
        }).unwrap();
        let h = load_history(tmp.path(), 7);
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].reason.as_deref(), Some("对端断开"));
        // 损坏行跳过不炸
        fs::write(job.join("history.jsonl"), "not json\n").unwrap();
        assert!(load_history(tmp.path(), 7).is_empty());
    }

    #[test]
    fn empty_file_missing_chunks() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("empty.bin");
        fs::write(&p, b"").unwrap();

        let m = Manifest::build(&p).unwrap();
        assert!(m.missing_chunks().is_empty());
    }
}
