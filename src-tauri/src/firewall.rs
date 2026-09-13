// Copyright (c) 2024 LocalTrans contributors.
// Windows 防火墙规则与网络状态体检

use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// GUI 子进程不弹控制台黑窗（CREATE_NO_WINDOW）。
/// 应用是 windows 子系统，spawn powershell/cmd/reg 默认会闪黑色窗口——
/// 设备页/设置页每次加载都会体检，导致反复闪框。
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 构造防火墙参数字符串（纯函数，用于测试）
pub fn firewall_args() -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        "name=\"LocalTrans\"".into(),
        "dir=in".into(),
        "action=allow".into(),
        "protocol=UDP".into(),
        "localport=47600-47601".into(),
    ]
}

/// 添加防火墙规则（触发 UAC）
/// v0.11.0 M-C7:powershell 子进程阻塞调用——下放阻塞线程池
pub async fn add_rule() -> Result<String, String> {
    tokio::task::spawn_blocking(add_rule_blocking)
        .await
        .map_err(|e| format!("防火墙任务失败: {}", e))?
}

fn add_rule_blocking() -> Result<String, String> {
    let args = firewall_args();

    let result = Command::new("powershell")
        .args(["-Command", &format!("Start-Process netsh -ArgumentList '{}' -Verb RunAs",
            args.join(" ").replace('"', r#"\""#))])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                Ok("防火墙规则添加成功".into())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // UAC 取消在中文系统报"用户取消操作"，英文系统报
                // "This operation was canceled by the user."
                if stderr.contains("用户取消操作")
                    || stderr.contains("user cancel")
                    || stderr.contains("canceled by the user")
                    || stderr.contains("cancelled by the user")
                {
                    Ok("用户取消了防火墙规则添加".into())
                } else {
                    Err(format!("添加防火墙规则失败: {}", stderr))
                }
            }
        }
        Err(e) => Err(format!("执行命令失败: {}", e))
    }
}

/// 解析 `netsh ... show rule` 的输出（纯函数，用于测试）
///
/// 输出随系统语言变化，调用侧已 chcp 65001 强制 UTF-8；
/// 这里去掉全部空白后做中英文双语匹配。
fn parse_rule_output(text: &str) -> (bool, bool) {
    let norm: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let not_found = norm.contains("没有与给定标准匹配的规则")
        || norm.contains("Norulesmatch");
    let exists = !not_found && norm.contains("LocalTrans");
    // 中文系统 "已启用: 是" / 英文系统 "Enabled: Yes"（冒号兼容全角）
    let enabled = exists && (norm.contains("已启用:是")
        || norm.contains("已启用：是")
        || norm.contains("Enabled:Yes"));
    (exists, enabled)
}

/// 查询 LocalTrans 入站规则状态（只读，不需要管理员）
/// 返回 (规则存在, 规则启用)。异步包装下放阻塞线程池(M-C7)。
pub async fn rule_status_async() -> (bool, bool) {
    tokio::task::spawn_blocking(rule_status).await.unwrap_or((false, false))
}

pub fn rule_status() -> (bool, bool) {
    let out = Command::new("cmd")
        .args(["/C", "chcp 65001 >NUL & netsh advfirewall firewall show rule name=LocalTrans"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) => {
            let text = format!("{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr));
            parse_rule_output(&text)
        }
        Err(_) => (false, false),
    }
}

/// 解析 `reg query ... /v EnableFirewall` 输出中的 0x0/0x1（纯函数）
fn parse_reg_state(text: &str) -> Option<bool> {
    // 输出形如 "    EnableFirewall    REG_DWORD    0x1"
    text.split_whitespace()
        .find(|t| t.starts_with("0x"))
        .map(|t| t != "0x0")
}

/// 防火墙三个配置文件的开关状态（注册表 reg 子进程，输出与系统语言无关）
/// 返回顺序 [域, 专用, 公用]，true = 该配置文件下防火墙启用。
/// 异步包装下放阻塞线程池(M-C7)。
pub async fn profile_states_async() -> [bool; 3] {
    tokio::task::spawn_blocking(profile_states).await.unwrap_or([true; 3])
}

pub fn profile_states() -> [bool; 3] {
    const BASE: &str = r"HKLM\SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy";
    const SUBKEYS: [&str; 3] = ["DomainProfile", "StandardProfile", "PublicProfile"];

    let mut out = [true; 3]; // 查询失败按"启用"处理，宁可多提醒
    for (i, sub) in SUBKEYS.iter().enumerate() {
        if let Ok(o) = Command::new("reg")
            .args(["query", &format!(r"{}\{}", BASE, sub), "/v", "EnableFirewall"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(v) = parse_reg_state(&text) {
                out[i] = v;
            }
        }
    }
    out
}

/// 本机主网络 IP（UDP connect 只做本地选路，不会真正发包）
pub fn primary_local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_firewall_args() {
        let args = firewall_args();
        assert_eq!(args.len(), 9);
        assert_eq!(args[0], "advfirewall");
        assert_eq!(args[8], "localport=47600-47601");

        let cmd = args.join(" ");
        assert!(cmd.contains("name=\"LocalTrans\""));
        assert!(cmd.contains("dir=in"));
        assert!(cmd.contains("action=allow"));
        assert!(cmd.contains("protocol=UDP"));
        assert!(cmd.contains("localport=47600-47601"));
    }

    #[test]
    fn parse_rule_output_chinese() {
        // 规则存在且启用（中文系统的典型输出，字段值间距不定）
        let zh_enabled = "\u{5df2}\u{542f}\u{7528}:                                 \u{662f}\r\n\
            \u{89c4}\u{5219}\u{540d}\u{79f0}:                 LocalTrans\r\n\
            \u{672c}\u{5730}\u{7aef}\u{53e3}:               47600-47601";
        assert_eq!(parse_rule_output(zh_enabled), (true, true));

        // 规则不存在（中文）
        let zh_none = "\u{6ca1}\u{6709}\u{4e0e}\u{7ed9}\u{5b9a}\u{6807}\u{51c6}\u{5339}\u{914d}\u{7684}\u{89c4}\u{5219}\u{3002}";
        assert_eq!(parse_rule_output(zh_none), (false, false));

        // 规则存在但被禁用（值 = 否）
        let zh_disabled = "\u{89c4}\u{5219}\u{540d}\u{79f0}: LocalTrans\r\n\u{5df2}\u{542f}\u{7528}: \u{5426}";
        assert_eq!(parse_rule_output(zh_disabled), (true, false));
    }

    #[test]
    fn parse_rule_output_english() {
        let en_enabled = "Rule Name:                LocalTrans\r\n\
            Enabled:                  Yes\r\n\
            Direction:                In";
        assert_eq!(parse_rule_output(en_enabled), (true, true));

        let en_none = "No rules match the specified criteria.";
        assert_eq!(parse_rule_output(en_none), (false, false));
    }

    #[test]
    fn parse_reg_state_values() {
        assert_eq!(parse_reg_state("    EnableFirewall    REG_DWORD    0x1"), Some(true));
        assert_eq!(parse_reg_state("    EnableFirewall    REG_DWORD    0x0"), Some(false));
        assert_eq!(parse_reg_state(""), None);
    }
}
