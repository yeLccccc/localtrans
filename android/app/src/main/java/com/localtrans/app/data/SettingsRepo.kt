package com.localtrans.app.data

import kotlinx.coroutines.flow.SharedFlow
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.RelayStatusDto
import uniffi.localtrans_ffi.SettingsDto

/**
 * Repository interface for settings operations
 */
interface SettingsRepo {
    /**
     * Get current settings
     */
    suspend fun settings(): SettingsDto

    /**
     * Save settings
     */
    suspend fun saveSettings(settings: SettingsDto)

    /**
     * Set hidden mode
     */
    suspend fun setHidden(hidden: kotlin.Boolean)

    /**
     * v0.9.0: 查询中继状态(设置页拉模式显示)
     */
    suspend fun relayStatus(): RelayStatusDto

    /**
     * Event flow from FFI layer
     */
    val events: SharedFlow<AppEvent>
}

/**
 * FFI-based implementation of SettingsRepo
 */
class FfiSettingsRepo(
    private val app: uniffi.localtrans_ffi.LocalTransApp,
    eventFlow: SharedFlow<AppEvent>
) : SettingsRepo {

    override val events: SharedFlow<AppEvent> = eventFlow

    override suspend fun settings(): SettingsDto =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.settings() }

    override suspend fun saveSettings(settings: SettingsDto) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.saveSettings(settings) }

    override suspend fun setHidden(hidden: kotlin.Boolean) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.setHidden(hidden) }

    override suspend fun relayStatus(): RelayStatusDto =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.`relayStatus`() }
}

/**
 * Fake implementation for testing
 */
class FakeSettingsRepo : SettingsRepo {
    private var _hidden = false
    private var _settings = SettingsDto(
        deviceName = "Test Device",
        hidden = false,
        offerTimeoutSecs = 60u,
        consentTimeoutSecs = 30u,
        relayEnabled = false,
        relayAddr = "",
        relayPsk = "",
        maxActiveTransfers = 3u,
        backupEnabled = false,
        backupTargetFp = "",
        backupPhotos = false,
        backupVideos = false
    )
    private val _events = kotlinx.coroutines.flow.MutableSharedFlow<AppEvent>(extraBufferCapacity = 256)

    /**
     * v0.9.0: 测试覆写中继状态(默认未启用)
     */
    var relayStatusOverride: RelayStatusDto? = null

    override val events: SharedFlow<AppEvent> = _events

    override suspend fun settings(): SettingsDto = _settings

    override suspend fun saveSettings(settings: SettingsDto) {
        _settings = settings
    }

    override suspend fun setHidden(hidden: kotlin.Boolean) {
        _hidden = hidden
        _settings = _settings.copy(hidden = hidden)
    }

    override suspend fun relayStatus(): RelayStatusDto =
        relayStatusOverride ?: RelayStatusDto(
            enabled = false,
            connected = false,
            status = "disabled",
            error = "",
            publicExit = null
        )

    /**
     * Helper method to emit events for testing
     */
    suspend fun emitEvent(event: AppEvent) {
        _events.emit(event)
    }
}
