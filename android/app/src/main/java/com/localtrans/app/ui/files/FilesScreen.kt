package com.localtrans.app.ui.files

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarResult
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.localtrans.app.media.MediaFilter
import com.localtrans.app.media.MediaPickerViewModel
import com.localtrans.app.media.MediaPickerViewModelFactory
import com.localtrans.app.media.MediaStoreRepo
import com.localtrans.app.util.Formatters
import uniffi.localtrans_ffi.DeviceDto
import android.os.Environment
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * Local secondary tabs
 */
enum class LocalTab {
    PHOTOS,
    VIDEOS,
    DOCS,
    ALL
}

@Composable
fun FilesScreen(
    filesViewModel: FilesViewModel,
    onNavigateToTransfers: () -> Unit = {},
    modifier: Modifier = Modifier
) {
    val uiState by filesViewModel.uiState.collectAsState()
    val entries by filesViewModel.entries.collectAsState()
    val breadcrumb by filesViewModel.breadcrumb.collectAsState()
    val searchQuery by filesViewModel.searchQuery.collectAsState()
    val selectedEntries by filesViewModel.selectedEntries.collectAsState()
    val shares by filesViewModel.shares.collectAsState()

    val context = LocalContext.current
    val coroutineScope = rememberCoroutineScope()

    // Secondary tab state for LOCAL location
    var localTab by remember { mutableStateOf(LocalTab.ALL) }

    // Media picker view model for photos/videos
    val mediaViewModel: MediaPickerViewModel = viewModel(
        factory = MediaPickerViewModelFactory(MediaStoreRepo(context))
    )
    val mediaItems by mediaViewModel.items.collectAsState()
    val mediaSelected by mediaViewModel.selected.collectAsState()

    // Docs tab state
    val docsRoots = remember {
        listOf(
            Environment.getExternalStorageDirectory().resolve("Download"),
            Environment.getExternalStorageDirectory().resolve("Documents"),
            Environment.getExternalStorageDirectory().resolve("DCIM")
        )
    }
    var docsFiles by remember { mutableStateOf<List<DocItem>>(emptyList()) }
    var docsRev by remember { mutableIntStateOf(0) }
    LaunchedEffect(docsRev) {
        withContext(Dispatchers.IO) { docsFiles = collectDocs(docsRoots) }
    }
    fun refreshDocs() { docsRev++ }
    var docsSelected by remember { mutableStateOf<Set<String>>(emptySet()) }

    // Device selection sheet state
    var showDeviceSheet by remember { mutableStateOf(false) }

    // Runtime permission re-request
    val permLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestMultiplePermissions()
    ) { /* FilesScreen recomposes on resume */ }

    // Show rename dialog
    var showRenameDialog by remember { mutableStateOf(false) }
    var renamePath by remember { mutableStateOf("") }
    var renameOldName by remember { mutableStateOf("") }

    // Show delete confirmation
    var showDeleteDialog by remember { mutableStateOf(false) }
    var deletePaths by remember { mutableStateOf(emptyList<String>()) }

    // Show create folder dialog
    var showCreateFolderDialog by remember { mutableStateOf(false) }

    // 长按菜单状态
    var menuEntry by remember { mutableStateOf<FileEntryUi?>(null) }

    // Helper to open local menu for media items (constructs FileEntryUi from path)
    fun openLocalMenu(path: String) {
        menuEntry = FileEntryUi(
            name = java.io.File(path).name,
            isDir = false,
            size = 0L,
            modifiedMs = 0L,
            path = path
        )
    }

    // Snackbar host state
    val snackbarHostState = remember { SnackbarHostState() }

    // Send confirmation callback
    fun onSendSuccess(message: String = "已开始发送") {
        mediaViewModel.clearSelection()
        docsSelected = emptySet()
        filesViewModel.clearSelection()
        coroutineScope.launch {
            snackbarHostState.showSnackbar(message)
        }
    }

    Box(modifier = modifier.fillMaxSize()) {
        Column(
            modifier = Modifier.fillMaxSize()
        ) {
            // Primary location tabs (本机/远程)
            LocationTabs(
                currentLocation = uiState.location,
                onLocationChange = {
                    filesViewModel.setLocation(it)
                    localTab = LocalTab.ALL // Reset to ALL on location change
                }
            )

            // LOCAL location: add secondary tabs
            if (uiState.location == Location.LOCAL) {
                LocalSecondaryTabs(
                    currentTab = localTab,
                    onTabChange = {
                        localTab = it
                        // Clear selections when switching tabs
                        mediaViewModel.clearSelection()
                        docsSelected = emptySet()
                        filesViewModel.clearSelection()
                    }
                )

                // Permission guide
                if (!com.localtrans.app.util.StoragePermissions.hasBrowsingPermissions(context)) {
                    PermissionGuide(
                        onRequestPermission = {
                            if (android.os.Build.VERSION.SDK_INT >= 30) {
                                context.startActivity(
                                    com.localtrans.app.util.StoragePermissions.allFilesAccessIntent(context)
                                )
                            } else {
                                permLauncher.launch(
                                    com.localtrans.app.util.StoragePermissions.requiredForBrowsing(context)
                                )
                            }
                        }
                    )
                    return@Column
                }

                // Secondary tab content
                when (localTab) {
                    LocalTab.PHOTOS -> {
                        mediaViewModel.setFilter(MediaFilter.PHOTOS)
                        Box(modifier = Modifier.fillMaxSize()) {
                            MediaGridTab(
                                items = mediaItems,
                                selected = mediaSelected,
                                onToggle = { mediaViewModel.toggleSelect(it) },
                                onLongClick = { path -> openLocalMenu(path) }
                            )
                            if (mediaSelected.isNotEmpty()) {
                                UnifiedSelectionBar(
                                    selectedCount = mediaSelected.size,
                                    totalBytes = mediaItems.filter { it.id in mediaSelected }.sumOf { it.size },
                                    onSend = { showDeviceSheet = true },
                                    onRename = {
                                        val p = mediaViewModel.selectedPaths().first()
                                        renamePath = p
                                        renameOldName = java.io.File(p).name
                                        showRenameDialog = true
                                    },
                                    onDelete = {
                                        deletePaths = mediaViewModel.selectedPaths()
                                        showDeleteDialog = true
                                    },
                                    onClear = { mediaViewModel.clearSelection() }
                                )
                            }
                        }
                    }
                    LocalTab.VIDEOS -> {
                        mediaViewModel.setFilter(MediaFilter.VIDEOS)
                        Box(modifier = Modifier.fillMaxSize()) {
                            MediaGridTab(
                                items = mediaItems,
                                selected = mediaSelected,
                                onToggle = { mediaViewModel.toggleSelect(it) },
                                onLongClick = { path -> openLocalMenu(path) }
                            )
                            if (mediaSelected.isNotEmpty()) {
                                UnifiedSelectionBar(
                                    selectedCount = mediaSelected.size,
                                    totalBytes = mediaItems.filter { it.id in mediaSelected }.sumOf { it.size },
                                    onSend = { showDeviceSheet = true },
                                    onRename = {
                                        val p = mediaViewModel.selectedPaths().first()
                                        renamePath = p
                                        renameOldName = java.io.File(p).name
                                        showRenameDialog = true
                                    },
                                    onDelete = {
                                        deletePaths = mediaViewModel.selectedPaths()
                                        showDeleteDialog = true
                                    },
                                    onClear = { mediaViewModel.clearSelection() }
                                )
                            }
                        }
                    }
                    LocalTab.DOCS -> {
                        Box(modifier = Modifier.fillMaxSize()) {
                            DocsTab(
                                files = docsFiles,
                                selected = docsSelected,
                                onToggle = { path ->
                                    docsSelected = if (path in docsSelected) docsSelected - path
                                    else docsSelected + path
                                },
                                onLongClick = { path -> openLocalMenu(path) }
                            )
                            if (docsSelected.isNotEmpty()) {
                                UnifiedSelectionBar(
                                    selectedCount = docsSelected.size,
                                    totalBytes = docsFiles.filter { it.file.absolutePath in docsSelected }.sumOf { it.sizeBytes },
                                    onSend = { showDeviceSheet = true },
                                    onRename = {
                                        val p = docsFiles.first { it.file.absolutePath in docsSelected }.file.absolutePath
                                        renamePath = p
                                        renameOldName = java.io.File(p).name
                                        showRenameDialog = true
                                    },
                                    onDelete = {
                                        deletePaths = docsFiles.filter { it.file.absolutePath in docsSelected }.map { it.file.absolutePath }
                                        showDeleteDialog = true
                                    },
                                    onClear = { docsSelected = emptySet() }
                                )
                            }
                        }
                    }
                    LocalTab.ALL -> {
                        // Existing directory browser (preserved 100%)
                        BreadcrumbRow(
                            breadcrumb = breadcrumb,
                            onNavigateUp = { filesViewModel.navigateUp() },
                            onNavigateToRoot = { filesViewModel.navigateToRoot() }
                        )

                        SearchBar(
                            query = searchQuery,
                            onQueryChange = { filesViewModel.setSearchQuery(it) }
                        )

                        if (uiState.isLoading) {
                            Box(
                                modifier = Modifier.fillMaxSize(),
                                contentAlignment = Alignment.Center
                            ) {
                                CircularProgressIndicator()
                            }
                        } else {
                            // P3-T3:同远程页——weight(1f) 防止 LazyColumn 挤掉底部多选栏
                            LazyColumn(
                                modifier = Modifier.fillMaxWidth().weight(1f),
                                contentPadding = PaddingValues(16.dp),
                                verticalArrangement = Arrangement.spacedBy(4.dp)
                            ) {
                                items(entries) { entry ->
                                    FileEntryCard(
                                        entry = entry,
                                        isSelected = selectedEntries.contains(entry.path),
                                        onClick = {
                                            when {
                                                selectedEntries.isNotEmpty() ->
                                                    filesViewModel.toggleSelection(entry.path)
                                                entry.isDir ->
                                                    filesViewModel.navigateTo(entry.path)
                                                else ->
                                                    // v0.10.1 单击文件=进入多选(与媒体 Tab 心智一致)
                                                    filesViewModel.toggleSelection(entry.path)
                                            }
                                        },
                                        onLongClick = { menuEntry = entry }
                                    )
                                }
                            }
                        }
                    }
                }

                // UnifiedSelectionBar for ALL tab (placed at bottom of Box via Box scope)
                if (localTab == LocalTab.ALL && selectedEntries.isNotEmpty()) {
                    val totalBytes = entries
                        .filter { it.path in selectedEntries }
                        .sumOf { it.size }
                    UnifiedSelectionBar(
                        selectedCount = selectedEntries.size,
                        totalBytes = totalBytes,
                        onSend = { showDeviceSheet = true },
                        onRename = {
                            val p = selectedEntries.first()
                            renamePath = p
                            renameOldName = p.split("/").last()
                            showRenameDialog = true
                        },
                        onDelete = {
                            deletePaths = selectedEntries.toList()
                            showDeleteDialog = true
                        },
                        onClear = { filesViewModel.clearSelection() }
                    )
                }
            } else {
                // REMOTE location: pick a connected device first, then browse its shares
                if (uiState.selectedDeviceFp == null) {
                    RemoteDevicePicker(
                        onDeviceSelected = { fp -> filesViewModel.setSelectedDevice(fp) }
                    )
                } else {
                    BreadcrumbRow(
                        breadcrumb = breadcrumb,
                        onNavigateUp = { filesViewModel.navigateUp() },
                        onNavigateToRoot = { filesViewModel.navigateToRoot() }
                    )

                    // Share switcher (device may expose multiple shares)
                    val currentShareId = uiState.selectedShareId
                        ?: shares.firstOrNull()?.shareId
                    if (shares.size > 1) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 8.dp),
                            horizontalArrangement = Arrangement.spacedBy(8.dp)
                        ) {
                            shares.forEach { share ->
                                FilterChip(
                                    selected = share.shareId == currentShareId,
                                    onClick = { filesViewModel.setSelectedShare(share.shareId) },
                                    label = { Text(share.alias.ifEmpty { share.shareId }) }
                                )
                            }
                        }
                    }

                    if (shares.isEmpty() && !uiState.isLoading) {
                        Box(
                            modifier = Modifier.fillMaxSize(),
                            contentAlignment = Alignment.Center
                        ) {
                            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                Text(
                                    "对方尚未共享文件夹",
                                    style = MaterialTheme.typography.bodyMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant
                                )
                                Spacer(modifier = Modifier.height(8.dp))
                                TextButton(onClick = { filesViewModel.setLocation(Location.REMOTE) }) {
                                    Text("换个设备")
                                }
                            }
                        }
                    } else {
                        if (uiState.isLoading) {
                            Box(
                                modifier = Modifier.fillMaxSize(),
                                contentAlignment = Alignment.Center
                            ) {
                                CircularProgressIndicator()
                            }
                        } else {
                            // P3-T3:weight(1f) 而非 fillMaxSize——否则 LazyColumn 独占
                            // 全部剩余高度,把底部多选栏挤到屏外(多选栏永不现形)
                            LazyColumn(
                                modifier = Modifier.fillMaxWidth().weight(1f),
                                contentPadding = PaddingValues(16.dp),
                                verticalArrangement = Arrangement.spacedBy(4.dp)
                            ) {
                                items(entries) { entry ->
                                    FileEntryCard(
                                        entry = entry,
                                        isSelected = selectedEntries.contains(entry.path),
                                        onClick = {
                                            when {
                                                selectedEntries.isNotEmpty() ->
                                                    filesViewModel.toggleSelection(entry.path)
                                                entry.isDir ->
                                                    filesViewModel.navigateTo(entry.path)
                                                else -> {
                                                    // 单击远程文件直拉(一步入口)
                                                    val fp = uiState.selectedDeviceFp ?: ""
                                                    val shareId = currentShareId ?: ""
                                                    if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                                                        filesViewModel.pullFiles(fp, shareId, listOf(entry.path))
                                                        coroutineScope.launch {
                                                            val result = snackbarHostState.showSnackbar(
                                                                message = "已开始下载 ${entry.name}",
                                                                actionLabel = "查看",
                                                                duration = SnackbarDuration.Short
                                                            )
                                                            if (result == SnackbarResult.ActionPerformed) {
                                                                onNavigateToTransfers()
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        },
                                        onLongClick = { menuEntry = entry }
                                    )
                                }
                            }
                        }

                        if (selectedEntries.isNotEmpty()) {
                            // P3-T3:远程多选——全选 + 「下载(N)」批量拉取
                            // (pull_files 多路径聚合单卡,FFI N1-T3 语义)
                            val totalBytes = entries
                                .filter { it.path in selectedEntries }
                                .sumOf { it.size }
                            UnifiedSelectionBar(
                                selectedCount = selectedEntries.size,
                                totalBytes = totalBytes,
                                onSend = {
                                    val fp = uiState.selectedDeviceFp ?: ""
                                    val shareId = currentShareId ?: ""
                                    if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                                        filesViewModel.pullFiles(fp, shareId, selectedEntries.toList())
                                        onSendSuccess("已开始下载")
                                    }
                                },
                                onRename = {},
                                onDelete = {},
                                onClear = { filesViewModel.clearSelection() },
                                isRemote = true,
                                onSelectAll = { filesViewModel.selectAll() }
                            )
                        }
                    }
                }
            }
        }

        // FAB for creating folder (only in ALL tab, no selection) - in Box scope
        if (uiState.location == Location.LOCAL && localTab == LocalTab.ALL && selectedEntries.isEmpty()) {
            FloatingActionButton(
                // M7:files-create-folder-fab
                onClick = { showCreateFolderDialog = true },
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .padding(16.dp)
                    .testTag("files-create-folder-fab")
            ) {
                Icon(Icons.Default.CreateNewFolder, "新建文件夹")
            }
        }

        // Device selection sheet for sending
        if (showDeviceSheet) {
            DeviceSelectionSheet(
                onDismiss = { showDeviceSheet = false },
                onDeviceSelected = { deviceFp ->
                    showDeviceSheet = false
                    handleSend(
                        localTab = localTab,
                        deviceFp = deviceFp,
                        filesViewModel = filesViewModel,
                        mediaViewModel = mediaViewModel,
                        docsFiles = docsFiles,
                        docsSelected = docsSelected,
                        onSuccess = { onSendSuccess() }
                    )
                }
            )
        }

        // Snackbar host
        SnackbarHost(
            hostState = snackbarHostState,
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(16.dp)
        )
    }

    // Rename dialog
    if (showRenameDialog) {
        RenameDialog(
            oldName = renameOldName,
            onDismiss = { showRenameDialog = false },
            onConfirm = { newName ->
                filesViewModel.renameEntry(renamePath, newName)
                showRenameDialog = false
                // v0.10.1 按当前 Tab 刷新数据源(媒体重查 MediaStore/文档重扫/全部重列目录)
                when (localTab) {
                    LocalTab.PHOTOS, LocalTab.VIDEOS -> mediaViewModel.refresh()
                    LocalTab.DOCS -> refreshDocs()
                    LocalTab.ALL -> { /* deleteEntries 内部已 loadEntries */ }
                }
            }
        )
    }

    // Delete confirmation dialog
    if (showDeleteDialog) {
        DeleteConfirmationDialog(
            count = deletePaths.size,
            onDismiss = { showDeleteDialog = false },
            onConfirm = {
                filesViewModel.deleteEntries(deletePaths)
                showDeleteDialog = false
                // v0.10.1 按当前 Tab 刷新数据源(媒体重查 MediaStore/文档重扫/全部重列目录)
                when (localTab) {
                    LocalTab.PHOTOS, LocalTab.VIDEOS -> mediaViewModel.refresh()
                    LocalTab.DOCS -> refreshDocs()
                    LocalTab.ALL -> { /* deleteEntries 内部已 loadEntries */ }
                }
                mediaViewModel.clearSelection()
                docsSelected = emptySet()
            }
        )
    }

    // Create folder dialog
    if (showCreateFolderDialog) {
        CreateFolderDialog(
            onDismiss = { showCreateFolderDialog = false },
            onConfirm = { name ->
                filesViewModel.createFolder(name)
                showCreateFolderDialog = false
            }
        )
    }

    // 长按菜单
    menuEntry?.let { entry ->
        val isRemotePage = uiState.location == Location.REMOTE
        FileEntryMenuSheet(
            isRemote = isRemotePage,
            onDismiss = { menuEntry = null },
            onDownload = {
                val fp = uiState.selectedDeviceFp ?: ""
                val shareId = shares.firstOrNull()?.shareId
                    ?: uiState.selectedShareId ?: ""
                if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                    filesViewModel.pullFiles(fp, shareId, listOf(entry.path))
                }
                menuEntry = null
            },
            onSend = {
                val path = entry.path
                menuEntry = null
                when (localTab) {
                    LocalTab.PHOTOS, LocalTab.VIDEOS -> {
                        mediaViewModel.selectOnly(path)
                        showDeviceSheet = true
                    }
                    LocalTab.DOCS -> {
                        docsSelected = setOf(path)
                        showDeviceSheet = true
                    }
                    LocalTab.ALL -> {
                        filesViewModel.selectOnly(path)
                        showDeviceSheet = true
                    }
                }
            },
            onRename = {
                renamePath = entry.path
                renameOldName = entry.name
                menuEntry = null
                showRenameDialog = true
            },
            onDelete = {
                deletePaths = listOf(entry.path)
                menuEntry = null
                showDeleteDialog = true
            },
            onSelectMultiple = {
                filesViewModel.toggleSelection(entry.path)
                menuEntry = null
            }
        )
    }

    // Error message
    uiState.error?.let { error ->
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp)
        ) {
            Snackbar {
                Text(error)
            }
        }
    }
}

/**
 * Handle send action based on current tab
 */
private fun handleSend(
    localTab: LocalTab,
    deviceFp: String,
    filesViewModel: FilesViewModel,
    mediaViewModel: MediaPickerViewModel,
    docsFiles: List<DocItem>,
    docsSelected: Set<String>,
    onSuccess: () -> Unit
) {
    // T7: 整树遍历可能很慢,移到 IO 线程避免主线程 ANR
    CoroutineScope(Dispatchers.IO).launch {
        when (val payload = buildSendPayload(localTab, filesViewModel, mediaViewModel, docsFiles, docsSelected)) {
            is SendPayload.Plain -> filesViewModel.pushFiles(deviceFp, payload.paths)
            is SendPayload.FolderTree -> filesViewModel.pushFilesRel(deviceFp, payload.files)
        }
        onSuccess()
    }
}

/** 发送载荷:普通文件列表 vs 带相对目录的文件夹展开 */
private sealed class SendPayload {
    data class Plain(val paths: List<String>) : SendPayload()
    data class FolderTree(val files: List<Pair<String, String>>) : SendPayload()
}

/** 在 IO 线程构建发送载荷(含目录整树遍历) */
private fun buildSendPayload(
    localTab: LocalTab,
    filesViewModel: FilesViewModel,
    mediaViewModel: MediaPickerViewModel,
    docsFiles: List<DocItem>,
    docsSelected: Set<String>
): SendPayload = when (localTab) {
    LocalTab.PHOTOS, LocalTab.VIDEOS ->
        // Send media files using pushFiles
        SendPayload.Plain(mediaViewModel.selectedPaths())
    LocalTab.DOCS ->
        // Send docs using pushFiles (get selected docs paths)
        SendPayload.Plain(
            docsFiles.filter { it.file.absolutePath in docsSelected }.map { it.file.absolutePath }
        )
    LocalTab.ALL -> {
        // Send with folder structure using pushFilesRel
        val selectedEntries = filesViewModel.selectedEntries.value
        val filesWithRelDir = mutableListOf<Pair<String, String>>()

        // Current browse root
        val currentPath = filesViewModel.breadcrumb.value.lastOrNull()
            ?: android.os.Environment.getExternalStorageDirectory().absolutePath

        for (path in selectedEntries) {
            val file = File(path)
            if (file.isDirectory) {
                // Walk folder and expand with relative paths
                file.walk().filter { it.isFile }.forEach { childFile ->
                    val relDir = childFile.absolutePath.substring(currentPath.length)
                        .removePrefix("/")
                        .split("/")
                        .dropLast(1)
                        .joinToString("/")
                    filesWithRelDir.add(childFile.absolutePath to relDir)
                }
            } else {
                // Single file: rel_dir = parent relative to browse root
                val parentRel = file.parent?.substring(currentPath.length)
                    ?.removePrefix("/")
                    ?.let { if (it.isEmpty()) "" else it }
                    ?: ""
                filesWithRelDir.add(path to parentRel)
            }
        }

        SendPayload.FolderTree(filesWithRelDir)
    }
}

@Composable
fun LocalSecondaryTabs(
    currentTab: LocalTab,
    onTabChange: (LocalTab) -> Unit
) {
    TabRow(selectedTabIndex = currentTab.ordinal) {
        LocalTab.values().forEach { tab ->
            Tab(
                selected = currentTab == tab,
                onClick = { onTabChange(tab) },
                text = {
                    Text(
                        when (tab) {
                            LocalTab.PHOTOS -> "相册"
                            LocalTab.VIDEOS -> "视频"
                            LocalTab.DOCS -> "文档"
                            LocalTab.ALL -> "全部"
                        }
                    )
                }
            )
        }
    }
}

@Composable
fun PermissionGuide(
    onRequestPermission: () -> Unit
) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        contentAlignment = Alignment.Center
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = "未授予存储权限，无法浏览本地文件",
                style = MaterialTheme.typography.bodyMedium
            )
            Spacer(modifier = Modifier.height(12.dp))
            Button(onClick = onRequestPermission) {
                Text("授予权限")
            }
        }
    }
}

@Composable
fun LocationTabs(
    currentLocation: Location,
    onLocationChange: (Location) -> Unit
) {
    TabRow(selectedTabIndex = if (currentLocation == Location.LOCAL) 0 else 1) {
        Tab(
            // M7:files-location-local-tab
            modifier = Modifier.testTag("files-location-local-tab"),
            selected = currentLocation == Location.LOCAL,
            onClick = { onLocationChange(Location.LOCAL) },
            text = { Text("本机") }
        )
        Tab(
            // M7:files-location-remote-tab(远程=浏览,桌面 browse-* 域入口)
            modifier = Modifier.testTag("files-location-remote-tab"),
            selected = currentLocation == Location.REMOTE,
            onClick = { onLocationChange(Location.REMOTE) },
            text = { Text("远程") }
        )
    }
}

@Composable
fun BreadcrumbRow(
    breadcrumb: List<String>,
    onNavigateUp: () -> Unit,
    onNavigateToRoot: () -> Unit
) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        tonalElevation = 2.dp
    ) {
        Row(
            modifier = Modifier.padding(start = 8.dp, end = 8.dp, top = 8.dp, bottom = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            IconButton(onClick = onNavigateUp, enabled = breadcrumb.isNotEmpty()) {
                Icon(Icons.Default.ArrowBack, "返回上级")
            }

            breadcrumb.forEach { path ->
                Text(
                    text = path.split("/").last(),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                Icon(Icons.Default.ChevronRight, null, modifier = Modifier.size(16.dp))
            }

            IconButton(onClick = onNavigateToRoot) {
                Icon(Icons.Default.Home, "回到根目录")
            }
        }
    }
}

@Composable
fun SearchBar(
    query: String,
    onQueryChange: (String) -> Unit
) {
    OutlinedTextField(
        value = query,
        onValueChange = onQueryChange,
        modifier = Modifier
            .fillMaxWidth()
            .padding(8.dp),
        placeholder = { Text("搜索文件...") },
        leadingIcon = { Icon(Icons.Default.Search, null) },
        singleLine = true
    )
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun FileEntryCard(
    entry: FileEntryUi,
    isSelected: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onClick, onLongClick = onLongClick),
        colors = if (isSelected) {
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)
        } else {
            CardDefaults.cardColors()
        }
    ) {
        ListItem(
            headlineContent = { Text(entry.name) },
            supportingContent = {
                Text(
                    if (entry.isDir) "文件夹" else Formatters.formatFileSize(entry.size.toULong())
                )
            },
            leadingContent = {
                Icon(
                    if (entry.isDir) Icons.Default.Folder else Icons.Default.InsertDriveFile,
                    null
                )
            },
            trailingContent = {
                if (isSelected) {
                    Icon(Icons.Default.CheckCircle, null)
                }
            }
        )
    }
}

@Composable
fun RenameDialog(
    oldName: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit
) {
    var newName by remember { mutableStateOf(oldName) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("重命名") },
        text = {
            OutlinedTextField(
                value = newName,
                onValueChange = { newName = it },
                label = { Text("新名称") },
                singleLine = true
            )
        },
        confirmButton = {
            Button(
                onClick = { onConfirm(newName) },
                enabled = newName.isNotEmpty() && newName != oldName
            ) {
                Text("确定")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("取消")
            }
        }
    )
}

@Composable
fun DeleteConfirmationDialog(
    count: Int,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("确认删除") },
        text = { Text("确定要删除选中的 $count 项吗？") },
        confirmButton = {
            Button(onClick = onConfirm) {
                Text("删除")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("取消")
            }
        }
    )
}

@Composable
fun CreateFolderDialog(
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit
) {
    var folderName by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("新建文件夹") },
        text = {
            OutlinedTextField(
                value = folderName,
                onValueChange = { folderName = it },
                label = { Text("文件夹名称") },
                singleLine = true
            )
        },
        confirmButton = {
            Button(
                onClick = { onConfirm(folderName) },
                enabled = folderName.isNotEmpty()
            ) {
                Text("创建")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("取消")
            }
        }
    )
}

/**
 * Remote tab landing: pick a connected device whose shares to browse.
 * List mirrors DeviceSelectionSheet semantics (connected devices only).
 */
@Composable
fun RemoteDevicePicker(
    onDeviceSelected: (String) -> Unit
) {
    val app = com.localtrans.app.LocalTransBridge.app
    var devices by remember { mutableStateOf(emptyList<DeviceDto>()) }
    var loading by remember { mutableStateOf(true) }

    // Refresh the list when devices change (pair/connect/disconnect)
    LaunchedEffect(Unit) {
        kotlinx.coroutines.flow.MutableSharedFlow<Unit>(extraBufferCapacity = 1).tryEmit(Unit)
        devices = try { kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.devices() } } catch (e: Exception) { emptyList() }
        loading = false
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp)
    ) {
        Text(
            "选择远程设备",
            style = MaterialTheme.typography.titleMedium
        )
        Spacer(modifier = Modifier.height(12.dp))

        if (loading) {
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
        } else {
            val connected = devices.filter { it.connected }
            if (connected.isEmpty()) {
                Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(
                            Icons.Default.DevicesOther,
                            contentDescription = null,
                            modifier = Modifier.size(56.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                        Spacer(modifier = Modifier.height(12.dp))
                        Text(
                            "暂无已连接的设备",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                        Spacer(modifier = Modifier.height(4.dp))
                        Text(
                            "先在设备页配对连接一台设备,即可浏览它的共享文件夹",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                }
            } else {
                LazyColumn(
                    verticalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    items(connected.size) { idx ->
                        val device = connected[idx]
                        Card(modifier = Modifier.fillMaxWidth()) {
                            ListItem(
                                headlineContent = { Text(device.name) },
                                supportingContent = { Text(device.addr) },
                                leadingContent = {
                                    Icon(
                                        if (device.viaRelay) Icons.Default.Cloud else Icons.Default.DesktopWindows,
                                        null
                                    )
                                },
                                trailingContent = {
                                    Icon(Icons.Default.ChevronRight, null)
                                },
                                modifier = Modifier.clickable { onDeviceSelected(device.fingerprint) }
                            )
                        }
                    }
                }
            }
        }
    }
}

/**
 * Device selection sheet for sending files
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceSelectionSheet(
    onDismiss: () -> Unit,
    onDeviceSelected: (String) -> Unit
) {
    val app = com.localtrans.app.LocalTransBridge.app
    var devices by remember { mutableStateOf(emptyList<DeviceDto>()) }

    LaunchedEffect(Unit) {
        devices = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.devices() }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp)
        ) {
            Text(
                "选择接收设备",
                style = MaterialTheme.typography.titleMedium
            )
            Spacer(modifier = Modifier.height(16.dp))

            val connectedDevices = devices.filter { it.connected }
            if (connectedDevices.isEmpty()) {
                Text(
                    "暂无已连接的设备,请先在设备页完成配对",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            } else {
                connectedDevices.forEach { device ->
                    ListItem(
                        headlineContent = { Text(device.name) },
                        supportingContent = { Text(device.addr) },
                        leadingContent = {
                            Icon(
                                if (device.viaRelay) Icons.Default.Cloud else Icons.Default.DevicesOther,
                                null
                            )
                        },
                        modifier = Modifier.clickable { onDeviceSelected(device.fingerprint) }
                    )
                }
            }
        }
    }
}
