package com.localtrans.app.ui.transfers

/**
 * FFI 层失败原因英文串 → 中文显示(未知串原样返回,Rust 侧部分 reason 已是中文)
 */
internal fun localizeFailReason(reason: String): String = when {
    reason == "removed" -> "已手动移除"
    reason.contains("refused") -> "对方拒绝了本次传输"
    reason.contains("timeout", ignoreCase = true) -> "等待超时"
    reason.contains("cancelled", ignoreCase = true) || reason.contains("canceled", ignoreCase = true) -> "传输已取消"
    reason.contains("disk full", ignoreCase = true) -> "存储空间不足"
    reason.contains("非法文件名") -> "文件名含目标系统不允许的字符"
    reason.isNotEmpty() -> reason
    else -> "传输失败"
}

/**
 * 可重试的失败类型:对端拒收 / 超时(removed 显式排除)
 */
internal fun isRetryableFailure(reason: String): Boolean {
    if (reason == "removed") return false
    if (reason.contains("非法文件名")) return false
    return reason.contains("refused") ||
        reason.contains("timeout") ||
        reason.contains("超时") ||
        reason.contains("拒绝")
}
