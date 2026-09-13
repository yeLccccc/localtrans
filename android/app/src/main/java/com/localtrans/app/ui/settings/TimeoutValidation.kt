package com.localtrans.app.ui.settings

/** 超时输入校验:非数字或超出 15-600 范围即无效 */
internal fun isTimeoutInvalid(text: String): Boolean {
    val v = text.toIntOrNull() ?: return true
    return v < 15 || v > 600
}
