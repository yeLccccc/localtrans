package com.localtrans.app

import android.content.Context
import android.util.Log
import com.localtrans.app.bridge.EventRouter
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.LocalTransCallback
import uniffi.localtrans_ffi.LocalTransApp as FfiApp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob

object LocalTransBridge {
    private const val TAG = "LocalTransBridge"

    private var appContext: Context? = null

    lateinit var app: FfiApp
        private set

    val events: MutableSharedFlow<AppEvent> = MutableSharedFlow(extraBufferCapacity = 256)

    /** start() 失败信息(端口被占等),null 表示启动正常;UI 据此展示错误态 */
    @JvmStatic
    var startError: String? = null
        private set

    // P3-T6:SupervisorJob + CoroutineExceptionHandler——子协程未捕获异常只打
    // logcat 不崩进程;SupervisorJob 保证单个子协程失败不取消 scope 内其他
    // 子协程(app.start()/事件路由等长命任务不受连坐)。
    private val applicationScope = CoroutineScope(
        SupervisorJob() + Dispatchers.Default + CoroutineExceptionHandler { _, e ->
            Log.e("LT::kotlin", "applicationScope 未捕获异常(进程不退出)", e)
        }
    )

    /**
     * Pending tab to navigate to (from notification deep link).
     * StateFlow 而非普通 var:P3-T1 修复——原实现仅被 AppNav 首组合的
     * LaunchedEffect(Unit) 消费一次,前台热启动(onNewIntent 重发)后无人再读,
     * 点完成通知不切页。改 StateFlow 后 AppNav 持续 collect,每次重发都会触发。
     */
    @JvmStatic
    val pendingTab = MutableStateFlow<String?>(null)

    /**
     * Set pending tab (called from MainActivity onNewIntent/onCreate)
     */
    @JvmStatic
    fun setPendingTab(tab: String?) {
        pendingTab.value = tab
    }

    /**
     * Clear pending tab (called from AppNav after navigation)
     */
    @JvmStatic
    fun clearPendingTab() {
        pendingTab.value = null
    }

    /**
     * Pending "browse this device's shares" target (set from Devices page,
     * consumed by FilesScreen when it lands on the REMOTE tab)
     */
    @JvmStatic
    var pendingBrowseDeviceFp: String? = null
        private set

    @JvmStatic
    fun setPendingBrowseDevice(fp: String?) {
        pendingBrowseDeviceFp = fp
    }

    @JvmStatic
    fun consumePendingBrowseDevice(): String? {
        val fp = pendingBrowseDeviceFp
        pendingBrowseDeviceFp = null
        return fp
    }

    fun init(context: Context) {
        if (::app.isInitialized) {
            Log.d(TAG, "Already initialized")
            return
        }

        appContext = context.applicationContext

        try {
            val dataDir = context.filesDir.resolve("localtrans").absolutePath
            Log.d(TAG, "Initializing with dataDir: $dataDir")

            // constructor 失败(runtime 初始化失败等)走 startError 错误态,不崩进程
            app = try {
                FfiApp(dataDir, Callback())
            } catch (e: Exception) {
                Log.e(TAG, "Failed to construct LocalTransApp", e)
                startError = e.message ?: "初始化失败"
                return
            }

            // Start app in background thread (tokio runtime, won't block UI)
            applicationScope.launch {
                try {
                    app.start()
                    Log.d(TAG, "App started successfully")
                    startError = null

                    // 接收落盘注入用户可见目录:Download/LocalTrans(图片/视频扫描进相册,
                    // 文档文件管理器可见)。目录自建;失败仅回落私有目录(不阻断启动)。
                    // 必须在 start() 之后——setInboxDir 写入 AppState,启动前尚不存在。
                    try {
                        @Suppress("DEPRECATION")
                        val downloads = android.os.Environment
                            .getExternalStoragePublicDirectory(android.os.Environment.DIRECTORY_DOWNLOADS)
                        val inbox = java.io.File(downloads, "LocalTrans")
                        if (inbox.exists() || inbox.mkdirs()) {
                            app.setInboxDir(inbox.absolutePath)
                        }
                    } catch (e: Exception) {
                        Log.w(TAG, "inbox 目录注入失败,回落私有目录: ${e.message}")
                    }
                } catch (e: Exception) {
                    // start() 失败(如端口被占):记录错误供 UI 展示,不崩进程
                    Log.e(TAG, "Failed to start app", e)
                    startError = e.message ?: "启动失败"
                }
            }
        } catch (e: Exception) {
            Log.e(TAG, "Failed to initialize LocalTransApp", e)
            throw e
        }
    }

    fun getAppContext(): Context {
        return appContext ?: throw IllegalStateException("LocalTransBridge not initialized")
    }

    class Callback : LocalTransCallback {
        override fun onEvent(event: AppEvent) {
            Log.d(TAG, "event: ${event::class.simpleName}")
            events.tryEmit(event)
            // Route event in coroutine scope
            applicationScope.launch {
                EventRouter.route(event)
            }
        }
    }
}
