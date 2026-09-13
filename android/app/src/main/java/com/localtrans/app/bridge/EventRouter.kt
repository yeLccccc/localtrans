package com.localtrans.app.bridge

import android.content.Context
import android.util.Log
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Dispatchers
import uniffi.localtrans_ffi.AppEvent
import com.localtrans.app.BuildConfig
import com.localtrans.app.notification.TransferNotifier
import com.localtrans.app.LocalTransBridge
import com.localtrans.app.debug.DebugTestHooks
import com.localtrans.app.media.MediaScanNotifier

/**
 * EventRouter - Routes FFI events to UI components and handlers
 */
object EventRouter {
    private const val TAG = "LocalTransBridge"

    /**
     * Main event flow from FFI layer
     * All FFI events are emitted here via LocalTransBridge
     */
    // replay=8: 权限弹窗/冷启动期间事件(OfferRequested 等)可能先于 AppNav 首个
    // 订阅者到达——replay 让订阅建立时立即补收最近事件。
    // (2026-09-07 实证:订阅后链路 onEvent→route→collect 全程无丢失,重装后弹窗
    // "丢失"实为 e2e 断言层假阴性;replay 保留用于真正的订阅前窗口)
    val events: MutableSharedFlow<AppEvent> = MutableSharedFlow(replay = 8, extraBufferCapacity = 256)

    /**
     * Route events to specific UI handlers
     */
    suspend fun route(event: AppEvent) {
        // M7 debug 钩子:自动同意配对(ADR-10,语义钩子替代系统弹窗自动化)。
        // 仅 debug 构建且开关开启时生效;命中则直接应答同意并停止下发——
        // 配对对话框不弹,UI 结构不变。release 构建整段被编译期剔除。
        if (BuildConfig.DEBUG && event is AppEvent.ConsentRequested) {
            val context = try { LocalTransBridge.getAppContext() } catch (e: Exception) { null }
            if (context != null && DebugTestHooks.getAutoConsentPairing(context)) {
                Log.i(TAG, "[test-hook] 自动同意配对: fp=${event.fingerprint.take(16)} name=${event.name}")
                withContext(Dispatchers.IO) {
                    try {
                        LocalTransBridge.app.respondConsent(event.fingerprint, true)
                    } catch (e: Exception) {
                        Log.w(TAG, "[test-hook] 自动同意应答失败: ${e.message}")
                    }
                }
                return
            }
        }

        // Emit to general event flow
        events.emit(event)

        // Handle specific event types
        when (event) {
            is AppEvent.TransferDone -> {
                // Show notification for completed transfers
                handleTransferDone(event)
            }
            is AppEvent.FilesSaved -> {
                try {
                    MediaScanNotifier.scanPaths(
                        LocalTransBridge.getAppContext(),
                        event.paths
                    )
                } catch (e: Exception) {
                    // Bridge 未初始化时静默
                }
            }
            else -> {
                // Other events are handled by ViewModels
            }
        }
    }

    /**
     * Handle TransferDone event for notifications
     */
    private suspend fun handleTransferDone(event: AppEvent.TransferDone) {
        // Get application context from bridge
        // Note: This requires LocalTransBridge to have been initialized with a context
        // In production, you'd want to pass the context more cleanly
        try {
            val notifier = TransferNotifier.getInstance(LocalTransBridge.getAppContext())
            notifier.handleTransferDoneEvent(event)
        } catch (e: Exception) {
            // Silently fail if context not available
        }
    }
}
