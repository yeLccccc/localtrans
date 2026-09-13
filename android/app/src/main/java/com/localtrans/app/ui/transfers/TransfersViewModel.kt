package com.localtrans.app.ui.transfers

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.localtrans.app.data.DevicesRepo
import com.localtrans.app.data.TransfersRepo
import com.localtrans.app.LocalTransBridge
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.TransferDto

class TransfersViewModel(
    private val repo: TransfersRepo,
    // M2 T5:对端展示名(别名>广播名>指纹缩写);可空便于纯单测构造
    private val devicesRepo: DevicesRepo? = null,
    @Suppress("unused") private val enableCountdownTicker: Boolean = true
) : ViewModel() {

    private val _uiState = MutableStateFlow(TransfersUiState())
    val uiState: StateFlow<TransfersUiState> = _uiState.asStateFlow()

    private val _transfers = MutableStateFlow<List<TransferUi>>(emptyList())
    val transfers: StateFlow<List<TransferUi>> = _transfers.asStateFlow()

    // M2 T5:磁盘历史(含视图已移除),打开入口时拉取
    private val _diskJobs = MutableStateFlow<List<uniffi.localtrans_ffi.DiskJobDto>>(emptyList())
    val diskJobs: StateFlow<List<uniffi.localtrans_ffi.DiskJobDto>> = _diskJobs.asStateFlow()

    // M2 T5:对端展示名源(指纹/名/别名)
    private val _peers = MutableStateFlow<List<PeerNameSource>>(emptyList())
    val peers: StateFlow<List<PeerNameSource>> = _peers.asStateFlow()

    // Speed estimation sampling buffer: jobId -> (timestampMs, doneBytes)
    private val speedSamples = mutableMapOf<Long, MutableList<Pair<Long, Long>>>()

    // R2 速度平滑:jobId -> 平滑器(EMA τ=2s + 尖峰抑制 + 500ms 显示节流,
    // 与桌面 ui/src/lib/speedSmooth.ts 同构——双端 UI 口径一致)
    private val speedSmoothers = mutableMapOf<Long, SpeedSmoother>()

    // 两级删除进行中的任务:事件(TransferUpdated)不得把删除中的卡插回列表
    private val removingJobIds = mutableSetOf<Long>()

    init {
        // Load initial transfers
        loadTransfers()
        loadPeers()

        // Listen for events
        viewModelScope.launch {
            repo.events.collect { event ->
                handleEvent(event)
            }
        }

        // v0.11.0 低危批:死 ticker 删除。offerSheet 已上移 AppNav 根层全局弹窗,
        // 本 VM 不再写 offerSheet,每秒空转循环无意义。
        // enableCountdownTicker 参数保留仅为测试构造兼容。
    }

    private fun loadTransfers() {
        viewModelScope.launch {
            try {
                val transferDtos = repo.transfers()
                _transfers.value = transferDtos.map { it.toUiModel() }
                _uiState.update { it.copy(transfers = _transfers.value) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    private fun loadPeers() {
        val devices = devicesRepo ?: return
        viewModelScope.launch {
            try {
                _peers.value = devices.devices().map {
                    PeerNameSource(fingerprint = it.fingerprint, name = it.name, alias = "")
                }
            } catch (e: Exception) {
                // 名册拉取失败→指纹缩写兜底
            }
        }
    }

    private fun handleEvent(event: AppEvent) {
        when (event) {
            is AppEvent.OfferRequested -> {
                // 弹窗已上移 AppNav 根层全局处理(任何页面可见)。
                // 这里不再驱动 offerSheet——保留 UiState 字段仅为测试兼容。
            }
            is AppEvent.TransferUpdated -> {
                // Update transfer in list
                updateTransfer(event.transfer.toUiModel())
            }
            is AppEvent.TransferDone -> {
                if (event.failReason == "removed") {
                    // 两级删除/取消收尾事件:卡已从表删出或标记 removed,列表同步删行
                    dropRow(event.jobId.toLong())
                } else {
                    updateTransferDone(event.jobId, event.ok, event.failReason)
                }
            }
            is AppEvent.DevicesChanged -> loadPeers()
            else -> {
                // Handle other events if needed
            }
        }
    }

    private fun updateTransfer(transfer: TransferUi) {
        // 两级删除进行中的卡:引擎残余事件不得插回(否则刚删的卡凭空复活)
        if (transfer.jobId in removingJobIds) return

        // Record speed sample before applying update
        val now = System.currentTimeMillis()
        val samples = speedSamples.getOrPut(transfer.jobId) { mutableListOf() }
        samples.add(Pair(now, transfer.done))

        // Trim samples to within last 3000ms and cap at 8 entries
        val cutoff = now - 3000
        samples.retainAll { it.first >= cutoff }
        if (samples.size > 8) {
            samples.removeAt(0)
        }

        // If speedBps is 0, estimate from samples
        val rawBps = if (transfer.speedBps == 0L && samples.size >= 2) {
            SpeedEstimator.estimate(samples)
        } else {
            transfer.speedBps
        }

        // R2:速度进 UI 前过平滑器(进度导数优先+EMA+尖峰抑制+显示节流);ETA 随平滑速度
        // (对齐桌面 TransferItem——剩余时间用平滑后速度计算,不随瞬跳)
        val smoother = speedSmoothers.getOrPut(transfer.jobId) { SpeedSmoother() }
        val displayBps = smoother.push(rawBps, now, transfer.done)
        val etaSecs = if (transfer.state == "active" && displayBps > 0) {
            SpeedEstimator.etaSecs(maxOf(0L, transfer.total - transfer.done), displayBps)
        } else {
            transfer.etaSecs
        }
        val finalTransfer = transfer.copy(speedBps = displayBps, etaSecs = etaSecs)

        _transfers.update { transfers ->
            // upsert:事件可能先于列表加载到达(接收侧任务从未进过表),
            // 未知 jobId 直接插到头部——否则对方推送的任务在传输页凭空消失
            if (transfers.none { it.jobId == finalTransfer.jobId }) {
                listOf(finalTransfer) + transfers
            } else {
                transfers.map {
                    if (it.jobId == finalTransfer.jobId) finalTransfer else it
                }
            }
        }
        _uiState.update { it.copy(transfers = _transfers.value) }
    }

    private fun updateTransferDone(jobId: kotlin.ULong, ok: kotlin.Boolean, failReason: kotlin.String) {
        val jobIdLong = jobId.toLong()
        // Clean up samples for terminal state
        speedSamples.remove(jobIdLong)
        speedSmoothers.remove(jobIdLong)

        _transfers.update { transfers ->
            transfers.map { transfer ->
                if (transfer.jobId == jobIdLong) {
                    transfer.copy(
                        state = if (ok) "done" else "failed",
                        failReason = failReason
                    )
                } else {
                    transfer
                }
            }
        }
        _uiState.update { it.copy(transfers = _transfers.value) }
    }

    /** 从列表删行(removed 事件路径):清采样与删除守卫 */
    private fun dropRow(jobIdLong: Long) {
        speedSamples.remove(jobIdLong)
        speedSmoothers.remove(jobIdLong)
        removingJobIds.remove(jobIdLong)
        _transfers.update { it.filter { t -> t.jobId != jobIdLong } }
        _uiState.update { s -> s.copy(transfers = _transfers.value) }
    }

    // User actions

    fun pauseTransfer(jobId: kotlin.ULong) {
        viewModelScope.launch {
            try {
                repo.pause(jobId)
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun resumeTransfer(jobId: kotlin.ULong) {
        viewModelScope.launch {
            try {
                repo.resume(jobId)
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    /**
     * 取消活动/暂停卡 = 取消 + 彻底删(destroy 级):FFI 侧走 cancelling 仲裁
     * (引擎取消路径 + 5s 看门狗),parts 保留为续传数据交磁盘历史决定。
     */
    fun cancelTransfer(jobId: kotlin.ULong) {
        removeWithLevel(jobId, "destroy")
    }

    /**
     * 删除单卡(两级):终态卡 level="view"(可找回)或 "destroy"(不可恢复);
     * 活动卡一律 destroy(取消仲裁后收尾)。
     */
    fun removeTransfer(jobId: kotlin.ULong, level: String) {
        removeWithLevel(jobId, level)
    }

    private fun removeWithLevel(jobId: kotlin.ULong, level: String) {
        val jobIdLong = jobId.toLong()
        speedSamples.remove(jobIdLong)
        removingJobIds.add(jobIdLong)

        viewModelScope.launch {
            try {
                val removed = repo.transferRemoveLevel(jobId, level)
                if (removed) {
                    // 删行但保留守卫:destroy 活动卡在 cancelling 仲裁窗口(≤5s)内
                    // 引擎残余 TransferUpdated 不得把卡插回;removed 事件/超时兜底清守卫
                    removeRowLocally(jobIdLong)
                    armRemoveGuardTimeout(jobIdLong)
                } else {
                    // FFI 无此卡(表已无)——清本地残行与守卫
                    removingJobIds.remove(jobIdLong)
                    removeRowLocally(jobIdLong)
                }
            } catch (e: Exception) {
                // FFI 删除失败——保留卡片(view 级对活动卡报"任务进行中,请先取消")
                removingJobIds.remove(jobIdLong)
            }
        }
    }

    /** 仅删行不动守卫(removed 事件路径 dropRow 负责清守卫) */
    private fun removeRowLocally(jobIdLong: Long) {
        speedSamples.remove(jobIdLong)
        speedSmoothers.remove(jobIdLong)
        _transfers.update { it.filter { t -> t.jobId != jobIdLong } }
        _uiState.update { s -> s.copy(transfers = _transfers.value) }
    }

    /** 守卫超时兜底:看门狗 5s + 事件余量,过期放行(防守卫泄漏挡住磁盘历史恢复) */
    private fun armRemoveGuardTimeout(jobIdLong: Long) {
        viewModelScope.launch {
            kotlinx.coroutines.delay(8_000)
            removingJobIds.remove(jobIdLong)
        }
    }

    /**
     * 清空所有终态卡(两级删除语义=移除视图):逐卡走 view 级,
     * 磁盘 parts 保留,可在磁盘历史找回。
     */
    fun clearFinishedTransfers() {
        viewModelScope.launch {
            val terminal = _transfers.value.filter { it.state in TERMINAL_STATES }
            val terminalIds = terminal.map { it.jobId }.toSet()
            terminal.forEach { removingJobIds.add(it.jobId) }
            try {
                terminal.forEach {
                    try {
                        repo.transferRemoveLevel(it.jobId.toULong(), "view")
                    } catch (e: Exception) {
                        // 单卡失败跳过,不影响其余
                    }
                }
            } finally {
                _transfers.update { list -> list.filter { it.jobId !in terminalIds } }
                terminalIds.forEach { removingJobIds.remove(it) }
                _uiState.update { s -> s.copy(transfers = _transfers.value) }
            }
        }
    }

    fun retryTransfer(jobId: kotlin.ULong) {
        viewModelScope.launch {
            try {
                repo.retryTransfer(jobId)
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun respondOffer(jobId: kotlin.ULong, accept: kotlin.Boolean) {
        viewModelScope.launch {
            try {
                repo.respondOffer(jobId, accept)
                // Dismiss sheet
                _uiState.update { it.copy(offerSheet = null) }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    fun dismissOffer() {
        _uiState.update { it.copy(offerSheet = null) }
    }

    // ===== 磁盘历史(M2 T5;对齐桌面 Transfers.vue 磁盘历史区) =====

    /** 打开磁盘历史入口时拉取一次 */
    fun loadDiskJobs() {
        viewModelScope.launch {
            try {
                _diskJobs.value = repo.listDiskJobs()
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    /** 恢复到列表:FFI 补发 TransferUpdated(upsert 回列表),并刷新磁盘列表 */
    fun restoreDiskJob(jobId: kotlin.ULong) {
        viewModelScope.launch {
            try {
                repo.restoreDiskJob(jobId)
                loadTransfers()
            } catch (e: Exception) {
                // Handle error
            } finally {
                loadDiskJobs()
            }
        }
    }

    /** 彻底删除磁盘历史(在表 finalize_destroy / 纯孤儿删 parts),刷新磁盘列表 */
    fun destroyDiskJob(jobId: kotlin.ULong) {
        viewModelScope.launch {
            try {
                repo.destroyDiskJob(jobId)
            } catch (e: Exception) {
                // Handle error
            } finally {
                loadDiskJobs()
            }
        }
    }

    // ===== debug 构建专属:演示卡注入(M2 T5 视觉验证/T7 场景断言用) =====
    // FFI 现阶段 children 恒空、queue_pos/积压态依赖时序,六状态截图目检
    // 无法全部由真实传输产生;沿用 DebugTestHooks 惯例提供 debug 注入,
    // 调用点由 BuildConfig.DEBUG 门控,release 无入口。仅改 VM 内存态,不触 repo。

    fun seedDemoCardsForUiCheck() {
        val demo = listOf(
            TransferUi(
                jobId = 9_001, name = "photos-batch", total = 3_000_000, done = 3_000_000,
                state = "active", speedBps = 1_200_000, peer = "aa99881122334455",
                direction = "push", localRole = "source-push", progressPercent = 100,
                etaSecs = 0, failReason = "", localPath = null,
                remoteDone = 1_500_000, instant = false,
                queuePos = null, batchId = "b-1", partsId = null,
                startedAtMs = System.currentTimeMillis() - 65_000, finishedAtMs = null,
                sourcePath = null,
                children = listOf(
                    ChildUi("c-1", "sunset.jpg", 1_200_000, 1_200_000, "done"),
                    ChildUi("c-2", "report.pdf", 800_000, 300_000, "active"),
                    ChildUi("c-3", "notes.txt", 1_000_000, 0, "pending")
                )
            ),
            TransferUi(
                jobId = 9_002, name = "backup-2026.zip", total = 500_000_000, done = 0,
                state = "pending", speedBps = 0, peer = "aa99881122334455",
                direction = "push", localRole = "source-push", progressPercent = 0,
                etaSecs = -1, failReason = "", localPath = null,
                remoteDone = 0, instant = false,
                queuePos = 2, batchId = null, partsId = null,
                startedAtMs = System.currentTimeMillis(), finishedAtMs = null,
                sourcePath = null
            ),
            TransferUi(
                jobId = 9_003, name = "movie.mkv", total = 1_400_000_000, done = 600_000_000,
                state = "paused", speedBps = 0, peer = "aa99881122334455",
                direction = "rx", localRole = "receiver", progressPercent = 42,
                etaSecs = -1, failReason = "", localPath = null,
                remoteDone = 0, instant = false,
                queuePos = null, batchId = null, partsId = "p-9-3",
                startedAtMs = System.currentTimeMillis() - 300_000, finishedAtMs = null,
                sourcePath = null
            ),
            TransferUi(
                jobId = 9_004, name = "invoice.pdf", total = 300_000, done = 300_000,
                state = "done", speedBps = 0, peer = "aa99881122334455",
                direction = "rx", localRole = "receiver", progressPercent = 100,
                etaSecs = -1, failReason = "", localPath = null,
                remoteDone = 0, instant = false,
                queuePos = null, batchId = null, partsId = null,
                startedAtMs = System.currentTimeMillis() - 900_000,
                finishedAtMs = System.currentTimeMillis() - 870_000,
                sourcePath = null
            ),
            TransferUi(
                jobId = 9_005, name = "big-presentation.pptx", total = 50_000_000, done = 10_000_000,
                state = "failed", speedBps = 0, peer = "aa99881122334455",
                direction = "push", localRole = "source-push", progressPercent = 20,
                etaSecs = -1, failReason = "refused by peer", localPath = null,
                remoteDone = 0, instant = false,
                queuePos = null, batchId = null, partsId = null,
                startedAtMs = System.currentTimeMillis() - 1_200_000,
                finishedAtMs = System.currentTimeMillis() - 1_100_000,
                sourcePath = null
            )
        )
        _transfers.update { list ->
            val known = list.map { it.jobId }.toSet()
            list + demo.filter { it.jobId !in known }
        }
        _uiState.update { it.copy(transfers = _transfers.value) }
    }
}

// Extension function to convert TransferDto to TransferUi
private fun TransferDto.toUiModel(): TransferUi {
    return TransferUi(
        jobId = jobId.toLong(),
        name = name,
        total = total.toLong(),
        done = done.toLong(),
        state = state,
        speedBps = speedBps.toLong(),
        peer = peer,
        direction = direction,
        localRole = localRole,
        progressPercent = progressPercent.toInt(),
        etaSecs = etaSecs,
        failReason = failReason,
        localPath = localPath,
        remoteDone = remoteDone.toLong(),
        instant = instant,
        // M2 T5 契约消费:T4 起 queue_pos 运行期已填,children/parts/时间戳随卡透出
        queuePos = queuePos?.toInt(),
        batchId = batchId,
        children = children.map {
            ChildUi(jobId = it.jobId, name = it.name, total = it.total.toLong(), done = it.done.toLong(), state = it.state)
        },
        partsId = partsId,
        startedAtMs = startedAtMs,
        finishedAtMs = finishedAtMs,
        sourcePath = sourcePath
    )
}

/**
 * Factory for creating TransfersViewModel with FFI-based repository
 */
class TransfersViewModelFactory(
    private val bridge: LocalTransBridge
) : androidx.lifecycle.ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : androidx.lifecycle.ViewModel> create(modelClass: Class<T>): T {
        if (modelClass.isAssignableFrom(TransfersViewModel::class.java)) {
            val repo = com.localtrans.app.data.FfiTransfersRepo(bridge.app, bridge.events)
            val devices = com.localtrans.app.data.FfiDevicesRepo(bridge.app, bridge.events)
            return TransfersViewModel(repo, devicesRepo = devices) as T
        }
        throw IllegalArgumentException("Unknown ViewModel class")
    }
}
