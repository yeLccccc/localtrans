package com.localtrans.app.debug

import android.content.Context
import android.util.Log

/**
 * M7 测试钩子(debug 构建专用,spec §7.3):
 * 编排器无法稳定自动化系统级配对弹窗(ADR-10),debug 构建提供语义开关——
 * 配对确认事件到达时由 EventRouter 直接自动同意,不再弹 UI。
 *
 * 存储:独立 SharedPreferences(debug_test_hooks),不进 Rust config——
 * 避免跨层协议改动(SettingsDto 是 core/ffi/两壳四方契约)。
 * 开关只在 debug 构建可写/可读(BuildConfig.DEBUG 门控在调用方 EventRouter
 * 与 SettingsScreen 的 debug 区块,release 构建无任何调用点)。
 */
object DebugTestHooks {
    private const val TAG = "LT::kotlin::hooks"
    private const val PREFS = "debug_test_hooks"
    private const val KEY_AUTO_CONSENT = "auto_consent_pairing"

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** 配对请求自动同意开关(默认关)。 */
    fun getAutoConsentPairing(context: Context): Boolean =
        prefs(context).getBoolean(KEY_AUTO_CONSENT, false)

    fun setAutoConsentPairing(context: Context, enabled: Boolean) {
        prefs(context).edit().putBoolean(KEY_AUTO_CONSENT, enabled).apply()
        Log.i(TAG, "[test-hook] auto_consent_pairing=$enabled")
    }
}
