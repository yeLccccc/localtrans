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
    #[serde(default)]
    pub relay_enabled: bool,
    #[serde(default)]
    pub relay_server: String,
    #[serde(default)]
    pub relay_psk: String,
    #[serde(default = "default_consent_timeout_secs")]
    pub consent_timeout_secs: u64,
    #[serde(default = "default_offer_timeout_secs")]
    pub offer_timeout_secs: u64,
    /// Task 7:全局并发闸门容量(同对端仍串行;范围钳制 1-8 在壳层设置入口)
    #[serde(default = "default_max_active_transfers")]
    pub max_active_transfers: u32,
    /// M3c T3 强制走中继:per 设备持久化开关(指纹 hex → bool)。
    /// 值缺失按 false;开启时 connect 命令层跳过评分决策直选中继路径
    /// (排障后门,评分/切换 core 语义不动)。PC 壳与 ffi 壳共用本配置。
    #[serde(default)]
    pub force_relay_map: std::collections::HashMap<String, bool>,
}

fn default_max_active_transfers() -> u32 {
    3
}

fn default_consent_timeout_secs() -> u64 {
    60
}

fn default_offer_timeout_secs() -> u64 {
    60
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
            // T1:默认端口统一走 ports 模块(设 LOCALTRANS_TEST_PORT_BASE 后整体偏移)
            quic_port: crate::ports::quic_port(),
            discovery_port: crate::ports::discovery_port(),
            shares: vec![],
            relay_enabled: false,
            relay_server: String::new(),
            relay_psk: String::new(),
            consent_timeout_secs: 60,
            offer_timeout_secs: 60,
            max_active_transfers: 3,
            force_relay_map: std::collections::HashMap::new(),
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

/// 出厂配置:在 Default 基础上默认初始化 downloads 与 share 两个目录并落盘。
/// 基准取 data 目录的父目录——PC 便携版即 exe 目录,Android FFI 即 filesDir(均可写)。
/// 只在首启(config.json 不存在)与损坏重置时调用;存量配置即便 shares 为空也不补,
/// 不覆盖用户已做的选择。
fn fresh_config(dir: &std::path::Path) -> Config {
    let base = dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let mut c = Config::default();
    c.download_dir = base.join("downloads");
    let share_path = base.join("share");
    if let Err(e) = std::fs::create_dir_all(&c.download_dir) {
        tracing::warn!("默认下载目录创建失败({e}): {}", c.download_dir.display());
    }
    if let Err(e) = std::fs::create_dir_all(&share_path) {
        tracing::warn!("默认共享目录创建失败({e}): {}", share_path.display());
    }
    c.shares = vec![ShareDef {
        // 16 hex 随机 ID,与壳层 add_share 同款约定
        id: hex::encode(rand::random::<[u8; 8]>()),
        alias: "共享文件夹".into(),
        path: share_path,
    }];
    let _ = save_config(dir, &c);
    c
}

pub fn load_config(dir: &std::path::Path) -> Config {
    let path = dir.join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => load_config_from_reader(&s).unwrap_or_else(|()| {
            // M-B2: JSON 解析失败——先把损坏文件改名备份,再重置落盘
            tracing::warn!("config 损坏,备份后重置为默认");
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = dir.join(format!("config.json.corrupt-{ts}"));
            if std::fs::rename(&path, &backup).is_err() {
                tracing::error!("config 损坏文件备份失败(可能被占用),跳过重置不覆盖原文件");
                return Config::default();
            }
            fresh_config(dir)
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fresh_config(dir)
        }
        // M-B2: 读错误(非 NotFound)不回写——避免用默认配置覆盖真实配置
        Err(e) => {
            tracing::warn!("config 读取失败({e}),使用内存默认但不落盘");
            Config::default()
        }
    }
}

/// 纯解析函数,便于单测三分支
pub(crate) fn load_config_from_reader(s: &str) -> Result<Config, ()> {
    serde_json::from_str(s).map_err(|_| ())
}

/// 纯函数三分支判定,便于单测(不经真实文件系统)
#[cfg(test)]
pub(crate) fn classify_read_error(kind: io::ErrorKind) -> bool {
    kind == io::ErrorKind::NotFound
}

pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|s| s.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
        .or_else(|_| std::fs::copy(&tmp, path).map(|_| ()))?;
    Ok(())
}

pub fn save_config(dir: &std::path::Path, cfg: &Config) -> io::Result<()> {
    let json = serde_json::to_string_pretty(cfg).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write(&dir.join("config.json"), json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn first_run_initializes_default_share_and_downloads() {
        // BUG07:首启(config.json 不存在)默认初始化 share/downloads 并建目录
        let tmp = tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let cfg = load_config(&data);
        // 两个路径都以 data 父目录为基准(便携语义 = exe 目录)
        assert_eq!(cfg.download_dir, tmp.path().join("downloads"));
        assert_eq!(cfg.shares.len(), 1);
        assert_eq!(cfg.shares[0].alias, "共享文件夹");
        assert_eq!(cfg.shares[0].path, tmp.path().join("share"));
        // 目录真实创建 + 配置落盘
        assert!(cfg.download_dir.is_dir());
        assert!(cfg.shares[0].path.is_dir());
        assert!(data.join("config.json").exists());
        // id 为 16 hex(与壳层 add_share 同款约定)
        assert_eq!(cfg.shares[0].id.len(), 16);
    }

    #[test]
    fn existing_config_not_reinitialized() {
        // 存量配置即便 shares 为空也不补默认 share——不覆盖用户选择
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.json"),
            r#"{"device_name":"n","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#,
        ).unwrap();
        let cfg = load_config(tmp.path());
        assert!(cfg.shares.is_empty());
    }

    #[test]
    fn corrupted_reset_also_initializes_default_dirs() {
        // 损坏重置 = 回到出厂,同样带默认 share/downloads
        // (基准 = data 父目录,故此处模拟 data/ 子目录结构)
        let tmp = tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("config.json"), "{bad").unwrap();
        let cfg = load_config(&data);
        assert_eq!(cfg.shares.len(), 1);
        assert_eq!(cfg.download_dir, tmp.path().join("downloads"));
    }

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
        assert_eq!(loaded.quic_port, crate::ports::quic_port());
        assert_eq!(loaded.discovery_port, crate::ports::discovery_port());
    }

    #[test]
    fn corrupted_config_falls_back_to_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{invalid json").unwrap();
        let cfg = load_config(dir.path());
        assert_eq!(cfg.quic_port, crate::ports::quic_port()); // 默认值
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

    #[test]
    fn config_relay_fields_roundtrip() {
        let dir = tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.relay_enabled = true;
        cfg.relay_server = "1.2.3.4:9443".into();
        cfg.relay_psk = "secret".into();
        save_config(dir.path(), &cfg).unwrap();
        let loaded = load_config(dir.path());
        assert!(loaded.relay_enabled);
        assert_eq!(loaded.relay_server, "1.2.3.4:9443");
        assert_eq!(loaded.relay_psk, "secret");
    }

    #[test]
    fn config_offer_timeout_roundtrip_and_default() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        assert_eq!(cfg.offer_timeout_secs, 60, "默认 60s");

        let mut cfg2 = Config::default();
        cfg2.offer_timeout_secs = 120;
        save_config(dir.path(), &cfg2).unwrap();
        let loaded = load_config(dir.path());
        assert_eq!(loaded.offer_timeout_secs, 120);
    }

    #[test]
    fn config_offer_timeout_old_file_defaults() {
        // 老配置文件没有该字段 → serde default 兜底 60
        let dir = tempdir().unwrap();
        let old = r#"{"device_name":"n","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#;
        std::fs::write(dir.path().join("config.json"), old).unwrap();
        let cfg = load_config(dir.path());
        assert_eq!(cfg.offer_timeout_secs, 60);
    }

    #[test]
    fn force_relay_map_roundtrip_and_old_file_default() {
        // M3c T3:force_relay_map 持久化往返 + 老配置缺字段 default 兜底(空表)
        let dir = tempdir().unwrap();
        let mut cfg = Config::default();
        assert!(cfg.force_relay_map.is_empty(), "默认空表");
        cfg.force_relay_map.insert("aabb".into(), true);
        cfg.force_relay_map.insert("ccdd".into(), false);
        save_config(dir.path(), &cfg).unwrap();
        let loaded = load_config(dir.path());
        assert_eq!(loaded.force_relay_map.get("aabb"), Some(&true));
        assert_eq!(loaded.force_relay_map.get("ccdd"), Some(&false));

        // 老配置文件无该字段 → serde default 空表(不破坏存量配置读取)
        let dir2 = tempdir().unwrap();
        let old = r#"{"device_name":"n","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#;
        std::fs::write(dir2.path().join("config.json"), old).unwrap();
        let cfg2 = load_config(dir2.path());
        assert!(cfg2.force_relay_map.is_empty(), "老配置缺字段应 default 空表");
    }

    #[test]
    fn parse_failure_classifies_as_corrupt() {
        // M-B2 三分支纯函数判定:坏 JSON → 解析失败
        assert!(load_config_from_reader("{invalid").is_err());
        // 合法 JSON → Ok
        let ok = load_config_from_reader(
            r#"{"device_name":"n","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#,
        );
        assert!(ok.is_ok());
        // NotFound 归为"可落盘"类
        assert!(classify_read_error(io::ErrorKind::NotFound));
        assert!(!classify_read_error(io::ErrorKind::PermissionDenied));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_config_does_not_overwrite() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"device_name":"keep","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let cfg = load_config(dir.path());
        assert_eq!(cfg.device_name, Config::default().device_name); // 内存默认
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("keep"), "读错误时不得回写原文件");
    }

    #[test]
    fn corrupt_config_backed_up_then_reset() {
        // M-B2: JSON 损坏 → rename 备份 config.json.corrupt-<ts> + 重置落盘
        for i in 0..3 {
            let dir = tempdir().unwrap();
            let path = dir.path().join("config.json");
            let marker = format!("{{bad{i}}}");
            std::fs::write(&path, &marker).unwrap();
            let cfg = load_config(dir.path());
            assert_eq!(cfg.quic_port, crate::ports::quic_port());
            // 原路径已被合法文件替换
            assert!(serde_json::from_str::<Config>(&std::fs::read_to_string(&path).unwrap()).is_ok());
            // 备份存在且内容是原始损坏串
            let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("config.json.corrupt-"))
                .collect();
            assert_eq!(entries.len(), 1, "应恰好一个 corrupt 备份");
            assert_eq!(
                std::fs::read_to_string(dir.path().join(&entries[0])).unwrap(),
                marker,
                "备份保留损坏原文"
            );
            let _ = i;
        }
    }
}
