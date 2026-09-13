pub mod adapt;
pub mod dedup;
pub mod engine;
pub mod manifest;
pub mod sender_state;
pub mod source_probe;

use std::path::Path;
use crate::transfer::manifest::Manifest;

// T14: Re-exports for transfer engine
pub use adapt::AdaptiveStreams;
pub use manifest::{TransferMeta, HistoryEvent, patch_manifest_meta, append_history, load_history};
// T16: 传输引擎公共面（app 壳经 transfer:: 路径消费）
pub use engine::{
    control_task, start_pull, start_pull_into, push_files, push_files_rel, push_files_rel_cancellable, run_sender, spawn_rpc_router,
    set_inbound_recv_hook, list_remote_dir_recursive, start_pull_dir,
    ProgressEvent, SourceRole, OfferAsk, DeleteAsk, TaskControl, EngineError, PartWriter, BufferPool,
    DirPullEvent, RemoteDirListing,
    AutoOfferInfo, set_auto_offer_hook,
};

/// 扫描本地 .localtrans-parts 目录，返回所有待续传任务
/// 返回 (job_id, Manifest) 列表，用于 UI 启动时重连续传
pub fn pending_jobs(parts_root: &Path) -> Vec<(u64, Manifest)> {
    let parts_dir = parts_root.join(".localtrans-parts");
    let mut jobs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let manifest_path = path.join("manifest.json");
                if manifest_path.exists() {
                    if let Ok(manifest) = Manifest::load(&path) {
                        // 从目录名解析 job_id
                        if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                            if let Ok(job_id) = u64::from_str_radix(dir_name, 16) {
                                // 只返回未完成的任务（有缺失块）
                                if !manifest.missing_chunks().is_empty() {
                                    jobs.push((job_id, manifest));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    jobs
}

/// M-C2: .localtrans-parts 目录 GC(2026-08-30 收窄)。
/// 只删"位图全真"(所有块已收齐但未 finalize)的孤儿目录:直接删除让用户
/// 重新拉取(重试 finalize 需要对端会话与目标路径完整还原,复杂度高;
/// 收齐却没 finalize 说明 finalize 中途被打断,数据完整性存疑)。
/// 原"mtime 超 7 天盲删"已取消——parts 是用户数据,改为空间紧张时提示,
/// 不静默删除(spec §2.4)。
/// `now_epoch_secs` 参数保留以维持签名兼容;调用方应在 spawn_blocking 中执行。
/// 返回删除的目录数。
pub fn gc_stale_parts(parts_root: &Path, now_epoch_secs: u64) -> usize {
    let _ = now_epoch_secs;
    let parts_dir = parts_root.join(".localtrans-parts");
    let Ok(entries) = std::fs::read_dir(&parts_dir) else {
        return 0;
    };

    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // 目录名必须是合法 job_id 十六进制,否则不动(防误删未知结构)
        let is_job_dir = path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| u64::from_str_radix(s, 16).is_ok())
            .unwrap_or(false);
        if !is_job_dir {
            continue;
        }

        // 位图全真的"差一步 finalize"目录:直接删除(见函数注释取舍说明)
        let fully_received = Manifest::load(&path)
            .map(|m| m.missing_chunks().is_empty())
            .unwrap_or(false);
        if fully_received {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 孤儿任务全量扫描(历史记录入口的数据源)。
/// 与 `pending_jobs` 区别:不去掉位图全真的任务(全真孤儿也要列出,
/// 供用户手动 finalize/删除);manifest 加载失败或损坏的目录跳过。
#[derive(Clone)]
pub struct OrphanJob {
    pub job_id: u64,
    pub manifest: Manifest,
}

pub fn orphan_jobs(parts_root: &Path) -> Vec<OrphanJob> {
    let parts_dir = parts_root.join(".localtrans-parts");
    let mut jobs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(manifest) = Manifest::load(&path) {
                    if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                        if let Ok(job_id) = u64::from_str_radix(dir_name, 16) {
                            jobs.push(OrphanJob { job_id, manifest });
                        }
                    }
                }
            }
        }
    }

    jobs
}

/// P0-1: 推送落盘文件名净化(拒绝制为主、剔除为辅)。
/// 拒绝:空串 / 含分隔符与非法字符 / `.`/`..` / Windows 保留名 / 尾点尾空格 / 净化后超 255 字节。
/// 剔除:Unicode 控制字符与 bidi 控制符(剔除后为空则拒绝)。
/// 错误信息只含类别,不回显原始名(明文名不进日志规约)。
pub fn sanitize_file_name(name: &str) -> Result<String, EngineError> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    // 1) 剔除控制字符与 bidi 控制符(改写,不拒绝)
    let mut cleaned = String::with_capacity(name.len());
    for c in name.chars() {
        let is_ctrl = matches!(c as u32, 0x0000..=0x001F | 0x007F | 0x202A..=0x202E | 0x2066..=0x2069);
        if !is_ctrl {
            cleaned.push(c);
        }
    }

    // 2) 拒绝制校验(顺序即错误类别)
    if cleaned.is_empty() {
        return Err(EngineError::Protocol("非法文件名(为空)".into()));
    }
    if cleaned.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(EngineError::Protocol("非法文件名(含分隔符或非法字符)".into()));
    }
    if cleaned == "." || cleaned == ".." {
        return Err(EngineError::Protocol("非法文件名(穿越目录)".into()));
    }
    let stem_upper = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem_upper.as_str()) {
        return Err(EngineError::Protocol("非法文件名(Windows 保留名)".into()));
    }
    if cleaned.ends_with('.') || cleaned.ends_with(' ') {
        return Err(EngineError::Protocol("非法文件名(尾点或尾空格)".into()));
    }
    if cleaned.len() > 255 {
        return Err(EngineError::Protocol("非法文件名(超过 255 字节)".into()));
    }
    Ok(cleaned)
}

#[cfg(test)]
mod sanitize_file_name_tests {
    use super::sanitize_file_name;

    #[test]
    fn normal_names_pass_untouched() {
        assert_eq!(sanitize_file_name("报告.pdf").unwrap(), "报告.pdf");
        assert_eq!(sanitize_file_name("movie (2026) [1080p].mkv").unwrap(), "movie (2026) [1080p].mkv");
        assert_eq!(sanitize_file_name("照片🎉.png").unwrap(), "照片🎉.png");
        assert_eq!(sanitize_file_name("a.b.c.txt").unwrap(), "a.b.c.txt");
    }

    #[test]
    fn empty_rejected() {
        assert!(sanitize_file_name("").is_err());
    }

    #[test]
    fn separators_rejected() {
        for bad in ["a/b", "a\\b", "C:\\Users\\x\\evil.bat", "\\\\?\\C:\\x", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "..\\..\\evil.dll"] {
            assert!(sanitize_file_name(bad).is_err(), "应拒绝: {:?}", bad);
        }
    }

    #[test]
    fn dot_names_rejected() {
        assert!(sanitize_file_name(".").is_err());
        assert!(sanitize_file_name("..").is_err());
    }

    #[test]
    fn windows_reserved_rejected() {
        for bad in ["CON", "con", "NUL.txt", "com1", "LPT1.log", "PRN", "aux", "COM9.tar.gz"] {
            assert!(sanitize_file_name(bad).is_err(), "应拒绝保留名: {:?}", bad);
        }
    }

    #[test]
    fn trailing_dot_or_space_rejected() {
        assert!(sanitize_file_name("evil.exe.").is_err());
        assert!(sanitize_file_name("evil.exe ").is_err());
    }

    #[test]
    fn too_long_rejected() {
        let long = "a".repeat(256);
        assert!(sanitize_file_name(&long).is_err());
        let ok = "a".repeat(255);
        assert!(sanitize_file_name(&ok).is_ok());
    }

    #[test]
    fn control_chars_stripped_not_rejected() {
        assert_eq!(sanitize_file_name("a\u{0000}b.txt").unwrap(), "ab.txt");
        assert_eq!(sanitize_file_name("a\u{007F}b").unwrap(), "ab");
        // 剔除后为空 → 拒绝
        assert!(sanitize_file_name("\u{0001}").is_err());
    }

    #[test]
    fn bidi_controls_stripped() {
        // U+202E RTL override 伪装扩展名:剔除后是正常名,不拒绝
        assert_eq!(sanitize_file_name("fdp\u{202E}exe.pdf").unwrap(), "fdpexe.pdf");
        assert_eq!(sanitize_file_name("a\u{2066}b.txt").unwrap(), "ab.txt");
    }

    #[test]
    fn error_message_contains_no_original_name() {
        let err = sanitize_file_name("..\\..\\secret\\evil.dll").unwrap_err();
        let msg = format!("{:?}", err);
        assert!(!msg.contains("secret"), "错误信息不得回显原始文件名: {}", msg);
    }
}

#[cfg(test)]
mod gc_parts_tests {
    use super::*;
    use tempfile::tempdir;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn set_mtime_old(path: &std::path::Path) {
        let old = now() - 8 * 24 * 3600; // 8 天前
        let t = filetime::FileTime::from_unix_time(old as i64, 0);
        filetime::set_file_mtime(path, t).unwrap();
        // Windows: 目录 mtime 也要单独设置
        #[cfg(windows)]
        {
            // set_file_mtime 对目录同样生效,无需额外处理
            let _ = path;
        }
    }

    #[test]
    fn stale_over_7_days_now_survives() {
        // 2026-08-30 定案:取消 7 天盲删——parts 是用户数据,只能显式删
        let tmp = tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000042");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("000.part"), b"x").unwrap();
        set_mtime_old(&job);
        assert_eq!(gc_stale_parts(tmp.path(), now()), 0);
        assert!(job.exists(), "超 7 天不再盲删");
    }

    #[test]
    fn fresh_job_dir_survives() {
        let tmp = tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000007");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("000.part"), b"x").unwrap();

        let removed = gc_stale_parts(tmp.path(), now());
        assert_eq!(removed, 0);
        assert!(job.exists(), "新目录不应被清理");
    }

    #[test]
    fn non_hex_dirs_untouched() {
        let tmp = tempdir().unwrap();
        let weird = tmp.path().join(".localtrans-parts/not-a-job");
        std::fs::create_dir_all(&weird).unwrap();
        set_mtime_old(&weird);

        let removed = gc_stale_parts(tmp.path(), now());
        assert_eq!(removed, 0);
        assert!(weird.exists(), "非法目录名不应被误删");
    }

    #[test]
    fn fully_received_orphan_removed_regardless_of_age() {
        // 位图全真(差一步 finalize)的目录直接删——数据完整性存疑,
        // 让用户重新拉取比静默续传不可信数据更安全
        let tmp = tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000100");
        std::fs::create_dir_all(&job).unwrap();
        // 构造单块全收的 manifest:hash 须匹配才算 received=true
        use sha2::{Digest, Sha256};
        let data = b"hello";
        let hash = hex::encode(Sha256::digest(data));
        let manifest_json = format!(
            r#"{{"file_name":"a.bin","total_size":5,"chunk_hashes":["{}"],"received":[true],"peer":null}}"#,
            hash
        );
        std::fs::write(job.join("manifest.json"), manifest_json).unwrap();

        let removed = gc_stale_parts(tmp.path(), now());
        assert_eq!(removed, 1);
        assert!(!job.exists(), "位图全真孤儿应被删除");
    }

    #[test]
    fn orphan_jobs_lists_all_incl_fully_received() {
        use crate::transfer::orphan_jobs;
        let tmp = tempdir().unwrap();
        let root = tmp.path().join(".localtrans-parts");
        // 任务 A:半收(缺块)
        let a = root.join("0000000000000001");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("manifest.json"),
            r#"{"file_name":"a.bin","total_size":5,"chunk_hashes":["00"],"received":[false]}"#).unwrap();
        // 任务 B:位图全真(差一步 finalize)
        let b = root.join("0000000000000002");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("manifest.json"),
            r#"{"file_name":"b.bin","total_size":5,"chunk_hashes":["00"],"received":[true]}"#).unwrap();
        let jobs = orphan_jobs(tmp.path());
        assert_eq!(jobs.len(), 2, "全真孤儿也要列出(历史记录入口)");
        assert!(jobs.iter().any(|j| j.job_id == 2));
    }
}
