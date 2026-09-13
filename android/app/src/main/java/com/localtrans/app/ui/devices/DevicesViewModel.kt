package com.localtrans.app.ui.devices

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.localtrans.app.data.DevicesRepo
import com.localtrans.app.data.FfiDevicesRepo
import com.localtrans.app.LocalTransBridge
import kotlinx.coroutines.Job
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.ChannelDto
import uniffi.localtrans_ffi.DeviceDto

class DevicesViewModel(private val repo: DevicesRepo) : ViewModel() {

    private val _uiState = MutableStateFlow(DevicesUiState())
    val uiState: StateFlow<DevicesUiState> = _uiState.asStateFlow()

    private val _devices = MutableStateFlow<List<DeviceUi>>(emptyList())
    val devices: StateFlow<List<DeviceUi>> = _devices.asStateFlow()

    /** M3c T2:通道表原始记录(通道面板每地址明细数据源) */
    private val _channels = MutableStateFlow<List<ChannelDto>>(emptyList())
    val channels: StateFlow<List<ChannelDto>> = _channels.asStateFlow()

    /** M3c T2:正在手动快检的对端指纹(null=空闲;通道面板按钮"探测中…"态) */
    private val _probingPeer = MutableStateFlow<String?>(null)
    val probingPeer: StateFlow<String?> = _probingPeer.asStateFlow()

    private var pairingWatcherJob: Job? = null
    private var probeJob: Job? = null

    init {
        // Load initial data
        loadMyFingerprint()
        loadDevices()
        loadSettings()
        loadLocalIp()

        // Listen for events
        viewModelScope.launch {
            repo.events.collect { event ->
                handleEvent(event)
            }
        }
    }

    // Timeout watcher for code entry - called internally
    private fun startTimeoutWatcher() {
        pairingWatcherJob?.cancel()
        pairingWatcherJob = viewModelScope.launch {
            val timeoutMs = _uiState.value.consentTimeoutSecs * 1000L
            kotlinx.coroutines.delay(timeoutMs)
            val st = _uiState.value.pairing ?: return@launch
            if (st.state == PairingState.CODE_ENTRY) {
                _uiState.update {
                    it.copy(pairing = st.copy(state = PairingState.FAILED, reason = "等待对端确认超时"))
                }
            }
        }
    }

    private fun loadMyFingerprint() {
        viewModelScope.launch {
            try {
                val fp = repo.myFingerprint()
                _uiState.update { it.copy(myFingerprint = fp) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    private fun loadDevices() {
        viewModelScope.launch {
            try {
                // M3c T1:通道表与设备列表同节奏拉取(失败不影响设备列表,
                // 标签退化为「未知」);全量探测晚几秒到,SessionUp 有延迟补刷
                val channels = try {
                    repo.channels()
                } catch (e: Exception) {
                    emptyList()
                }
                _channels.value = channels
                val deviceDtos = repo.devices()
                _devices.value = deviceDtos.map { it.toUiModel(channels) }
                _uiState.update { it.copy(devices = _devices.value) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    /**
     * M3c T2:只重拉通道表并修补标签(通道面板重测后/SessionUp 补刷共用;
     * 不动 connected 等事件乐观补丁)。
     */
    private suspend fun refreshChannelsOnly() {
        try {
            val channels = repo.channels()
            _channels.value = channels
            _devices.update { list ->
                list.map { d -> d.copy(channel = channelFor(channels, d.fingerprint, d.addr)) }
            }
            _uiState.update { it.copy(devices = _devices.value) }
        } catch (e: Exception) {
            // 通道表拉取失败:保持现状,等下一轮 DevicesChanged
        }
    }

    /**
     * M3c T1:只重拉通道表并修补标签(不动 connected 等事件乐观补丁)。
     * SessionUp → 全量探测(阶梯 ~5MB)需数秒,延迟后补刷一次让标签及时出现;
     * 之后随 DevicesChanged(发现层 ~5s 节奏)持续刷新。
     */
    private fun scheduleChannelRefreshAfterSessionUp() {
        viewModelScope.launch {
            kotlinx.coroutines.delay(3_000)
            refreshChannelsOnly()
        }
    }

    private fun loadSettings() {
        viewModelScope.launch {
            try {
                val settings = repo.settings()
                _uiState.update {
                    it.copy(
                        hidden = settings.hidden,
                        consentTimeoutSecs = settings.consentTimeoutSecs.toLong(),
                        deviceName = settings.deviceName
                    )
                }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    private fun loadLocalIp() {
        viewModelScope.launch {
            try {
                val ip = repo.localIp()
                _uiState.update { it.copy(localIp = ip ?: "") }
            } catch (e: Exception) {
                _uiState.update { it.copy(localIp = "") }
            }
        }
    }

    private fun handleEvent(event: AppEvent) {
        when (event) {
            is AppEvent.DevicesChanged -> {
                loadDevices()
            }
            is AppEvent.ConsentRequested -> {
                _uiState.update {
                    it.copy(
                        pairing = PairingUiState(
                            fingerprint = event.fingerprint,
                            peerName = event.name,
                            state = PairingState.CONSENT_REQUESTED
                        )
                    )
                }
            }
            is AppEvent.PairingCodeShown -> {
                _uiState.update {
                    it.copy(
                        pairing = PairingUiState(
                            fingerprint = event.fingerprint,
                            peerName = "",
                            code = event.code,
                            state = PairingState.CODE_SHOWN
                        )
                    )
                }
            }
            is AppEvent.PairingCodeEntry -> {
                val timeoutMs = _uiState.value.consentTimeoutSecs * 1000L
                _uiState.update {
                    it.copy(
                        pairing = PairingUiState(
                            fingerprint = event.fingerprint,
                            peerName = event.name,
                            state = PairingState.CODE_ENTRY,
                            deadlineEpochMs = System.currentTimeMillis() + timeoutMs
                        )
                    )
                }
                startTimeoutWatcher()
            }
            is AppEvent.PairingWaitConsent -> {
                _uiState.update {
                    it.copy(
                        pairing = PairingUiState(
                            fingerprint = event.fingerprint,
                            peerName = event.name,
                            state = PairingState.WAITING_CONSENT
                        )
                    )
                }
            }
            is AppEvent.PairingResult -> {
                if (event.ok) {
                    _uiState.update {
                        it.copy(
                            pairing = it.pairing?.copy(
                                state = PairingState.SUCCESS
                            )
                        )
                    }
                    // Clear pairing dialog after delay
                    viewModelScope.launch {
                        kotlinx.coroutines.delay(2000)
                        _uiState.update { it.copy(pairing = null) }
                    }
                } else {
                    _uiState.update {
                        it.copy(
                            pairing = it.pairing?.copy(
                                state = PairingState.FAILED,
                                reason = event.reason
                            )
                        )
                    }
                }
            }
            is AppEvent.SessionUp -> {
                // Update device connection status
                updateDeviceConnection(event.fingerprint, true)
                // Clear pairing dialog
                _uiState.update { it.copy(pairing = null) }
                // M3c T1:会话建立 → 全量探测数秒后出数据,延迟补刷通道标签
                scheduleChannelRefreshAfterSessionUp()
            }
            is AppEvent.SessionDown -> {
                updateDeviceConnection(event.fingerprint, false)
            }
            else -> {
                // Handle other events if needed
            }
        }
    }

    private fun updateDeviceConnection(fingerprint: String, connected: Boolean) {
        _devices.update { devices ->
            devices.map { device ->
                if (device.fingerprint == fingerprint) {
                    device.copy(connected = connected)
                } else {
                    device
                }
            }
        }
        _uiState.update { it.copy(devices = _devices.value) }
    }

    // User actions

    fun connectDevice(fingerprint: String) {
        viewModelScope.launch {
            try {
                repo.connectDevice(fingerprint)
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message ?: "连接失败") }
            }
        }
    }

    /**
     * M3c T3:强制走中继开关(持久化;成功后乐观修补本地列表,角标/菜单
     * 勾选即时生效,不等下一轮 DevicesChanged)。
     */
    fun setForceRelay(fingerprint: String, on: Boolean) {
        viewModelScope.launch {
            try {
                repo.setForceRelay(fingerprint, on)
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message ?: "设置强制走中继失败") }
                return@launch
            }
            _devices.update { list ->
                list.map { d ->
                    if (d.fingerprint == fingerprint) d.copy(forceRelay = on) else d
                }
            }
            _uiState.update { it.copy(devices = _devices.value) }
        }
    }

    /**
     * M3c T2:通道面板「重新探测」——手动单对端快检;命令返回时内存表已
     * 更新,立即刷一次;升级全量时数据晚几秒到,3s 后补刷一次。
     * 失败经 error snackbar 呈现(设备未连接/无记录/探测拉黑文案由 ffi 给出)。
     */
    fun probeNowPeer(fingerprint: String) {
        if (_probingPeer.value != null) return // 防重入:同一时刻只跑一次快检
        viewModelScope.launch {
            _probingPeer.value = fingerprint
            try {
                repo.probeNowPeer(fingerprint)
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message ?: "重新探测失败") }
            } finally {
                _probingPeer.value = null
            }
            refreshChannelsOnly()
            kotlinx.coroutines.delay(3_000)
            refreshChannelsOnly()
        }
    }

    fun disconnect(fingerprint: String) {
        viewModelScope.launch {
            try {
                repo.disconnect(fingerprint)
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun respondConsent(fingerprint: String, accept: Boolean) {
        viewModelScope.launch {
            try {
                repo.respondConsent(fingerprint, accept)
                if (!accept) {
                    _uiState.update { it.copy(pairing = null) }
                }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun submitPairingCode(fingerprint: String, code: String) {
        viewModelScope.launch {
            try {
                repo.submitPairingCode(fingerprint, code)
            } catch (e: Exception) {
                _uiState.update {
                    it.copy(pairing = it.pairing?.copy(state = PairingState.FAILED, reason = e.message ?: "提交失败"))
                }
            }
        }
    }

    fun cancelWait(fingerprint: String) {
        viewModelScope.launch {
            try {
                repo.cancelWait(fingerprint)
                _uiState.update { it.copy(pairing = null) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun setHidden(hidden: Boolean) {
        viewModelScope.launch {
            try {
                repo.setHidden(hidden)
                _uiState.update { it.copy(hidden = hidden) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun clearPairing() {
        _uiState.update { it.copy(pairing = null) }
    }

    fun clearError() {
        _uiState.update { it.copy(error = null) }
    }

    fun probeDevice(addr: String) {
        probeJob?.cancel()
        _uiState.update {
            it.copy(manualProbe = ManualProbeUiState(state = ProbeState.PROBING, target = addr))
        }
        probeJob = viewModelScope.launch {
            try {
                repo.probeAddr(addr)
            } catch (e: Exception) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.ERROR, target = addr,
                        message = "探测发送失败: ${e.message}"
                    ))
                }
                return@launch
            }
            kotlinx.coroutines.delay(5000)
            ensureActive()
            val targetIp = addr.substringBefore(':')
            val found = try {
                repo.devices().any { it.addr.substringBefore(':') == targetIp }
            } catch (e: Exception) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.ERROR, target = addr,
                        message = "设备查询失败: ${e.message}"
                    ))
                }
                return@launch
            }
            if (found) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.FOUND, target = addr, message = "已发现设备"
                    ))
                }
                loadDevices()
            } else {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.NOT_FOUND, target = addr,
                        message = "未发现设备:对方可能不在线、已开启隐身,或本机隐身中暂停了探测"
                    ))
                }
            }
        }
    }

    fun clearManualProbe() {
        probeJob?.cancel()
        _uiState.update { it.copy(manualProbe = null) }
    }
}

// Extension function to convert DeviceDto to DeviceUi(M3c T1 附带通道标签映射;M3c T3 附带强制中继标记)
private fun DeviceDto.toUiModel(channels: List<ChannelDto>): DeviceUi {
    return DeviceUi(
        fingerprint = fingerprint,
        name = name,
        addr = addr,
        online = online,
        connected = connected,
        viaRelay = viaRelay,
        channel = channelFor(channels, fingerprint, addr),
        forceRelay = forceRelay
    )
}

/**
 * Factory for creating DevicesViewModel with FFI-based repository
 */
class DevicesViewModelFactory(
    private val bridge: LocalTransBridge
) : androidx.lifecycle.ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : androidx.lifecycle.ViewModel> create(modelClass: Class<T>): T {
        if (modelClass.isAssignableFrom(DevicesViewModel::class.java)) {
            val repo = FfiDevicesRepo(bridge.app, bridge.events)
            return DevicesViewModel(repo) as T
        }
        throw IllegalArgumentException("Unknown ViewModel class")
    }
}

