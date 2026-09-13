//! v0.10.0 收件箱 hash 秒传:下载目录的已收文件索引 + 本地复用。
//! 索引文件 .localtrans-inbox-index.json 存于下载目录根;损坏降级空表。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug)]
struct IndexEntry {
    path: PathBuf,
    size: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct IndexFile {
    entries: HashMap<String, IndexEntry>,
}

pub struct InboxIndex {
    inner: IndexFile,
    path: PathBuf,
}

impl InboxIndex {
    /// 加载索引;文件缺失或 JSON 损坏 → 空索引(不炸,秒传退化为普通传输)
    pub fn load(index_path: &Path) -> Self {
        let inner = std::fs::read(index_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        InboxIndex { inner, path: index_path.to_path_buf() }
    }

    /// 命中条件:hash 存在且 size 与索引记录一致(防索引脏数据)
    pub fn lookup(&self, hash: &str, size: u64) -> Option<&PathBuf> {
        self.inner.entries.get(hash)
            .filter(|e| e.size == size)
            .map(|e| &e.path)
    }

    /// 命中时文件已不存在 → 删除该索引项(惰性清理)并返回未命中语义由调用方处理;
    /// 本方法只负责写入记录。
    pub fn insert(&mut self, hash: String, path: PathBuf, size: u64) {
        self.inner.entries.insert(hash, IndexEntry { path, size });
    }

    /// 移除指向已不存在文件的陈旧项(lookup 前调用)
    pub fn prune_missing(&mut self) {
        self.inner.entries.retain(|_, e| e.path.exists());
    }

    /// 原子落盘(tmp+rename);tmp 名带 pid 保证并发写者互不踩踏
    /// (低危审计修复:旧固定名 .tmp 多任务并发保存可能互相截断);
    /// 失败仅记警告不中断传输。
    /// 批量保存合并(整 offer 只 save 一次)由调用方保证——索引在 offer
    /// 编排中为单一实例,save 点已收敛到 offer 结束处,不再逐文件保存。
    pub fn save(&self) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let file_name = self.path.file_name().and_then(|f| f.to_str())
            .unwrap_or("index").to_string();
        let tmp = self.path.with_file_name(format!(
            "{}.{}.{}.{}.tmp", file_name, std::process::id(), n, nanos
        ));
        if let Ok(b) = serde_json::to_vec(&self.inner) {
            if let Err(e) = std::fs::write(&tmp, &b) {
                tracing::warn!("索引临时文件写入失败: {}", e);
            } else if let Err(e) = std::fs::rename(&tmp, &self.path) {
                tracing::warn!("索引原子替换失败: {}", e);
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

/// 把已命中的源文件放到目标位置:源==目标直接返回;先试硬链,
/// 失败回退复制;目标同名冲突按 "name (1).ext" 递增(镜像 write_small_file)。
pub fn place_dedup_copy(src: &Path, dest_dir: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)?;
    let first = dest_dir.join(file_name);
    if same_file(src, &first) {
        return Ok(first);
    }
    // 冲突重命名:name (1).ext、name (2).ext ...
    let stem = {
        let f = Path::new(file_name);
        f.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name).to_string()
    };
    let ext = Path::new(file_name)
        .extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();
    let mut candidate = first.clone();
    let mut n = 1u32;
    while candidate.exists() && !same_file(src, &candidate) {
        candidate = dest_dir.join(format!("{} ({}){}", stem, n, ext));
        n += 1;
    }
    if same_file(src, &candidate) {
        return Ok(candidate);
    }
    // 硬链优先(零拷贝);跨设备/不支持 → fs::copy
    if std::fs::hard_link(src, &candidate).is_err() {
        std::fs::copy(src, &candidate)?;
    }
    Ok(candidate)
}

fn same_file(a: &Path, b: &Path) -> bool {
    if !b.exists() || !a.exists() {
        return false;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// 流式整体 SHA-256(64KiB 缓冲,大文件不占内存)
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = Sha256::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// 内存数据整体 SHA-256(批流小文件已在内存,直接算)
pub fn sha256_of_slice(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_or_corrupt_returns_empty() {
        let dir = tempdir().unwrap();
        let idx = InboxIndex::load(&dir.path().join("idx.json"));
        assert!(idx.lookup("deadbeef", 1).is_none());
        std::fs::write(dir.path().join("idx.json"), "{not json").unwrap();
        let idx2 = InboxIndex::load(&dir.path().join("idx.json"));
        assert!(idx2.lookup("deadbeef", 1).is_none());
    }

    #[test]
    fn lookup_hit_requires_size_match() {
        let dir = tempdir().unwrap();
        let mut idx = InboxIndex::load(&dir.path().join("idx.json"));
        idx.insert("h1".into(), dir.path().join("a.bin"), 100);
        assert_eq!(idx.lookup("h1", 100), Some(&dir.path().join("a.bin")));
        assert_eq!(idx.lookup("h1", 101), None, "size 不符视为未命中");
        assert_eq!(idx.lookup("h2", 100), None);
    }

    #[test]
    fn insert_then_save_reload_roundtrip() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("idx.json");
        {
            let mut idx = InboxIndex::load(&p);
            idx.insert("h1".into(), dir.path().join("a.bin"), 7);
            idx.save();
        }
        let idx2 = InboxIndex::load(&p);
        assert_eq!(idx2.lookup("h1", 7), Some(&dir.path().join("a.bin")));
    }

    #[test]
    fn place_copy_hardlink_or_copy_and_conflict_rename() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.bin");
        std::fs::write(&src, b"hello").unwrap();
        // 目标已有同名文件 → 冲突重命名 name (1).ext
        std::fs::create_dir(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out").join("src.bin"), b"old").unwrap();
        let dest = place_dedup_copy(&src, &dir.path().join("out"), "src.bin").unwrap();
        assert_eq!(dest.file_name().unwrap().to_str().unwrap(), "src (1).bin");
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(std::fs::read(dir.path().join("out").join("src.bin")).unwrap(), b"old");
    }

    #[test]
    fn place_copy_same_path_guard() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("same.bin");
        std::fs::write(&src, b"x").unwrap();
        let dest = place_dedup_copy(&src, dir.path(), "same.bin").unwrap();
        assert_eq!(dest, src); // 源即目标:不复制
    }

    #[test]
    fn sha256_file_known_vector() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
