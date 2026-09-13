use crate::store::ShareDef;
use crate::protocol::FileEntry;
use std::path::{Path, PathBuf};
use std::io;
use std::sync::{Arc, RwLock};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

#[derive(Debug)]
pub enum ShareError {
    UnknownShare,
    IllegalPath,
    NotFound,
    /// M-B8: 目录条目超过上限(50k),拒绝枚举防止内存/响应卡死
    TooManyEntries,
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::UnknownShare => write!(f, "未知共享区"),
            ShareError::IllegalPath => write!(f, "非法路径"),
            ShareError::NotFound => write!(f, "文件或目录不存在"),
            ShareError::TooManyEntries => write!(f, "目录条目过多"),
        }
    }
}

impl std::error::Error for ShareError {}

/// 共享区注册表：内部 RwLock 经 Arc 共享，Clone 是句柄复制（watchdog
/// 与路由器各持一份，看到的始终是同一份共享区定义）
#[derive(Clone)]
pub struct ShareRegistry {
    shares: Arc<RwLock<Vec<ShareDef>>>,
}

impl ShareRegistry {
    pub fn new(shares: Vec<ShareDef>) -> Self {
        Self { shares: Arc::new(RwLock::new(shares)) }
    }

    pub fn list(&self) -> Vec<ShareDef> {
        self.shares.read().unwrap().clone()
    }

    /// R9-4: 添加共享区（若 ID 已存在则覆盖）
    pub fn add(&self, def: ShareDef) {
        let mut shares = self.shares.write().unwrap();
        // 移除同 ID 的旧共享区（若有）
        shares.retain(|s| s.id != def.id);
        shares.push(def);
    }

    /// R9-4: 移除共享区（返回是否找到并删除）
    pub fn remove(&self, id: &str) -> bool {
        let mut shares = self.shares.write().unwrap();
        let orig_len = shares.len();
        shares.retain(|s| s.id != id);
        shares.len() < orig_len
    }

    pub fn resolve(&self, share_id: &str, rel: &str) -> Result<PathBuf, ShareError> {
        // Find the share and clone it to extend the borrow lifetime
        let share_def = self.shares.read().unwrap().iter()
            .find(|s| s.id == share_id)
            .ok_or(ShareError::UnknownShare)?
            .clone();

        // Check for path traversal attempts using Component iteration
        for component in Path::new(rel).components() {
            match component {
                std::path::Component::ParentDir => {
                    return Err(ShareError::IllegalPath);
                }
                std::path::Component::Prefix(_) => {
                    // Reject Windows drive letter prefixes
                    return Err(ShareError::IllegalPath);
                }
                std::path::Component::RootDir => {
                    // Reject absolute paths
                    return Err(ShareError::IllegalPath);
                }
                _ => {}
            }
        }

        // Check for Windows drive letters in the path string itself
        if rel.contains(':') {
            return Err(ShareError::IllegalPath);
        }

        // Build the full path
        let full_path = share_def.path.join(rel);

        // ① Canonicalize both paths first
        let canonical_full = full_path.canonicalize()
            .map_err(|_| ShareError::NotFound)?;
        let canonical_root = share_def.path.canonicalize()
            .map_err(|_| ShareError::NotFound)?;

        // ② Ensure the canonical path starts with the canonical root
        if !canonical_full.starts_with(&canonical_root) {
            return Err(ShareError::IllegalPath);
        }

        // ③ Check symlink_metadata to reject reparse points (after validation)
        let metadata = match std::fs::symlink_metadata(&full_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ShareError::NotFound),
            Err(_) => return Err(ShareError::NotFound),
        };

        // Reject reparse points (junctions, symlinks) - must return IllegalPath
        #[cfg(windows)]
        if metadata.file_attributes() & 0x400 != 0 {
            // FILE_ATTRIBUTE_REPARSE_POINT
            return Err(ShareError::IllegalPath);
        }

        #[cfg(not(windows))]
        if metadata.file_type().is_symlink() {
            return Err(ShareError::IllegalPath);
        }

        // ④ Return canonicalized path (not the original joined path)
        Ok(canonical_full)
    }

    pub async fn list_dir(&self, share_id: &str, rel: &str, cursor: u64, limit: usize) -> Result<(Vec<FileEntry>, Option<u64>), ShareError> {
        const MAX_LIMIT: usize = 500;

        // Apply limit cap (limit=0 returns empty results, not 500)
        let limit = limit.min(MAX_LIMIT);

        // Resolve the path to get the full directory path
        let dir_path = self.resolve(share_id, rel)?;

        // Check if it's a directory
        if !dir_path.is_dir() {
            return Err(ShareError::NotFound);
        }

        // Read the directory
        let mut entries = tokio::fs::read_dir(&dir_path).await
            .map_err(|_| ShareError::NotFound)?;

        // M-B8: 条目数硬上限——巨型目录(如 C:\Windows\System32 或网盘挂载点)
        // 全量枚举会卡死 RPC 响应并撑爆内存,fail-closed 报错让客户端及时显示失败
        const MAX_ENTRIES: usize = 50_000;
        let mut count: usize = 0;
        let mut all_entries: Vec<FileEntry> = Vec::new();

        while let Some(entry) = entries.next_entry().await
            .map_err(|_| ShareError::NotFound)? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy().to_string();

            // Skip hidden files
            if name_str.starts_with('.') {
                continue;
            }

            count += 1;
            if count > MAX_ENTRIES {
                return Err(ShareError::TooManyEntries);
            }

            let metadata = entry.metadata().await
                .map_err(|_| ShareError::NotFound)?;

            // Skip hidden/system files on Windows
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                let attrs = metadata.file_attributes();
                if attrs & 0x02 != 0 || attrs & 0x04 != 0 {
                    // FILE_ATTRIBUTE_HIDDEN or FILE_ATTRIBUTE_SYSTEM
                    continue;
                }
            }

            let mtime = metadata.modified()
                .map_err(|_| ShareError::NotFound)?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| ShareError::NotFound)?
                .as_secs();

            all_entries.push(FileEntry {
                name: name_str,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                mtime,
            });
        }

        // Sort by name
        all_entries.sort_by(|a, b| a.name.cmp(&b.name));

        // Apply pagination (cursor is offset)
        let cursor_usize = cursor as usize;

        if cursor_usize >= all_entries.len() {
            return Ok((vec![], None));
        }

        let end_idx = std::cmp::min(cursor_usize + limit, all_entries.len());
        let page_entries: Vec<FileEntry> = all_entries[cursor_usize..end_idx].to_vec();

        // Set next_cursor if there are more entries
        let next_cursor = if end_idx < all_entries.len() {
            Some(end_idx as u64)
        } else {
            None
        };

        Ok((page_entries, next_cursor))
    }

    pub fn share_id_of_alias(&self, alias: &str) -> Option<String> {
        self.shares.read().unwrap().iter()
            .find(|s| s.alias == alias)
            .map(|s| s.id.clone())
    }

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
        if !Self::valid_name(new_name) {
            return Err(ShareError::IllegalPath);
        }
        let old = self.resolve(share_id, rel)?;
        let parent = old.parent().ok_or_else(|| ShareError::IllegalPath)?;
        let target = parent.join(new_name);
        if target.exists() {
            return Err(ShareError::IllegalPath);
        }
        std::fs::rename(&old, &target).map_err(|_| ShareError::NotFound)
    }

    pub fn op_delete(&self, share_id: &str, rel: &str) -> Result<(), ShareError> {
        let p = self.resolve(share_id, rel)?;
        if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        }.map_err(|_| ShareError::NotFound)
    }

    pub fn op_mkdir(&self, share_id: &str, rel: &str) -> Result<(), ShareError> {
        // For mkdir, we need to validate the path without requiring it to exist
        // Build the path manually with the same validation as resolve, but skip the existence check
        let share_def = self.shares.read().unwrap().iter()
            .find(|s| s.id == share_id)
            .ok_or(ShareError::UnknownShare)?
            .clone();

        // Check for path traversal attempts using Component iteration
        for component in Path::new(rel).components() {
            match component {
                std::path::Component::ParentDir => {
                    return Err(ShareError::IllegalPath);
                }
                std::path::Component::Prefix(_) => {
                    return Err(ShareError::IllegalPath);
                }
                std::path::Component::RootDir => {
                    return Err(ShareError::IllegalPath);
                }
                _ => {}
            }
        }

        // Check for Windows drive letters in the path string itself
        if rel.contains(':') {
            return Err(ShareError::IllegalPath);
        }

        // Build the full path
        let full_path = share_def.path.join(rel);

        // Canonicalize the parent (share root) to ensure we're creating within the share
        let canonical_root = share_def.path.canonicalize()
            .map_err(|_| ShareError::NotFound)?;

        // If the directory already exists, return error
        if full_path.exists() {
            return Err(ShareError::IllegalPath);
        }

        // Create the directory
        std::fs::create_dir_all(&full_path).map_err(|_| ShareError::NotFound)?;

        // Verify the created path is within the share root
        let canonical_created = full_path.canonicalize()
            .map_err(|_| ShareError::NotFound)?;
        if !canonical_created.starts_with(&canonical_root) {
            // Clean up the created directory if it's outside the share
            let _ = std::fs::remove_dir_all(&full_path);
            return Err(ShareError::IllegalPath);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    fn reg(tmp: &Path) -> ShareRegistry {
        let root = tmp.join("share");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"hi").unwrap();
        ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "分享".into(),
            path: root
        }])
    }

    #[tokio::test]
    async fn list_dir_rejects_over_50k_entries() {
        // M-B8: 50001 个可见文件应触发 TooManyEntries(消息"目录条目过多")
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("big");
        fs::create_dir_all(&root).unwrap();
        for i in 0..50_001u32 {
            fs::write(root.join(format!("f{:07}", i)), b"x").unwrap();
        }
        let r = ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "大目录".into(),
            path: root,
        }]);
        let err = r.list_dir("s1", "", 0, 100).await.unwrap_err();
        assert!(matches!(err, ShareError::TooManyEntries), "got: {:?}", err);
        assert_eq!(err.to_string(), "目录条目过多");
    }

    #[test]
    fn add_remove_changes_list() {
        let tmp = tempdir().unwrap();
        let r = ShareRegistry::new(vec![]);

        // 初始为空
        assert_eq!(r.list().len(), 0);

        // 添加共享区
        let root = tmp.path().join("share");
        fs::create_dir_all(&root).unwrap();
        r.add(ShareDef {
            id: "s1".into(),
            alias: "分享1".into(),
            path: root.clone(),
        });

        assert_eq!(r.list().len(), 1);
        assert_eq!(r.list()[0].id, "s1");

        // 添加第二个
        r.add(ShareDef {
            id: "s2".into(),
            alias: "分享2".into(),
            path: root.clone(),
        });

        assert_eq!(r.list().len(), 2);

        // 移除一个
        assert!(r.remove("s1"), "应成功移除 s1");
        assert_eq!(r.list().len(), 1);
        assert_eq!(r.list()[0].id, "s2");

        // 移除不存在的
        assert!(!r.remove("s3"), "移除不存在的应返回 false");
        assert_eq!(r.list().len(), 1, "移除不存在的不应改变列表");

        // 覆盖同名 ID
        r.add(ShareDef {
            id: "s2".into(),
            alias: "新分享2".into(),
            path: root.clone(),
        });
        assert_eq!(r.list().len(), 1, "覆盖同名 ID 不应增加数量");
        assert_eq!(r.list()[0].alias, "新分享2");
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
        use std::process::Command;

        let tmp = tempdir().unwrap();
        let root = tmp.path().join("share");
        fs::create_dir_all(&root).unwrap();

        let link = root.join("junction");
        let target = r"C:\Windows";

        // Try to create a junction; skip test if it fails due to permissions
        let result = Command::new("cmd")
            .args(["/c", "mklink", "/J", link.to_str().unwrap(), target])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    let r = ShareRegistry::new(vec![ShareDef {
                        id: "s1".into(),
                        alias: "分享".into(),
                        path: root
                    }]);

                    // Resolving the junction should be rejected as IllegalPath
                    assert!(matches!(r.resolve("s1", "junction"), Err(ShareError::IllegalPath)));

                    // Clean up junction
                    let _ = fs::remove_file(&link);
                } else {
                    eprintln!("mklink failed (exit code: {:?}), skipping junction test", output.status.code());
                }
            }
            Err(e) => {
                eprintln!("Failed to execute mklink: {}, skipping junction test", e);
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn deep_junction_escape_rejected() {
        use std::process::Command;

        let tmp = tempdir().unwrap();
        let root = tmp.path().join("share");
        fs::create_dir_all(root.join("sub")).unwrap();

        let link = root.join("sub").join("link");
        let target = r"C:\Windows";

        // Try to create a junction in subdirectory; skip test if it fails
        let result = Command::new("cmd")
            .args(["/c", "mklink", "/J", link.to_str().unwrap(), target])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    let r = ShareRegistry::new(vec![ShareDef {
                        id: "s1".into(),
                        alias: "分享".into(),
                        path: root
                    }]);

                    // Resolving the deep junction should be rejected as IllegalPath
                    assert!(matches!(r.resolve("s1", "sub/link"), Err(ShareError::IllegalPath)));

                    // Clean up junction
                    let _ = fs::remove_file(&link);
                } else {
                    eprintln!("mklink failed (exit code: {:?}), skipping deep junction test", output.status.code());
                }
            }
            Err(e) => {
                eprintln!("Failed to execute mklink: {}, skipping deep junction test", e);
            }
        }
    }

    #[tokio::test]
    async fn list_dir_paginates_sorted() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("share");
        fs::create_dir_all(&root).unwrap();

        // Create files in reverse order to test sorting
        fs::write(root.join("z.txt"), b"last").unwrap();
        fs::write(root.join("a.txt"), b"first").unwrap();
        fs::write(root.join("m.txt"), b"middle").unwrap();

        let r = ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "分享".into(),
            path: root
        }]);

        // First page with limit=2
        let (page1, next1) = r.list_dir("s1", "", 0, 2).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].name, "a.txt");
        assert_eq!(page1[1].name, "m.txt");
        assert_eq!(next1, Some(2));

        // Check FileEntry fields
        assert_eq!(page1[0].is_dir, false);
        assert_eq!(page1[0].size, 5); // "first" = 5 bytes

        // Second page with cursor=2
        let (page2, next2) = r.list_dir("s1", "", 2, 2).await.unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].name, "z.txt");
        assert_eq!(next2, None); // No more entries
    }

    // v0.6.0 文件操作测试
    fn test_reg() -> (ShareRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("share")).unwrap();
        std::fs::write(dir.path().join("share/a.txt"), "hello").unwrap();
        let reg = ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "默认共享".into(),
            path: dir.path().join("share"),
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
            ControlMsg::ShareRename { share_id: "s1".into(), path: "a.txt".into(), new_name: "b.txt".into(), msg_id: 7 },
            ControlMsg::ShareDelete { share_id: "s1".into(), path: "a.txt".into(), msg_id: 8 },
            ControlMsg::ShareMkdir { share_id: "s1".into(), path: "x/y".into(), msg_id: 9 },
            ControlMsg::ShareOpResult { ok: true, error: None, msg_id: 10 },
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
        let json = serde_json::to_string(&crate::protocol::ControlMsg::ShareOpResult { ok: true, error: None, msg_id: 0 }).unwrap();
        assert!(!json.contains("error"));
        let back: crate::protocol::ControlMsg = serde_json::from_str("{\"type\":\"share_op_result\",\"ok\":true}").unwrap();
        assert!(matches!(back, crate::protocol::ControlMsg::ShareOpResult { ok: true, error: None, msg_id: 0 }));
    }
}
