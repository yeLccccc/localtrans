//! M3a FR5 连接记忆制——数据结构、持久化与退避纯函数（core 可测层）。
//!
//! 职责切分（计划卡 T4 定案）：**core 只管数据**——记忆条目的增删查、
//! `connect_memory.json` 落盘、退避序列/回落判定/抖动三组纯函数；
//! **编排（调度/退避循环/toast 时机）在壳层**（src-tauri `reconnect` 模块，
//! 有 discovery/session 全上下文）。Android FFI 接入属后续任务，本模块不出口 ffi。
//!
//! 存储选型：独立 `data/connect_memory.json`，不动 `trusted_peers.json`——
//! 信任表文件会被外部夹具（E2E seedTrust）整文件重写，附加字段会被静默丢弃；
//! 记忆独立成文件则与信任生命周期解耦（移除信任由壳层显式清记忆，
//! 见壳层 remove_trusted）。格式仿 probe_targets.json 的持久化模式。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type Fingerprint = crate::identity::Fingerprint;

/// 退避基数：第 1 次失败后等 2s
pub const BACKOFF_BASE_SECS: u64 = 2;
/// 退避封顶：≥5 次失败后恒 30s
pub const BACKOFF_CAP_SECS: u64 = 30;
/// 连续失败回落阈值：第 5 次失败后停止主动重试（spec FR5"连续 5 次失败回落停止"）
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;
/// 抖动幅度 ±20%
pub const JITTER_RATIO: f64 = 0.2;

/// 第 `failures` 次连续失败后的退避秒数（failures 从 1 计）：
/// 2/4/8/16 封顶 30——即 2/4/8/16/30/30/30…
pub fn backoff_secs(failures: u32) -> u64 {
    if failures == 0 {
        return BACKOFF_BASE_SECS;
    }
    BACKOFF_BASE_SECS
        .saturating_mul(1u64 << (failures - 1).min(31))
        .min(BACKOFF_CAP_SECS)
}

/// 连续失败 `failures` 次后是否应回落停止（不再主动重试）。
pub fn give_up(failures: u32) -> bool {
    failures >= MAX_CONSECUTIVE_FAILURES
}

/// ±20% 抖动：`r ∈ [0,1]` 映射到 base×[0.8, 1.2]，四舍五入，至少 1s。
/// r 越界按边界钳制（防调用方传负数/NaN 时产生离谱值）。
pub fn jitter_secs(base: u64, r: f64) -> u64 {
    let r = if r.is_finite() { r.clamp(0.0, 1.0) } else { 0.0 };
    let scaled = base as f64 * (1.0 - JITTER_RATIO + 2.0 * JITTER_RATIO * r);
    (scaled.round() as u64).max(1)
}

/// 单条连接记忆：用户主动连过（成功会话）的设备。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    /// 首次成功连接的 Unix 秒
    pub first_connected_at: u64,
    /// 最近一次成功连接的 Unix 秒
    pub last_connected_at: u64,
}

/// 落盘格式：指纹 hex（小写）→ 条目。BTreeMap 保证落盘键序稳定（diff 友好）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct MemoryFile {
    entries: BTreeMap<String, MemoryEntry>,
}

/// 连接记忆表（内存态 + data/connect_memory.json 落盘，写穿）。
pub struct ConnectMemory {
    entries: BTreeMap<String, MemoryEntry>,
    dir: PathBuf,
}

fn fp_key(fp: &Fingerprint) -> String {
    hex::encode(fp)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ConnectMemory {
    /// 从 data 目录加载；文件缺失/损坏 → 空表（记忆可丢，不影响正确性）。
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("connect_memory.json");
        let entries = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<MemoryFile>(&s)
                .map(|f| f.entries)
                .unwrap_or_else(|e| {
                    tracing::warn!("connect_memory.json 损坏({e}),重置为空");
                    BTreeMap::new()
                }),
            Err(_) => BTreeMap::new(),
        };
        ConnectMemory { entries, dir: dir.to_path_buf() }
    }

    /// 落盘（原子写）。失败由调用方记 warn——记忆属尽力持久化，不阻断主流程。
    pub fn save(&self) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&MemoryFile { entries: self.entries.clone() })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        crate::store::atomic_write(&self.dir.join("connect_memory.json"), json.as_bytes())
    }

    /// 登记一次成功连接（当前时间）。返回 true 表示新设备入表，false 为既有条目续期。
    pub fn record(&mut self, fp: &Fingerprint) -> bool {
        self.record_at(fp, now_secs())
    }

    /// `record` 的时间可注入版（单测确定性用）。
    pub fn record_at(&mut self, fp: &Fingerprint, ts: u64) -> bool {
        let key = fp_key(fp);
        match self.entries.get_mut(&key) {
            Some(e) => {
                e.last_connected_at = ts;
                false
            }
            None => {
                self.entries.insert(key, MemoryEntry { first_connected_at: ts, last_connected_at: ts });
                true
            }
        }
    }

    /// 清除一条记忆（移除信任时由壳层调用）。返回是否存在。
    pub fn remove(&mut self, fp: &Fingerprint) -> bool {
        self.entries.remove(&fp_key(fp)).is_some()
    }

    pub fn contains(&self, fp: &Fingerprint) -> bool {
        self.entries.contains_key(&fp_key(fp))
    }

    /// 全部已记忆指纹快照（启动扫描用）。
    pub fn remembered(&self) -> Vec<Fingerprint> {
        self.entries.keys().filter_map(|k| {
            hex::decode(k).ok().and_then(|b| <[u8; 32]>::try_from(b).ok())
        }).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fp(b: u8) -> Fingerprint {
        [b; 32]
    }

    // ===== 退避序列：2/4/8/16/30/30… =====

    #[test]
    fn backoff_sequence_doubling_capped() {
        let seq: Vec<u64> = (1..=7).map(backoff_secs).collect();
        assert_eq!(seq, vec![2, 4, 8, 16, 30, 30, 30], "退避序列 2/4/8/16 封顶 30");
        assert_eq!(backoff_secs(0), 2, "0 次失败按首退避处理（防御）");
        assert_eq!(backoff_secs(u32::MAX), 30, "极大入参不溢出");
    }

    // ===== 5 败回落判定 =====

    #[test]
    fn give_up_boundary_at_five() {
        assert!(!give_up(0));
        assert!(!give_up(1));
        assert!(!give_up(4), "前 4 次失败仍重试");
        assert!(give_up(5), "第 5 次失败回落停止");
        assert!(give_up(6));
        assert!(give_up(u32::MAX));
    }

    // ===== 抖动边界：±20% =====

    #[test]
    fn jitter_bounds_and_clamp() {
        assert_eq!(jitter_secs(30, 0.0), 24, "下界 30×0.8");
        assert_eq!(jitter_secs(30, 1.0), 36, "上界 30×1.2");
        assert_eq!(jitter_secs(10, 0.5), 10, "中点回原值");
        assert_eq!(jitter_secs(2, 0.0), 2, "小基数下界四舍五入(1.6→2)");
        assert_eq!(jitter_secs(2, 1.0), 2, "小基数上界四舍五入(2.4→2)");
        assert_eq!(jitter_secs(3, 0.0), 2, "3×0.8=2.4→2");
        assert_eq!(jitter_secs(3, 1.0), 4, "3×1.2=3.6→4");
        // 越界钳制 + 非法输入兜底
        assert_eq!(jitter_secs(30, -5.0), 24);
        assert_eq!(jitter_secs(30, 99.0), 36);
        assert_eq!(jitter_secs(30, f64::NAN), 24, "NaN 按下界处理");
        assert_eq!(jitter_secs(0, 0.5), 1, "至少 1s（防 0 延迟热轮询）");
    }

    // ===== 记忆持久化 roundtrip =====

    #[test]
    fn memory_record_remove_roundtrip() {
        let dir = tempdir().unwrap();
        let mut mem = ConnectMemory::load(dir.path());
        assert!(mem.is_empty());

        // 登记：新设备 true、重复 false（续期不改首连时间）
        assert!(mem.record_at(&fp(1), 1000));
        assert!(!mem.record_at(&fp(1), 2000));
        assert!(mem.contains(&fp(1)));
        assert_eq!(mem.len(), 1);
        assert!(mem.record_at(&fp(2), 1500));
        // 落盘 → 重载 → 全量还原
        mem.save().unwrap();
        let mut mem2 = ConnectMemory::load(dir.path());
        assert_eq!(mem2.len(), 2);
        assert!(mem2.contains(&fp(1)) && mem2.contains(&fp(2)));
        assert_eq!(mem2.remembered(), vec![fp(1), fp(2)], "指纹 hex 键序稳定还原");
        // 清除 → 落盘 → 重载不复活
        assert!(mem2.remove(&fp(1)));
        assert!(!mem2.remove(&fp(1)), "重复清除 false");
        mem2.save().unwrap();
        let mem3 = ConnectMemory::load(dir.path());
        assert!(!mem3.contains(&fp(1)));
        assert!(mem3.contains(&fp(2)));
    }

    #[test]
    fn memory_missing_and_corrupt_file_tolerated() {
        let dir = tempdir().unwrap();
        assert!(ConnectMemory::load(dir.path()).is_empty(), "文件缺失 → 空表");
        std::fs::write(dir.path().join("connect_memory.json"), "{bad json").unwrap();
        assert!(ConnectMemory::load(dir.path()).is_empty(), "损坏 → 空表不 panic");
    }

    #[test]
    fn memory_file_format_is_stable_json() {
        // 落盘格式钉死：顶层 {"entries": {"<64hex>": {...}}}——便于人工排查与工具兼容
        let dir = tempdir().unwrap();
        let mut mem = ConnectMemory::load(dir.path());
        mem.record_at(&fp(0xAB), 42);
        mem.save().unwrap();
        let text = std::fs::read_to_string(dir.path().join("connect_memory.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let key = hex::encode([0xAB; 32]);
        assert_eq!(v["entries"][&key]["firstConnectedAt"], serde_json::Value::Null, "蛇形字段名");
        assert_eq!(v["entries"][&key]["first_connected_at"], serde_json::json!(42));
        assert_eq!(v["entries"][&key]["last_connected_at"], serde_json::json!(42));
    }
}
