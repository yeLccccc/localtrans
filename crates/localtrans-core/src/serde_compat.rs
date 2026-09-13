//! Serde 兼容模块,处理 u64 ↔ JSON 字符串的精度问题。

pub mod u64_hex_string {
    use serde::{Deserialize, Deserializer, Serializer};

    /// 把 u64 序列化为 16 位小写 hex 字符串(无 0x 前缀)。
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{:016x}", v))
    }

    /// 从 hex 字符串反序列化回 u64;允许可选 `0x` 前缀。
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        let stripped = s.trim_start_matches("0x");
        u64::from_str_radix(stripped, 16).map_err(serde::de::Error::custom)
    }
}
