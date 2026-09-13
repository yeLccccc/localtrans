package com.localtrans.app.data

import kotlinx.coroutines.flow.SharedFlow
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.DeviceDto
import uniffi.localtrans_ffi.SettingsDto

/**
 * Repository interface for device operations
 * Abstracts FFI layer for testability
 */
interface DevicesRepo {
    /**
     * Get list of discovered devices
     */
    suspend fun devices(): List<DeviceDto>

    /**
     * Connect to a device by fingerprint
     */
    suspend fun connectDevice(fingerprint: String)

    /**
     * Disconnect from a device
     */
    suspend fun disconnect(fingerprint: String)

    /**
     * Respond to a pairing consent request
     */
    suspend fun respondConsent(fingerprint: String, accept: Boolean)

    /**
     * Submit a pairing code
     */
    suspend fun submitPairingCode(fingerprint: String, code: String)

    /**
     * Cancel waiting for pairing code
     */
    suspend fun cancelWait(fingerprint: String)

    /**
     * Set hidden mode
     */
    suspend fun setHidden(hidden: Boolean)

    /**
     * Get my fingerprint
     */
    suspend fun myFingerprint(): String

    /**
     * Get current settings
     */
    suspend fun settings(): SettingsDto

    /**
     * 本机主网卡 IP(无网络时 null)
     */
    suspend fun localIp(): String?

    /**
     * 手动探测指定地址(仅 IP 补默认端口 47600)
     */
    suspend fun probeAddr(addr: String)

    /**
     * 通道探测记录表(M3c T1:设备卡通道标签数据源;内存态,表空=未知)
     */
    suspend fun channels(): List<uniffi.localtrans_ffi.ChannelDto>

    /**
     * 手动单对端快检(M3c T2:通道面板「重新探测」;当前通道 64KB 快检,
     * 掉 50% 升级全量——与后台 5min 调度器同路径,只更新内存通道表)
     */
    suspend fun probeNowPeer(fingerprint: String)

    /**
     * 强制走中继开关(M3c T3:per 设备持久化;开启后 connect 跳过评分直选中继)
     */
    suspend fun setForceRelay(fingerprint: String, on: Boolean)

    /**
     * Event flow from FFI layer
     */
    val events: SharedFlow<AppEvent>
}

/**
 * FFI-based implementation of DevicesRepo
 */
class FfiDevicesRepo(
    private val app: uniffi.localtrans_ffi.LocalTransApp,
    eventFlow: SharedFlow<AppEvent>
) : DevicesRepo {

    override val events: SharedFlow<AppEvent> = eventFlow

    override suspend fun devices(): List<DeviceDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.devices() }

    override suspend fun connectDevice(fingerprint: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.connectDevice(fingerprint) }

    override suspend fun disconnect(fingerprint: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.disconnect(fingerprint) }

    override suspend fun respondConsent(fingerprint: String, accept: Boolean) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.respondConsent(fingerprint, accept) }

    override suspend fun submitPairingCode(fingerprint: String, code: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.submitPairingCode(fingerprint, code) }

    override suspend fun cancelWait(fingerprint: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.cancelWait(fingerprint) }

    override suspend fun setHidden(hidden: Boolean) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.setHidden(hidden) }

    override suspend fun myFingerprint(): String =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.myFingerprint() }

    override suspend fun settings(): SettingsDto =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.settings() }

    override suspend fun localIp(): String? =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.localIp() }

    override suspend fun probeAddr(addr: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.probeAddr(addr) }

    override suspend fun channels(): List<uniffi.localtrans_ffi.ChannelDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.channels() }

    override suspend fun probeNowPeer(fingerprint: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.probePeerNow(fingerprint) }

    override suspend fun setForceRelay(fingerprint: String, on: Boolean) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.setForceRelay(fingerprint, on) }
}

/**
 * Fake implementation for testing
 */
class FakeDevicesRepo : DevicesRepo {
    private val _devices = mutableListOf<DeviceDto>()
    private val _events = kotlinx.coroutines.flow.MutableSharedFlow<AppEvent>(extraBufferCapacity = 256)
    private var _myFingerprint = "ABC12345"
    private var _hidden = false

    var fakeLocalIp: String? = "192.168.1.105"
    var probeError: Exception? = null
    var lastProbedAddr: String? = null
    var submitError: Exception? = null
    /** M3c T1:通道表 fake 数据(默认空表=未知) */
    private val fakeChannels = mutableListOf<uniffi.localtrans_ffi.ChannelDto>()

    override val events: SharedFlow<AppEvent> = _events

    override suspend fun devices(): List<DeviceDto> = _devices.toList()

    override suspend fun connectDevice(fingerprint: String) {
        // No-op for testing
    }

    override suspend fun disconnect(fingerprint: String) {
        // No-op for testing
    }

    override suspend fun respondConsent(fingerprint: String, accept: Boolean) {
        // No-op for testing
    }

    override suspend fun submitPairingCode(fingerprint: String, code: String) {
        submitError?.let { throw it }
    }

    override suspend fun cancelWait(fingerprint: String) {
        // No-op for testing
    }

    override suspend fun setHidden(hidden: Boolean) {
        _hidden = hidden
    }

    override suspend fun myFingerprint(): String = _myFingerprint

    override suspend fun settings(): SettingsDto = SettingsDto(
        deviceName = "Test Device",
        hidden = _hidden,
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

    override suspend fun localIp(): String? = fakeLocalIp

    override suspend fun probeAddr(addr: String) {
        lastProbedAddr = addr
        probeError?.let { throw it }
    }

    override suspend fun channels(): List<uniffi.localtrans_ffi.ChannelDto> = fakeChannels.toList()

    /** M3c T2:fake 记录最近一次重测目标(单测断言用);可注入探测错误 */
    var lastProbedPeer: String? = null
    var probePeerError: Exception? = null

    override suspend fun probeNowPeer(fingerprint: String) {
        lastProbedPeer = fingerprint
        probePeerError?.let { throw it }
    }

    /** M3c T3:记录开关调用(fake 持久化语义由单测注入) */
    private val fakeForceRelay = mutableMapOf<String, Boolean>()
    var setForceRelayError: Exception? = null

    override suspend fun setForceRelay(fingerprint: String, on: Boolean) {
        setForceRelayError?.let { throw it }
        if (on) fakeForceRelay[fingerprint] = true else fakeForceRelay.remove(fingerprint)
    }

    /** M3c T3:fake 查询(配合 FakeDevicesRepo.devices() 注记) */
    fun isForceRelay(fingerprint: String): Boolean = fakeForceRelay[fingerprint] ?: false

    /**
     * Helper method to emit events for testing
     */
    suspend fun emitEvent(event: AppEvent) {
        _events.emit(event)
    }

    /**
     * M3c T1:注入通道记录供测试(fakeChannels 可变)
     */
    fun setChannels(records: List<uniffi.localtrans_ffi.ChannelDto>) {
        fakeChannels.clear()
        fakeChannels.addAll(records)
    }

    /**
     * Helper method to add devices for testing
     */
    fun addDevice(device: DeviceDto) {
        _devices.add(device)
    }

    /**
     * Helper method to set my fingerprint for testing
     */
    fun setMyFingerprint(fingerprint: String) {
        _myFingerprint = fingerprint
    }
}
