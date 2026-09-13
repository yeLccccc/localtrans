// 共享目录 watchdog：周期扫描每个共享区根目录的内容指纹，
// 检测到变化时通过回调通知（Tauri 壳据此 emit 前端事件 + 推送
// SharesChanged 控制消息给已连接的对端）。
//
// 设计取舍：不引入 notify/inotify/ReadDirectoryChangesW 等文件系统
// 事件依赖——共享区通常文件数有限（千级），2s 一次浅扫（仅根目录一层
// 的 name/size/mtime 哈希）开销可忽略，换来零平台差异与零 FFI 复杂度。
// 子目录变化不单独通知：浏览方收到根指纹变化后按需重新拉列表，
// ListResp 本身就是实时读目录的。

use crate::share::ShareRegistry;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// 目录内容指纹：条目 (name, is_dir, size, mtime) 的 FNV-1a 汇总。
/// 不追求密码学强度——只用来判断"变没变"
fn dir_fingerprint(dir: &Path) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut mut_fnv = |bytes: &[u8]| {
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    let Ok(entries) = std::fs::read_dir(dir) else {
        // 读不了（目录被删/权限）：给个常量但区别于空的指纹，
        // 让"存在→读不到"也算变化
        return u64::MAX;
    };

    // 排序保证同一目录内容的指纹稳定（read_dir 顺序不保证）
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // 与 ShareRegistry::list_dir 的可见性规则对齐：跳过隐藏项
        if name.starts_with('.') {
            continue;
        }
        #[cfg(windows)]
        {
            if let Ok(md) = entry.metadata() {
                use std::os::windows::fs::MetadataExt;
                let attrs = md.file_attributes();
                if attrs & 0x02 != 0 || attrs & 0x04 != 0 {
                    continue;
                }
            }
        }
        names.push(name);
    }
    names.sort();

    for name in names {
        mut_fnv(name.as_bytes());
        let full = dir.join(&name);
        let Ok(md) = std::fs::metadata(&full) else {
            mut_fnv(b"?"); // 元数据读不到也计入指纹
            continue;
        };
        mut_fnv(if md.is_dir() { b"d" } else { b"f" });
        mut_fnv(&md.len().to_le_bytes());
        if let Ok(mt) = md.modified() {
            if let Ok(secs) = mt.duration_since(std::time::UNIX_EPOCH) {
                mut_fnv(&secs.as_secs().to_le_bytes());
            }
        }
    }
    hash
}

/// 启动 watchdog。返回的 JoinHandle 一般直接丢弃（随进程退出）。
///
/// `on_change(share_id)` 在检测到某共享区根目录指纹变化时同步调用，
/// 调用方负责去抖（这里每个 share 独立比对，天然不会重复触发同一状态）。
///
/// v0.2.9 CPU 优化：空闲指数退避——连续 N 轮无变化后扫描间隔逐级翻倍
/// （2s→4s→…→上限 30s），任一共享区变化立即回到基础间隔。常驻挂机时
/// 目录扫描系统调用次数降到 1/15，变化检测的响应延迟仍 ≤30s（浏览页
/// 还有手动刷新兜底）。另：reg.list() 每轮只调一次（此前循环里调两次）。
pub fn spawn_share_watcher<F>(reg: ShareRegistry, base_period: Duration, mut on_change: F)
where
    F: FnMut(String) + Send + 'static,
{
    tokio::spawn(async move {
        const MAX_BACKOFF_MULT: u32 = 15; // 2s * 15 = 30s 上限
        let mut last: HashMap<String, u64> = HashMap::new();
        let mut idle_rounds: u32 = 0;
        let mut period = base_period;
        let mut interval = tokio::time::interval(period);
        interval.tick().await; // 首个 tick 立即返回，用于建立基线

        loop {
            interval.tick().await;
            let defs = reg.list();
            // 低危审计修复:目录扫描是文件系统 IO,下放阻塞线程池,
            // 大目录(慢盘/网络盘)不再卡 runtime worker
            let paths: Vec<(String, std::path::PathBuf)> =
                defs.iter().map(|d| (d.id.clone(), d.path.clone())).collect();
            let fps = tokio::task::spawn_blocking(move || {
                paths.into_iter()
                    .map(|(id, p)| (id, dir_fingerprint(&p)))
                    .collect::<Vec<_>>()
            }).await.unwrap_or_default();
            let mut changed_any = false;
            for def in &defs {
                let Some((_, fp)) = fps.iter().find(|(id, _)| id == &def.id) else {
                    continue;
                };
                let fp = *fp;
                match last.get(&def.id) {
                    Some(&prev) if prev == fp => {}
                    Some(_) => {
                        tracing::debug!("共享区变化: {} ({})", def.alias, def.id);
                        on_change(def.id.clone());
                        changed_any = true;
                    }
                    None => {} // 首轮基线，不通知
                }
                last.insert(def.id.clone(), fp);
            }
            // 已移除的共享区清掉基线，防泄漏
            if last.len() > defs.len() {
                let alive: std::collections::HashSet<String> =
                    defs.iter().map(|d| d.id.clone()).collect();
                last.retain(|id, _| alive.contains(id));
            }

            // 指数退避：无变化翻倍（封顶），有变化归位
            if changed_any {
                idle_rounds = 0;
                if period != base_period {
                    period = base_period;
                    interval = tokio::time::interval(period);
                }
            } else {
                idle_rounds = idle_rounds.saturating_add(1);
                let mult = 1u32 << idle_rounds.min(4); // 2,4,8,16 封顶
                let new_period = (base_period * mult).min(base_period * MAX_BACKOFF_MULT);
                if new_period != period {
                    period = new_period;
                    interval = tokio::time::interval(period);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ShareDef;

    #[test]
    fn fingerprint_changes_on_file_add() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("share");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();

        let fp1 = dir_fingerprint(&root);
        assert_ne!(fp1, 0);

        std::fs::write(root.join("b.txt"), b"ho").unwrap();
        let fp2 = dir_fingerprint(&root);
        assert_ne!(fp1, fp2);

        // 内容稳定 → 指纹稳定
        assert_eq!(fp2, dir_fingerprint(&root));
    }

    #[test]
    fn fingerprint_order_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("share");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("x.txt"), b"1").unwrap();
        std::fs::write(root.join("y.txt"), b"2").unwrap();
        let fp1 = dir_fingerprint(&root);

        // 重建同名目录（写入顺序不同），内容一致 → 指纹一致
        let root2 = tmp.path().join("share2");
        std::fs::create_dir_all(&root2).unwrap();
        std::fs::write(root2.join("y.txt"), b"2").unwrap();
        std::fs::write(root2.join("x.txt"), b"1").unwrap();
        assert_eq!(fp1, dir_fingerprint(&root2));
    }

    #[tokio::test]
    async fn watcher_notifies_on_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("share");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"hi").unwrap();

        let reg = ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "测试".into(),
            path: root.clone(),
        }]);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        spawn_share_watcher(reg, Duration::from_millis(50), move |id| {
            let _ = tx.send(id);
        });

        // 等两轮建立基线
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(rx.try_recv().is_err(), "基线阶段不应通知");

        std::fs::write(root.join("new.txt"), b"new").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("应在变化后收到通知")
            .expect("通道不应关闭");
        assert_eq!(got, "s1");
    }
}
