package com.localtrans.app.data

import kotlinx.coroutines.flow.SharedFlow
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.FileEntryDto
import uniffi.localtrans_ffi.FileOp
import uniffi.localtrans_ffi.ShareDto

/**
 * Repository interface for file operations
 */
interface FilesRepo {
    /**
     * List local directory
     */
    suspend fun listLocal(dir: String): List<FileEntryDto>

    /**
     * List remote directory
     */
    suspend fun listRemote(fp: String, shareId: String, path: String): List<FileEntryDto>

    /**
     * Get list of remote shares
     */
    suspend fun remoteShares(fp: String): List<ShareDto>

    /**
     * Perform local file operation
     */
    suspend fun localOp(op: FileOp, path: String, newName: String)

    /**
     * Perform file operation on remote share
     */
    suspend fun shareOp(fp: String, shareId: String, op: FileOp, path: String, newName: String): String

    /**
     * Push files to remote device
     */
    suspend fun pushFiles(fp: String, paths: List<String>): kotlin.ULong

    /**
     * Push files with relative dirs (folder structure preserved)
     */
    suspend fun pushFilesRel(fp: String, files: List<Pair<String, String>>): kotlin.ULong

    /**
     * Pull files from remote share
     */
    suspend fun pullFiles(fp: String, remoteShareId: String, remotePaths: List<String>): kotlin.ULong

    /**
     * Event flow from FFI layer
     */
    val events: SharedFlow<AppEvent>
}

/**
 * FFI-based implementation of FilesRepo
 */
class FfiFilesRepo(
    private val app: uniffi.localtrans_ffi.LocalTransApp,
    eventFlow: SharedFlow<AppEvent>
) : FilesRepo {

    override val events: SharedFlow<AppEvent> = eventFlow

    override suspend fun listLocal(dir: String): List<FileEntryDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.listLocal(dir) }

    override suspend fun listRemote(fp: String, shareId: String, path: String): List<FileEntryDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.listRemote(fp, shareId, path) }

    override suspend fun remoteShares(fp: String): List<ShareDto> =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.remoteShares(fp) }

    override suspend fun localOp(op: FileOp, path: String, newName: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.localOp(op, path, newName) }

    override suspend fun shareOp(fp: String, shareId: String, op: FileOp, path: String, newName: String): String =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.shareOp(fp, shareId, op, path, newName) }

    override suspend fun pushFiles(fp: String, paths: List<String>): kotlin.ULong =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.pushFiles(fp, paths) }

    override suspend fun pushFilesRel(fp: String, files: List<Pair<String, String>>): kotlin.ULong =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            val dtos = files.map { uniffi.localtrans_ffi.PushFileDto(path = it.first, relDir = it.second) }
            app.pushFilesRel(fp, dtos)
        }

    override suspend fun pullFiles(fp: String, remoteShareId: String, remotePaths: List<String>): kotlin.ULong =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.pullFiles(fp, remoteShareId, remotePaths) }
}

/**
 * Fake implementation for testing
 */
class FakeFilesRepo : FilesRepo {
    private val _localEntries = mutableListOf<FileEntryDto>()
    private val _remoteShares = mutableListOf<ShareDto>()
    private val _events = kotlinx.coroutines.flow.MutableSharedFlow<AppEvent>(extraBufferCapacity = 256)

    /** Last directory passed to listLocal (for assertions in tests) */
    var lastLocalDir: String? = null
        private set

    /** Track pullFiles calls for testing */
    val pulled = mutableListOf<Triple<String, String, List<String>>>()

    override val events: SharedFlow<AppEvent> = _events

    override suspend fun listLocal(dir: String): List<FileEntryDto> {
        lastLocalDir = dir
        return _localEntries.toList()
    }

    override suspend fun listRemote(fp: String, shareId: String, path: String): List<FileEntryDto> {
        // Return empty list for testing
        return emptyList()
    }

    override suspend fun remoteShares(fp: String): List<ShareDto> = _remoteShares.toList()

    override suspend fun localOp(op: FileOp, path: String, newName: String) {
        // No-op for testing
    }

    override suspend fun shareOp(fp: String, shareId: String, op: FileOp, path: String, newName: String): String {
        // Return empty string for testing
        return ""
    }

    override suspend fun pushFiles(fp: String, paths: List<String>): kotlin.ULong {
        // Return dummy job ID
        return 1u
    }

    override suspend fun pushFilesRel(fp: String, files: List<Pair<String, String>>): kotlin.ULong {
        // Return dummy job ID
        return 1u
    }

    override suspend fun pullFiles(fp: String, remoteShareId: String, remotePaths: List<String>): kotlin.ULong {
        pulled.add(Triple(fp, remoteShareId, remotePaths))
        // Return dummy job ID
        return 1u
    }

    /**
     * Helper method to emit events for testing
     */
    suspend fun emitEvent(event: AppEvent) {
        _events.emit(event)
    }

    /**
     * Helper method to add local entries for testing
     */
    fun addLocalEntry(entry: FileEntryDto) {
        _localEntries.add(entry)
    }

    /**
     * Helper method to emit local changed event
     */
    suspend fun emitLocalChanged() {
        _events.emit(AppEvent.DevicesChanged) // Reuse existing event as placeholder
    }
}
