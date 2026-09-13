// 配对码:一次一随机生成 + 哈希常数时间比对
//
// 模型(v0.4.0 同意门):接受方 B 点"同意"瞬间随机生成 6 位码,只在 B 的
// 屏幕展示;发起方 A 人工读码输入,B 侧与哈希常数时间比对。码随连接即焚。
// 旧的对 SAS 对称派生(derive_sas_code)已废弃——对称模型下 A 界面必然
// 显示与 B 相同的码,违反"B 的码不出现在 A 界面"的硬约束。

use sha2::{Digest, Sha256};
use thiserror::Error;

/// 配对失败冷却时间(秒)
pub const COOLDOWN_SECS: u64 = 300;

/// 配对错误
#[derive(Error, Debug)]
pub enum PairingError {
    #[error("会话错误: {0}")]
    Session(#[from] crate::session::SessionError),
}

/// 随机生成 6 位十进制配对码(密码学随机,无偏采样,零填充)
pub fn generate_pair_code() -> String {
    use rand::Rng;
    let value: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{:06}", value)
}

/// 配对码的 SHA-256 哈希(core 内只存哈希,明文只在 B 的 UI 短暂存在)
pub fn hash_code(code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    hasher.finalize().into()
}

/// 常数时间比对:防止时序侧信道逐位猜码
pub fn verify_code(code: &str, hash: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    let candidate = hash_code(code);
    candidate.ct_eq(hash).into()
}

/// 配对状态机(仅接受方 B 使用:持有码哈希与失败计数)
pub struct PairingMachine {
    code_hash: [u8; 32],
    submitted_ok: bool,
    fails: u8,
}

impl PairingMachine {
    pub fn new(code_hash: [u8; 32]) -> Self {
        PairingMachine { code_hash, submitted_ok: false, fails: 0 }
    }

    /// 提交远程验证码,常数时间比对
    pub fn submit_remote(&mut self, remote_code: &str) -> bool {
        if verify_code(remote_code, &self.code_hash) {
            self.submitted_ok = true;
            true
        } else {
            self.fails += 1;
            false
        }
    }

    pub fn is_complete(&self) -> bool {
        self.submitted_ok
    }

    pub fn failed_out(&self) -> bool {
        self.fails >= 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_code_is_6_digits() {
        for _ in 0..100 {
            let code = generate_pair_code();
            assert_eq!(code.len(), 6, "码应为 6 位: {}", code);
            assert!(code.chars().all(|c| c.is_ascii_digit()), "码应全数字: {}", code);
        }
    }

    #[test]
    fn codes_are_random_across_attempts() {
        // 100 次生成的码收集去重,至少应有 90 个不同值
        // (生日碰撞下限保护;真随机 6 位码 100 次几乎不可能少于 90 个不同值)
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(generate_pair_code());
        }
        assert!(seen.len() >= 90, "100 次生成应至少 90 个不同码,实际 {}", seen.len());
    }

    #[test]
    fn hash_and_verify_roundtrip() {
        let code = generate_pair_code();
        let hash = hash_code(&code);
        assert!(verify_code(&code, &hash), "正确码应验证通过");
        assert!(!verify_code("000000", &hash) || code == "000000", "错误码应验证失败");
    }

    #[test]
    fn pairing_machine_accepts_correct_code() {
        let code = generate_pair_code();
        let mut m = PairingMachine::new(hash_code(&code));
        assert!(m.submit_remote(&code), "正确码应匹配");
        assert!(m.is_complete());
        assert!(!m.failed_out());
    }

    #[test]
    fn pairing_machine_counts_failures() {
        let code = generate_pair_code();
        let mut m = PairingMachine::new(hash_code(&code));
        assert!(!m.submit_remote("000000") || code == "000000");
        assert!(!m.submit_remote("111111") || code == "111111");
        assert!(!m.failed_out(), "2 次失败不应判定失败");
        assert!(!m.submit_remote("222222") || code == "222222");
        assert!(m.failed_out(), "3 次失败应判定失败");
    }
}
