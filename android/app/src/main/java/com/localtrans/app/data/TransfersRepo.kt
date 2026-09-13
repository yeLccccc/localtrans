package com.localtrans.app.data

import kotlinx.coroutines.flow.SharedFlow
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.DiskJobDto
import uniffi.localtrans_ffi.TransferDto

/**
 * Repository interface for transfer operations
 */
interface TransfersRepo {
    /**
     * Get list of all transfers
     */
    suspend fun transfers(): List<TransferDto>

    /**
     * Pause a transfer
     */
    suspend fun pause(jobId: kotlin.ULong)

    /**
     * Resume a transfer
     */
    suspend fun resume(jobId: kotlin.ULong)

    /**
     * Cancel a transfer
     */
    suspend fun cancel(jobId: kotlin.ULong)

    /**
     * Remove a transfer record (terminal rows removed directly;
     * active rows are cancelled first then removed)
     */
    suspend fun transferRemove(jobId: kotlin.ULong)

    /**
     * 两级删除(对齐桌面壳 remove_transfer level):
     * - "view":仅终态卡——removed=true 保留表内,可在磁盘历史找回;
     * - "destroy":终态卡实删(连带 parts);活动卡取消仲裁后收尾(parts 保留)。
     * 返回是否发生删除动作(false=卡片不存在)。
     */
    suspend fun transferRemoveLevel(jobId: kotlin.ULong, level: kotlin.String): kotlin.Boolean

    /**
     * 磁盘历史入口:全部 parts 根 manifest(含视图已移除)+表内无 parts 终态卡
     */
    suspend fun listDiskJobs(): List<DiskJobDto>

    /**
     * 恢复磁盘历史任务到视图(removed=false;孤儿按重建逻辑建卡)
     */
    suspend fun restoreDiskJob(jobId: kotlin.ULong)

    /**
     * 彻底删除磁盘历史任务(在表 finalize_destroy;纯孤儿直接删 parts)
     */
    suspend fun destroyDiskJob(jobId: kotlin.ULong)

    /**
     * Remove all terminal transfer records (done/failed/interrupted).
     * Returns removed count.
     */
    suspend fun transfersClearFinished(): kotlin.ULong

    /**
     * Retry a failed/interrupted transfer
     */
    suspend fun retryTransfer(jobId: kotlin.ULong): kotlin.ULong

    /**
     * Respond to an offer request
     */
    suspend fun respondOffer(jobId: kotlin.ULong, accept: kotlin.Boolean)

    /**
     * Event flow from FFI layer
     */
    val events: SharedFlow<AppEvent>
}

/**
 * FFI-based implementation of TransfersRepo
 */
class FfiTransfersRepo(
    private val app: uniffi.localtrans_ffi.LocalTransApp,
    eventFlow: SharedFlow<AppEvent>
) : TransfersRepo {

    override val events: SharedFlow<AppEvent> = eventFlow

    override suspend fun transfers(): List<TransferDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.transfers() }

    override suspend fun pause(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.pause(jobId) }

    override suspend fun resume(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.resume(jobId) }

    override suspend fun cancel(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.cancel(jobId) }

    override suspend fun transferRemove(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.transferRemove(jobId) }

    override suspend fun transferRemoveLevel(jobId: kotlin.ULong, level: kotlin.String): kotlin.Boolean =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            app.transferRemoveLevel(jobId, level)
        }

    override suspend fun listDiskJobs(): List<DiskJobDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.listDiskJobs() }

    override suspend fun restoreDiskJob(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.restoreDiskJob(jobId) }

    override suspend fun destroyDiskJob(jobId: kotlin.ULong) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.destroyDiskJob(jobId) }

    override suspend fun transfersClearFinished(): kotlin.ULong =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.transfersClearFinished() }

    override suspend fun retryTransfer(jobId: kotlin.ULong): kotlin.ULong =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.retryTransfer(jobId) }

    override suspend fun respondOffer(jobId: kotlin.ULong, accept: kotlin.Boolean) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.respondOffer(jobId, accept) }
}

/**
 * Fake implementation for testing
 */
class FakeTransfersRepo : TransfersRepo {
    private val _transfers = mutableListOf<TransferDto>()
    private val _events = kotlinx.coroutines.flow.MutableSharedFlow<AppEvent>(extraBufferCapacity = 256)

    override val events: SharedFlow<AppEvent> = _events

    override suspend fun transfers(): List<TransferDto> = _transfers.toList()

    override suspend fun pause(jobId: kotlin.ULong) {
        // Update transfer state to paused
        _transfers.find { it.jobId == jobId }?.let {
            val index = _transfers.indexOf(it)
            _transfers[index] = it.copy(state = "paused")
        }
    }

    override suspend fun resume(jobId: kotlin.ULong) {
        // Update transfer state to running
        _transfers.find { it.jobId == jobId }?.let {
            val index = _transfers.indexOf(it)
            _transfers[index] = it.copy(state = "running")
        }
    }

    override suspend fun cancel(jobId: kotlin.ULong) {
        // Remove from list
        _transfers.removeIf { it.jobId == jobId }
    }

    override suspend fun transferRemove(jobId: kotlin.ULong) {
        _transfers.removeIf { it.jobId == jobId }
    }

    // 两级删除记录(单测断言用):jobId -> level
    val removeLevelCalls = mutableListOf<Pair<kotlin.ULong, kotlin.String>>()

    override suspend fun transferRemoveLevel(
        jobId: kotlin.ULong,
        level: kotlin.String
    ): kotlin.Boolean {
        removeLevelCalls.add(jobId to level)
        if (level == "view") {
            // view 级只对终态卡有意义(FFI 同款裁定:活动卡报错)
            val card = _transfers.find { it.jobId == jobId } ?: return false
            if (card.state !in setOf("done", "failed", "interrupted")) return false
        }
        _transfers.removeIf { it.jobId == jobId }
        return true
    }

    // 磁盘历史(单测用内存实现)
    private val _diskJobs = mutableListOf<DiskJobDto>()

    override suspend fun listDiskJobs(): List<DiskJobDto> = _diskJobs.toList()

    fun setDiskJobs(jobs: List<DiskJobDto>) {
        _diskJobs.clear()
        _diskJobs.addAll(jobs)
    }

    override suspend fun restoreDiskJob(jobId: kotlin.ULong) {
        _diskJobs.removeAll { it.jobId == jobId }
    }

    override suspend fun destroyDiskJob(jobId: kotlin.ULong) {
        _diskJobs.removeAll { it.jobId == jobId }
    }

    override suspend fun transfersClearFinished(): kotlin.ULong {
        val before = _transfers.size
        _transfers.removeIf { it.state in setOf("done", "failed", "interrupted") }
        return (before - _transfers.size).toULong()
    }

    override suspend fun retryTransfer(jobId: kotlin.ULong): kotlin.ULong {
        // Return same job ID for retry
        return jobId
    }

    override suspend fun respondOffer(jobId: kotlin.ULong, accept: kotlin.Boolean) {
        // No-op for testing
    }

    /**
     * Helper method to emit events for testing
     */
    suspend fun emitEvent(event: AppEvent) {
        _events.emit(event)
    }

    /**
     * Helper method to add transfers for testing
     */
    fun addTransfer(transfer: TransferDto) {
        _transfers.add(transfer)
    }
}
