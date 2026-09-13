package com.localtrans.app.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.localtrans.app.AppFlags
import com.localtrans.app.data.SettingsRepo
import com.localtrans.app.LocalTransBridge
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.SettingsDto

/**
 * UI State for Settings Screen
 */
data class SettingsUiState(
    val isLoading: Boolean = false,
    val isSaving: Boolean = false,
    val savedTick: Int = 0,
    val error: String? = null,
    val relayStatusText: String = "未启用"
)

/**
 * Form state for settings editing
 * (v0.8.0 起相册备份功能已移除,备份字段不再上屏——
 *  SettingsDto 字段保留以兼容 Rust 侧配置结构,保存时固定传关闭值)
 */
data class SettingsFormState(
    val deviceName: String,
    val hidden: Boolean,
    val offerTimeoutSecs: String,
    val consentTimeoutSecs: String,
    val relayEnabled: Boolean,
    val relayAddr: String,
    val relayPsk: String
)

/**
 * ViewModel for Settings screen
 */
class SettingsViewModel(
    private val repo: SettingsRepo
) : ViewModel() {

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState.asStateFlow()

    private val _formState = MutableStateFlow(
        SettingsFormState(
            deviceName = "",
            hidden = false,
            offerTimeoutSecs = "60",
            consentTimeoutSecs = "30",
            relayEnabled = false,
            relayAddr = "",
            relayPsk = ""
        )
    )
    val formState: StateFlow<SettingsFormState> = _formState.asStateFlow()

    // M2 T4:并发上限暂不进设置表单(T5 接入 UI),加载时暂存、保存时透传,
    // 防 saveSettings 把已有配置冲回固定值(同备份字段"保留兼容"惯例)
    private var maxActiveTransfers: UInt = 3u

    init {
        // Load settings
        loadSettings()

        // Listen for events
        viewModelScope.launch {
            repo.events.collect { event ->
                handleEvent(event)
            }
        }
    }

    private fun loadSettings() {
        viewModelScope.launch {
            _uiState.update { it.copy(isLoading = true) }
            try {
                val settings = repo.settings()
                maxActiveTransfers = settings.maxActiveTransfers
                _formState.value = SettingsFormState(
                    deviceName = settings.deviceName,
                    hidden = settings.hidden,
                    offerTimeoutSecs = settings.offerTimeoutSecs.toString(),
                    consentTimeoutSecs = settings.consentTimeoutSecs.toString(),
                    relayEnabled = settings.relayEnabled,
                    relayAddr = settings.relayAddr,
                    relayPsk = settings.relayPsk
                )
                // v0.9.0: 加载完设置后顺手拉一次中继状态
                loadRelayStatus()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = "加载设置失败:${e.message ?: "未知错误"}") }
            } finally {
                _uiState.update { it.copy(isLoading = false) }
            }
        }
    }

    /**
     * v0.9.0: 拉模式拉取中继状态并渲染为中文文案。
     * 异常时不阻塞 UI,降级为"状态未知"。
     */
    fun loadRelayStatus() {
        // 发布态中继 UI 隐藏:不触达 FFI,状态文案停在默认"未启用"
        if (!AppFlags.RELAY_ENABLED) return
        viewModelScope.launch {
            try {
                val st = repo.relayStatus()
                val text = when {
                    !st.enabled -> "未启用"
                    st.connected -> "已连接"
                    st.status == "error" -> "配置错误: ${st.error}"
                    else -> "连接中"
                }
                _uiState.update { it.copy(relayStatusText = text) }
            } catch (e: Exception) {
                _uiState.update { it.copy(relayStatusText = "状态未知") }
            }
        }
    }

    private fun handleEvent(event: AppEvent) {
        // No specific settings events to handle currently
        // Settings are manually loaded when needed
    }

    // Form update methods

    fun updateDeviceName(name: String) {
        _formState.update { it.copy(deviceName = name) }
    }

    fun updateHidden(hidden: Boolean) {
        _formState.update { it.copy(hidden = hidden) }
    }

    fun updateOfferTimeout(secs: String) {
        _formState.update { it.copy(offerTimeoutSecs = secs) }
    }

    fun updateConsentTimeout(secs: String) {
        _formState.update { it.copy(consentTimeoutSecs = secs) }
    }

    fun updateRelayEnabled(enabled: Boolean) {
        _formState.update { it.copy(relayEnabled = enabled) }
    }

    fun updateRelayAddr(addr: String) {
        _formState.update { it.copy(relayAddr = addr) }
    }

    fun updateRelayPsk(psk: String) {
        _formState.update { it.copy(relayPsk = psk) }
    }

    fun saveSettings() {
        viewModelScope.launch {
            _uiState.update { it.copy(isSaving = true, error = null) }

            // 中继配置前置校验(Rust 单一规则实现,与桌面/服务端同源):
            // 失败提示具体原因,不落盘不发起连接
            if (_formState.value.relayEnabled) {
                try {
                    // M-C9: FFI 校验是阻塞调用,移到 IO 线程避免主线程 ANR
                    withContext(Dispatchers.IO) {
                        LocalTransBridge.app.validateRelay(
                            true,
                            _formState.value.relayAddr,
                            _formState.value.relayPsk
                        )
                    }
                } catch (e: Exception) {
                    _uiState.update { it.copy(isSaving = false, error = e.message ?: "中继配置无效") }
                    return@launch
                }
            }

            try {
                // Validate and clamp timeouts
                val offerTimeout = _formState.value.offerTimeoutSecs.toIntOrNull() ?: 60
                val consentTimeout = _formState.value.consentTimeoutSecs.toIntOrNull() ?: 30

                val clampedOfferTimeout = offerTimeout.coerceIn(15, 600)
                val clampedConsentTimeout = consentTimeout.coerceIn(15, 600)

                // Update form with clamped values
                _formState.update {
                    it.copy(
                        offerTimeoutSecs = clampedOfferTimeout.toString(),
                        consentTimeoutSecs = clampedConsentTimeout.toString()
                    )
                }

                val settings = SettingsDto(
                    deviceName = _formState.value.deviceName,
                    hidden = _formState.value.hidden,
                    offerTimeoutSecs = clampedOfferTimeout.toULong(),
                    consentTimeoutSecs = clampedConsentTimeout.toULong(),
                    relayEnabled = _formState.value.relayEnabled,
                    relayAddr = _formState.value.relayAddr,
                    relayPsk = _formState.value.relayPsk,
                    maxActiveTransfers = maxActiveTransfers,
                    backupEnabled = false,
                    backupTargetFp = "",
                    backupPhotos = false,
                    backupVideos = false
                )

                repo.saveSettings(settings)

                // Update hidden mode separately if changed
                repo.setHidden(_formState.value.hidden)

                // 保存成功反馈:tick 自增驱动 UI 显示"已保存"
                _uiState.update { it.copy(savedTick = it.savedTick + 1) }

                // v0.9.0: 中继配置可能刚变,保存后刷新一次状态行
                loadRelayStatus()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = "保存失败:${e.message ?: "未知错误"}") }
            } finally {
                _uiState.update { it.copy(isSaving = false) }
            }
        }
    }

    fun clearError() {
        _uiState.update { it.copy(error = null) }
    }
}

/**
 * Factory for creating SettingsViewModel with FFI-based repository
 */
class SettingsViewModelFactory(
    private val bridge: LocalTransBridge
) : androidx.lifecycle.ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : androidx.lifecycle.ViewModel> create(modelClass: Class<T>): T {
        if (modelClass.isAssignableFrom(SettingsViewModel::class.java)) {
            val repo = com.localtrans.app.data.FfiSettingsRepo(bridge.app, bridge.events)
            return SettingsViewModel(repo) as T
        }
        throw IllegalArgumentException("Unknown ViewModel class")
    }
}
