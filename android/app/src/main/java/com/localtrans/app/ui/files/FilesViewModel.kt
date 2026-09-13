package com.localtrans.app.ui.files

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.localtrans.app.data.FilesRepo
import com.localtrans.app.LocalTransBridge
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.FileEntryDto
import uniffi.localtrans_ffi.FileOp

/**
 * Location type for file browsing
 */
enum class Location {
    LOCAL,
    REMOTE
}

/**
 * UI State for Files Screen
 */
data class FilesUiState(
    val location: Location = Location.LOCAL,
    val isLoading: Boolean = false,
    val selectedDeviceFp: String? = null,
    val selectedShareId: String? = null,
    val error: String? = null
)

/**
 * File item UI model
 */
data class FileEntryUi(
    val name: String,
    val isDir: Boolean,
    val size: Long,
    val modifiedMs: Long,
    val path: String
)

/**
 * ViewModel for Files screen
 *
 * @param defaultLocalRoot absolute path listed when the LOCAL tab is at root
 * (Android: /storage/emulated/0). Empty string keeps the old relative-path
 * behaviour, which lists nothing on Android — kept only for existing tests.
 */
class FilesViewModel(
    private val repo: FilesRepo,
    private val defaultLocalRoot: String = ""
) : ViewModel() {

    private val _uiState = MutableStateFlow(FilesUiState())
    val uiState: StateFlow<FilesUiState> = _uiState.asStateFlow()

    /** 全量列表(当前目录未过滤),展示列表由它 + 搜索词派生 */
    private val _allEntries = MutableStateFlow<List<FileEntryUi>>(emptyList())

    private val _entries = MutableStateFlow<List<FileEntryUi>>(emptyList())
    val entries: StateFlow<List<FileEntryUi>> = _entries.asStateFlow()

    private val _breadcrumb = MutableStateFlow<List<String>>(emptyList())
    val breadcrumb: StateFlow<List<String>> = _breadcrumb.asStateFlow()

    private val _searchQuery = MutableStateFlow("")
    val searchQuery: StateFlow<String> = _searchQuery.asStateFlow()

    private val _selectedEntries = MutableStateFlow<Set<String>>(emptySet())
    val selectedEntries: StateFlow<Set<String>> = _selectedEntries.asStateFlow()

    private val _shares = MutableStateFlow<List<uniffi.localtrans_ffi.ShareDto>>(emptyList())
    val shares: StateFlow<List<uniffi.localtrans_ffi.ShareDto>> = _shares.asStateFlow()

    private val currentPath: String
        get() = _breadcrumb.value.lastOrNull()
            ?: if (_uiState.value.location == Location.LOCAL) defaultLocalRoot else ""

    init {
        // Load initial entries
        loadEntries()

        // Listen for events
        viewModelScope.launch {
            repo.events.collect { event ->
                handleEvent(event)
            }
        }
    }

    private fun loadEntries() {
        viewModelScope.launch {
            _uiState.update { it.copy(isLoading = true) }
            try {
                when (_uiState.value.location) {
                    Location.LOCAL -> {
                        val path = currentPath
                        val entries = repo.listLocal(path)
                        _allEntries.value = entries.map { it.toUiModel(path) }
                    }
                    Location.REMOTE -> {
                        val fp = _uiState.value.selectedDeviceFp ?: ""
                        val shareId = getSelectedShareId()
                        val path = currentPath
                        if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                            val entries = repo.listRemote(fp, shareId, path)
                            _allEntries.value = entries.map { it.toUiModel(path) }
                        } else {
                            _allEntries.value = emptyList()
                        }
                    }
                }
                applyFilter(_searchQuery.value)
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            } finally {
                _uiState.update { it.copy(isLoading = false) }
            }
        }
    }

    private fun getSelectedShareId(): String {
        // Explicit selection wins; otherwise fall back to the first share
        return _uiState.value.selectedShareId
            ?: _shares.value.firstOrNull()?.shareId
            ?: ""
    }

    private fun handleEvent(event: AppEvent) {
        when (event) {
            is AppEvent.DevicesChanged -> {
                // Reload shares when devices change
                loadShares()
            }
            else -> {
                // Handle other events if needed
            }
        }
    }

    private fun loadShares() {
        viewModelScope.launch {
            try {
                val fp = _uiState.value.selectedDeviceFp ?: ""
                if (fp.isNotEmpty()) {
                    _shares.value = repo.remoteShares(fp)
                }
            } catch (e: Exception) {
                // Handle error
            }
        }
    }

    // User actions

    fun setLocation(location: Location) {
        _uiState.update { it.copy(location = location, selectedDeviceFp = null, selectedShareId = null) }
        _breadcrumb.value = emptyList()
        _selectedEntries.value = emptySet()
        loadEntries()
    }

    fun setSelectedDevice(fp: String) {
        _uiState.update { it.copy(selectedDeviceFp = fp, selectedShareId = null) }
        _breadcrumb.value = emptyList()
        _selectedEntries.value = emptySet()
        loadShares()
        loadEntries()
    }

    /** Pick a specific share of the selected device (REMOTE location). */
    fun setSelectedShare(shareId: String) {
        _uiState.update { it.copy(selectedShareId = shareId) }
        _breadcrumb.value = emptyList()
        _selectedEntries.value = emptySet()
        loadEntries()
    }

    /** Browse a device's shared folder: switch to REMOTE + pick device, auto-fallback to first share. */
    fun browseDeviceShare(fp: String) {
        _uiState.update { it.copy(location = Location.REMOTE, selectedDeviceFp = fp, selectedShareId = null) }
        _breadcrumb.value = emptyList()
        _selectedEntries.value = emptySet()
        loadShares()
        loadEntries()
    }

    fun navigateTo(path: String) {
        _breadcrumb.update { it + path }
        _selectedEntries.value = emptySet()
        loadEntries()
    }

    fun navigateUp() {
        if (_breadcrumb.value.isNotEmpty()) {
            _breadcrumb.update { it.dropLast(1) }
            _selectedEntries.value = emptySet()
            loadEntries()
        }
    }

    fun navigateToRoot() {
        _breadcrumb.value = emptyList()
        _selectedEntries.value = emptySet()
        loadEntries()
    }

    fun setSearchQuery(query: String) {
        _searchQuery.value = query
        // v0.11.0 低危批修复:此前过滤直接覆盖 _entries,退格恢复时列表越筛越小
        // (丢结果)。现在展示列表始终由全量 _allEntries 派生,过滤是纯函数。
        applyFilter(query)
    }

    /** 全量 → 展示:按搜索词过滤(空词 = 原样展示) */
    private fun applyFilter(query: String) {
        _entries.value = if (query.isEmpty()) {
            _allEntries.value
        } else {
            _allEntries.value.filter { it.name.contains(query, ignoreCase = true) }
        }
    }

    fun toggleSelection(path: String) {
        _selectedEntries.update { selected ->
            if (selected.contains(path)) {
                selected - path
            } else {
                selected + path
            }
        }
    }

    fun clearSelection() {
        _selectedEntries.value = emptySet()
    }

    /** 单文件直发用:清空现有选中后仅选中 path */
    fun selectOnly(path: String) {
        _selectedEntries.value = setOf(path)
    }

    /**
     * P3-T3 多选:全选当前展示列表里的文件(目录排除——批量拉取按单文件
     * start_pull 顺序执行,目录路径不可拉;搜索过滤后所见即所选)。
     */
    fun selectAll() {
        _selectedEntries.value = _entries.value.filter { !it.isDir }.map { it.path }.toSet()
    }

    fun createFolder(name: String) {
        viewModelScope.launch {
            try {
                val path = java.io.File(currentPath, name).path
                when (_uiState.value.location) {
                    Location.LOCAL -> {
                        repo.localOp(FileOp.MKDIR, path, "")
                    }
                    Location.REMOTE -> {
                        val fp = _uiState.value.selectedDeviceFp ?: ""
                        val shareId = getSelectedShareId()
                        repo.shareOp(fp, shareId, FileOp.MKDIR, path, "")
                    }
                }
                loadEntries()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun renameEntry(oldPath: String, newName: String) {
        viewModelScope.launch {
            try {
                when (_uiState.value.location) {
                    Location.LOCAL -> {
                        repo.localOp(FileOp.RENAME, oldPath, newName)
                    }
                    Location.REMOTE -> {
                        val fp = _uiState.value.selectedDeviceFp ?: ""
                        val shareId = getSelectedShareId()
                        repo.shareOp(fp, shareId, FileOp.RENAME, oldPath, newName)
                    }
                }
                loadEntries()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun deleteEntries(paths: List<String>) {
        viewModelScope.launch {
            try {
                when (_uiState.value.location) {
                    Location.LOCAL -> {
                        paths.forEach { path ->
                            repo.localOp(FileOp.DELETE, path, "")
                        }
                    }
                    Location.REMOTE -> {
                        val fp = _uiState.value.selectedDeviceFp ?: ""
                        val shareId = getSelectedShareId()
                        paths.forEach { path ->
                            repo.shareOp(fp, shareId, FileOp.DELETE, path, "")
                        }
                    }
                }
                clearSelection()
                loadEntries()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun pushFiles(fp: String, paths: List<String>) {
        viewModelScope.launch {
            try {
                repo.pushFiles(fp, paths)
                // Transfer started, navigation to Transfers screen can be handled by UI
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun pushFilesRel(fp: String, files: List<Pair<String, String>>) {
        viewModelScope.launch {
            try {
                repo.pushFilesRel(fp, files)
                // Transfer started, navigation to Transfers screen can be handled by UI
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun pullFiles(fp: String, shareId: String, remotePaths: List<String>) {
        viewModelScope.launch {
            try {
                repo.pullFiles(fp, shareId, remotePaths)
                clearSelection()
            } catch (e: Exception) {
                _uiState.update { it.copy(error = e.message) }
            }
        }
    }

    fun clearError() {
        _uiState.update { it.copy(error = null) }
    }
}

// Extension function to convert FileEntryDto to FileEntryUi
private fun FileEntryDto.toUiModel(currentPath: String): FileEntryUi {
    return FileEntryUi(
        name = name,
        isDir = isDir,
        size = size.toLong(),
        modifiedMs = modifiedMs.toLong(),
        path = java.io.File(currentPath, name).path
    )
}

/**
 * Factory for creating FilesViewModel with FFI-based repository
 */
class FilesViewModelFactory(
    private val bridge: LocalTransBridge
) : androidx.lifecycle.ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : androidx.lifecycle.ViewModel> create(modelClass: Class<T>): T {
        if (modelClass.isAssignableFrom(FilesViewModel::class.java)) {
            val repo = com.localtrans.app.data.FfiFilesRepo(bridge.app, bridge.events)
            // Android 真实根:外部存储根目录(权限授予后可列出用户可见文件)
            val root = android.os.Environment.getExternalStorageDirectory().absolutePath
            return FilesViewModel(repo, defaultLocalRoot = root) as T
        }
        throw IllegalArgumentException("Unknown ViewModel class")
    }
}
