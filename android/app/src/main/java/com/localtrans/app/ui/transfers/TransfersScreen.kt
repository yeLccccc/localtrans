package com.localtrans.app.ui.transfers

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import coil.compose.AsyncImage
import coil.request.ImageRequest
import android.content.Context
import android.content.Intent
import android.content.ActivityNotFoundException
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File
import com.localtrans.app.util.FileTypeIcons
import com.localtrans.app.util.Formatters
import uniffi.localtrans_ffi.DiskJobDto

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
fun TransfersScreen(
    viewModel: TransfersViewModel,
    modifier: Modifier = Modifier
) {
    val uiState by viewModel.uiState.collectAsState()
    val transfers by viewModel.transfers.collectAsState()
    val diskJobs by viewModel.diskJobs.collectAsState()
    val peers by viewModel.peers.collectAsState()

    var showClearConfirm by remember { mutableStateOf(false) }
    // 两级删除确认目标(终态卡):null=不弹
    var removeTarget by remember { mutableStateOf<TransferUi?>(null) }
    // 磁盘历史彻底删除确认目标:null=不弹
    var destroyDiskTarget by remember { mutableStateOf<DiskJobDto?>(null) }

    // 活动/历史分区(桌面 splitActiveHistory 同款):历史默认折叠
    var historyExpanded by remember { mutableStateOf(false) }
    var diskExpanded by remember { mutableStateOf(false) }

    val split = splitActiveHistory(transfers)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("传输") },
                actions = {
                    // debug 构建专属:演示卡注入(六状态视觉验证/T7 场景断言用;
                    // release 无此入口,沿用 settings debug 区块惯例)
                    if (com.localtrans.app.BuildConfig.DEBUG) {
                        TextButton(
                            modifier = Modifier.testTag("transfers-debug-seed-btn"),
                            onClick = { viewModel.seedDemoCardsForUiCheck() }
                        ) {
                            Icon(
                                Icons.Default.Science,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp)
                            )
                            Spacer(modifier = Modifier.width(4.dp))
                            Text("演示")
                        }
                    }
                    TextButton(
                        // M7:桌面 transfers-clear-completed-btn
                        modifier = Modifier.testTag("transfers-clear-completed-btn"),
                        onClick = { showClearConfirm = true },
                        enabled = split.history.isNotEmpty()
                    ) {
                        Icon(
                            Icons.Default.DeleteSweep,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text("清空记录")
                    }
                }
            )
        }
    ) { padding ->
        Box(
            modifier = modifier
                .fillMaxSize()
                .padding(padding)
        ) {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                // 空态(列表为空但磁盘历史入口仍可达——view 级删除后列表可为空)
                if (split.active.isEmpty() && split.history.isEmpty()) {
                    item(key = "empty") { EmptyTransfersItem() }
                }

                // 活动区(非终态在上)
                items(split.active, key = { it.jobId }) { transfer ->
                    TransferCard(
                        transfer = transfer,
                        peers = peers,
                        onPause = { viewModel.pauseTransfer(transfer.jobId.toULong()) },
                        onResume = { viewModel.resumeTransfer(transfer.jobId.toULong()) },
                        onCancel = { viewModel.cancelTransfer(transfer.jobId.toULong()) },
                        onRetry = { viewModel.retryTransfer(transfer.jobId.toULong()) },
                        onRemove = { removeTarget = transfer }
                    )
                }

                // 历史区标头(终态默认折叠,标头显示条数)
                if (split.history.isNotEmpty()) {
                    item(key = "history-header") {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { historyExpanded = !historyExpanded }
                                .testTag("transfers-history-fold-btn")
                                .padding(vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Text(
                                text = if (historyExpanded) "▾" else "▸",
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                            Spacer(modifier = Modifier.width(6.dp))
                            Text(
                                text = "历史 (${split.history.size})",
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }
                    }
                }

                // 历史卡(展开时)
                if (historyExpanded) {
                    items(split.history, key = { it.jobId }) { transfer ->
                        TransferCard(
                            transfer = transfer,
                            peers = peers,
                            onPause = { viewModel.pauseTransfer(transfer.jobId.toULong()) },
                            onResume = { viewModel.resumeTransfer(transfer.jobId.toULong()) },
                            onCancel = { viewModel.cancelTransfer(transfer.jobId.toULong()) },
                            onRetry = { viewModel.retryTransfer(transfer.jobId.toULong()) },
                            onRemove = { removeTarget = transfer }
                        )
                    }
                }

                // 磁盘历史入口(历史区底部;列表为空时也恒可达,对齐桌面页头入口语义)
                item(key = "disk-history-entry") {
                    TextButton(
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag("transfers-disk-history-btn"),
                        onClick = {
                            diskExpanded = !diskExpanded
                            if (diskExpanded) viewModel.loadDiskJobs()
                        }
                    ) {
                        Icon(
                            Icons.Default.History,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text(if (diskExpanded) "收起磁盘历史" else "磁盘历史")
                    }
                }

                // 磁盘历史列表(含已移除)
                if (diskExpanded) {
                    item(key = "disk-history-header") {
                        Text(
                            text = "磁盘历史（含已移除）",
                            style = MaterialTheme.typography.titleSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                    if (diskJobs.isEmpty()) {
                        item(key = "disk-history-empty") {
                            Text(
                                text = "暂无磁盘历史记录",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }
                    } else {
                        // key 用字符串:jobId 是 ULong,不能直接作 LazyColumn key
                        //(Bundle 不支持,占位大 id 直接崩溃)
                        items(diskJobs, key = { "disk-${it.jobId}" }) { job ->
                            DiskJobRow(
                                job = job,
                                onRestore = { viewModel.restoreDiskJob(job.jobId) },
                                onDestroy = { destroyDiskTarget = job }
                            )
                        }
                    }
                }
            }

            // Offer sheet 已上移到 AppNav 根层全局弹窗(任何页面可见),
            // 此处不再挂页面级副本避免双弹窗
        }
    }

    if (showClearConfirm) {
        AlertDialog(
            onDismissRequest = { showClearConfirm = false },
            title = { Text("清空传输记录") },
            text = { Text("将移除所有已完成和已失败的记录（仅移除视图，可在磁盘历史找回），进行中的传输不受影响。") },
            confirmButton = {
                Button(onClick = {
                    viewModel.clearFinishedTransfers()
                    showClearConfirm = false
                }) {
                    Text("清空")
                }
            },
            dismissButton = {
                TextButton(onClick = { showClearConfirm = false }) {
                    Text("取消")
                }
            }
        )
    }

    // 两级删除确认(终态卡):移除视图=可找回 / 彻底删除=不可恢复
    removeTarget?.let { target ->
        AlertDialog(
            onDismissRequest = { removeTarget = null },
            // Dialog 是独立窗口,testTagsAsResourceId 需单独开启(OfferSheet 同款)
            modifier = Modifier.semantics { testTagsAsResourceId = true },
            title = { Text("删除任务") },
            text = {
                Text(
                    "彻底删除磁盘数据？此操作不可恢复。\n" +
                        "移除视图：磁盘保留，可在磁盘历史找回；\n" +
                        "彻底删除：连同磁盘数据一并删除。"
                )
            },
            confirmButton = {
                TextButton(
                    modifier = Modifier.testTag("transfer-remove-destroy-btn"),
                    onClick = {
                        viewModel.removeTransfer(target.jobId.toULong(), "destroy")
                        removeTarget = null
                    }
                ) {
                    Text("彻底删除", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                Row {
                    TextButton(
                        modifier = Modifier.testTag("transfer-remove-view-btn"),
                        onClick = {
                            viewModel.removeTransfer(target.jobId.toULong(), "view")
                            removeTarget = null
                        }
                    ) {
                        Text("移除视图")
                    }
                    TextButton(onClick = { removeTarget = null }) {
                        Text("取消")
                    }
                }
            }
        )
    }

    // 磁盘历史彻底删除确认
    destroyDiskTarget?.let { job ->
        AlertDialog(
            onDismissRequest = { destroyDiskTarget = null },
            title = { Text("彻底删除") },
            text = { Text("彻底删除该历史记录？此操作不可恢复") },
            confirmButton = {
                Button(onClick = {
                    viewModel.destroyDiskJob(job.jobId)
                    destroyDiskTarget = null
                }) {
                    Text("彻底删除", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { destroyDiskTarget = null }) {
                    Text("取消")
                }
            }
        )
    }
}

/** 空态(LazyColumn item 版,不撑满整屏——磁盘历史入口保持可达) */
@Composable
private fun EmptyTransfersItem() {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 64.dp),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Icon(
            imageVector = Icons.Default.SwapHoriz,
            contentDescription = null,
            modifier = Modifier.size(56.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(modifier = Modifier.height(12.dp))
        Text(
            text = "暂无传输任务",
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(modifier = Modifier.height(4.dp))
        Text(
            text = "从文件页选择文件发送,或浏览远程文件拉取",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
    }
}

@Composable
fun TransferCard(
    transfer: TransferUi,
    peers: List<PeerNameSource> = emptyList(),
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onRetry: () -> Unit,
    onRemove: () -> Unit
) {
    val context = LocalContext.current
    val isActive = transfer.state in ACTIVE_STATES
    // 父卡展开状态(有子文件才有展开钮)
    var expanded by remember(transfer.jobId) { mutableStateOf(false) }
    val hasChildren = transfer.children.isNotEmpty()

    // R1 单进度条:发送方主进度=对端确认字节(remoteDone),接收方=done
    val pair = progressPair(transfer)
    val percent = mainPercent(transfer)
    val awaitingConfirm = isAwaitingConfirmText(transfer)

    Card(
        // M7:动态前缀,与桌面 transfer-item-{job_id} 同构(文档级锚点)
        modifier = Modifier
            .fillMaxWidth()
            .testTag("transfer-item-${transfer.jobId}"),
        elevation = CardDefaults.cardElevation(defaultElevation = 2.dp),
        onClick = {
            // Only clickable when done with localPath
            if (transfer.state == "done" && transfer.localPath != null) {
                openFileWithFileProvider(context, transfer.localPath, transfer.name)
            }
        }
    ) {
        Column(
            modifier = Modifier.padding(16.dp)
        ) {
            // Header: Direction icon, file type icon, name, expand toggle, status
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.weight(1f)
                ) {
                    Icon(
                        imageVector = if (transfer.direction == "push") {
                            Icons.Default.Upload
                        } else {
                            Icons.Default.Download
                        },
                        contentDescription = null,
                        modifier = Modifier.size(20.dp)
                    )
                    Spacer(modifier = Modifier.width(4.dp))
                    Icon(
                        imageVector = FileTypeIcons.iconFor(transfer.name),
                        contentDescription = null,
                        modifier = Modifier.size(18.dp)
                    )
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = transfer.name,
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f)
                    )
                }
                if (hasChildren) {
                    // 父卡展开钮:▸ 收起 / ▾ 展开(桌面 transfer-expand-btn 同名)
                    Text(
                        text = if (expanded) "▾" else "▸",
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier
                            .clickable { expanded = !expanded }
                            .testTag("transfer-expand-btn")
                            .padding(horizontal = 8.dp)
                    )
                }
                StatusBadge(
                    if (transfer.state == "done" && transfer.instant) "instant" else transfer.state
                )
            }

            Spacer(modifier = Modifier.height(4.dp))

            // 角色与对端(桌面 role-badge + peerName 同构;别名>广播名>指纹缩写;
            // 旧卡 peer 可能为空串,只显角色)
            val peerName = peerDisplayName(transfer.peer, peers)
            Text(
                text = if (peerName.isEmpty()) roleText(transfer.localRole)
                else "${roleText(transfer.localRole)} · $peerName",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )

            Spacer(modifier = Modifier.height(8.dp))

            // 排队位次(pending 且有位次:排队中 · 第 N 位;无位次回退方向文案)
            if (transfer.state == "pending") {
                val queueHint = if (transfer.localRole == "source-push" && transfer.queuePos == null) {
                    "等待对方确认接收..."
                } else {
                    queueText(transfer.queuePos, transfer.direction)
                }
                Text(
                    text = queueHint,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag("transfer-queue-text")
                )
                Spacer(modifier = Modifier.height(4.dp))
            }

            // R1:单进度条(pending 无位次走不确定条;其余按主进度定值)
            if (transfer.state == "pending" && transfer.queuePos == null) {
                LinearProgressIndicator(
                    modifier = Modifier.fillMaxWidth()
                )
            } else {
                LinearProgressIndicator(
                    progress = { (percent / 100.0).toFloat() },
                    modifier = Modifier.fillMaxWidth()
                )
            }
            // 满格未确认/网络积压:仅"等待对方确认"文案态,不渲染第二条进度条(R1)
            if (awaitingConfirm) {
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = "等待对方确认",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag("transfer-awaiting-confirm")
                )
            }

            Spacer(modifier = Modifier.height(8.dp))

            // Info: 已用时长, Done/Total(发送方按主进度字节), Speed, ETA
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween
            ) {
                val elapsed = elapsedSeconds(transfer, System.currentTimeMillis())
                if (elapsed != null) {
                    Text(
                        text = "已用 ${formatElapsed(elapsed)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
                Text(
                    text = "${Formatters.formatFileSize(pair.mainDone.toULong())} / ${Formatters.formatFileSize(transfer.total.toULong())}",
                    style = MaterialTheme.typography.bodySmall
                )
                if (isActive && transfer.speedBps > 0) {
                    Text(
                        text = Formatters.formatSpeed(transfer.speedBps),
                        style = MaterialTheme.typography.bodySmall
                    )
                    Text(
                        text = Formatters.formatEta(transfer.etaSecs),
                        style = MaterialTheme.typography.bodySmall
                    )
                }
            }

            // Fail reason
            if (transfer.state == "failed" && transfer.failReason.isNotEmpty()) {
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = localizeFailReason(transfer.failReason),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error
                )
            }

            // 父卡子文件展开区(子文件名/大小/状态/进度细条)
            if (expanded && hasChildren) {
                Spacer(modifier = Modifier.height(8.dp))
                transfer.children.forEach { child ->
                    ChildRow(child)
                }
            }

            // Done row: show thumbnail for media files
            if (transfer.state == "done" && transfer.localPath != null) {
                val ext = transfer.name.substringAfterLast('.', "").lowercase()
                val isMedia = ext in setOf("jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp")
                if (isMedia) {
                    Spacer(modifier = Modifier.height(8.dp))
                    AsyncImage(
                        model = ImageRequest.Builder(LocalContext.current)
                            .data(transfer.localPath)
                            .crossfade(true)
                            .build(),
                        contentDescription = transfer.name,
                        modifier = Modifier.size(48.dp)
                    )
                }
            }

            Spacer(modifier = Modifier.height(8.dp))

            // Action buttons
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End
            ) {
                when (transfer.state) {
                    "active" -> {
                        TextButton(
                            modifier = Modifier.testTag("transfer-pause-btn"),
                            onClick = onPause
                        ) {
                            Icon(Icons.Default.Pause, null, modifier = Modifier.size(16.dp))
                            Spacer(modifier = Modifier.width(4.dp))
                            Text("暂停")
                        }
                    }
                    "paused" -> {
                        TextButton(
                            modifier = Modifier.testTag("transfer-resume-btn"),
                            onClick = onResume
                        ) {
                            Icon(Icons.Default.PlayArrow, null, modifier = Modifier.size(16.dp))
                            Spacer(modifier = Modifier.width(4.dp))
                            Text("继续")
                        }
                    }
                    "failed", "interrupted" -> {
                        // Show retry only for specific failure types
                        if (isRetryableFailure(transfer.failReason)) {
                            TextButton(
                                modifier = Modifier.testTag("transfer-retry-btn"),
                                onClick = onRetry
                            ) {
                                Icon(Icons.Default.Refresh, null, modifier = Modifier.size(16.dp))
                                Spacer(modifier = Modifier.width(4.dp))
                                Text("重试")
                            }
                        }
                    }
                }
                if (isActive || transfer.state == "paused") {
                    // 活动卡取消 = 取消 + 彻底删(destroy 级,cancelling 仲裁)
                    TextButton(
                        modifier = Modifier.testTag("transfer-cancel-btn"),
                        onClick = onCancel
                    ) {
                        Icon(Icons.Default.Close, null, modifier = Modifier.size(16.dp))
                        Spacer(modifier = Modifier.width(4.dp))
                        Text("取消")
                    }
                } else {
                    // 终态卡删除:两级确认(移除视图=可找回 / 彻底删除=不可恢复)
                    TextButton(
                        modifier = Modifier.testTag("transfer-remove-btn"),
                        onClick = onRemove
                    ) {
                        Icon(Icons.Default.DeleteOutline, null, modifier = Modifier.size(16.dp))
                        Spacer(modifier = Modifier.width(4.dp))
                        Text("删除")
                    }
                }
            }
        }
    }
}

/** 父卡子文件行:名称/大小/状态徽章/进度细条(桌面 child-row 同构) */
@Composable
private fun ChildRow(child: ChildUi) {
    val percent = if (child.total <= 0L) {
        if (child.state == "done") 1f else 0f
    } else {
        (child.done.toFloat() / child.total).coerceIn(0f, 1f)
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp)
            .testTag("transfer-child-${child.jobId}"),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(
            text = child.name,
            style = MaterialTheme.typography.labelSmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f)
        )
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = Formatters.formatFileSize(child.total.toULong()),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(modifier = Modifier.width(8.dp))
        ChildStateBadge(child.state)
        Spacer(modifier = Modifier.width(8.dp))
        LinearProgressIndicator(
            progress = { percent },
            modifier = Modifier
                .width(72.dp)
                .height(4.dp)
        )
    }
}

@Composable
private fun ChildStateBadge(state: String) {
    val color = when (state) {
        "done" -> MaterialTheme.colorScheme.tertiary
        "failed", "interrupted" -> MaterialTheme.colorScheme.error
        "active" -> MaterialTheme.colorScheme.primary
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Text(
        text = childStateText(state),
        style = MaterialTheme.typography.labelSmall,
        color = color
    )
}

/** 磁盘历史行:名称/状态/大小/时间 + 恢复到列表/彻底删除 */
@Composable
private fun DiskJobRow(
    job: DiskJobDto,
    onRestore: () -> Unit,
    onDestroy: () -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag("disk-job-${job.jobId}"),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = job.displayName,
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
            Text(
                text = listOfNotNull(
                    diskStateText(job.state),
                    Formatters.formatFileSize(job.total),
                    job.direction.takeIf { it.isNotEmpty() }?.let { if (it == "pull") "接收" else "发送" },
                    formatDiskTime(job.createdAtMs)
                ).joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
        TextButton(
            modifier = Modifier.testTag("disk-job-${job.jobId}-restore-btn"),
            onClick = onRestore
        ) {
            Text("恢复")
        }
        TextButton(
            modifier = Modifier.testTag("disk-job-${job.jobId}-destroy-btn"),
            onClick = onDestroy
        ) {
            Text("删除", color = MaterialTheme.colorScheme.error)
        }
    }
}

/** 磁盘历史状态文案(未知原样) */
private fun diskStateText(state: String): String = when (state) {
    "pending" -> "等待中"
    "active" -> "进行中"
    "paused" -> "已暂停"
    "done" -> "已完成"
    "failed" -> "失败"
    "interrupted" -> "已中断"
    else -> state
}

/** 磁盘历史时间:yyyy-MM-dd HH:mm(null=—) */
private fun formatDiskTime(ms: Long?): String {
    if (ms == null || ms <= 0L) return "—"
    val d = java.util.Date(ms)
    val f = java.text.SimpleDateFormat("yyyy-MM-dd HH:mm", java.util.Locale.getDefault())
    return f.format(d)
}

/**
 * Open file with FileProvider for ACTION_VIEW
 */
private fun openFileWithFileProvider(context: Context, localPath: String?, fileName: String) {
    if (localPath == null) return

    try {
        val file = File(localPath)
        if (!file.exists()) return

        val uri: Uri = FileProvider.getUriForFile(
            context,
            "${context.applicationContext.packageName}.fileprovider",
            file
        )

        val mimeType = guessMimeType(fileName)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, mimeType)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(intent)
    } catch (e: ActivityNotFoundException) {
        // Silently fail if no viewer available
    } catch (e: Exception) {
        // Silently fail for other errors
    }
}

/**
 * Guess MIME type from file name
 */
private fun guessMimeType(fileName: String): String {
    val ext = fileName.substringAfterLast('.', "").lowercase()
    return when (ext) {
        "jpg", "jpeg" -> "image/jpeg"
        "png" -> "image/png"
        "gif" -> "image/gif"
        "webp" -> "image/webp"
        "bmp" -> "image/bmp"
        "mp4" -> "video/mp4"
        "mov" -> "video/quicktime"
        "mkv" -> "video/x-matroska"
        "avi" -> "video/x-msvideo"
        "webm" -> "video/webm"
        "mp3" -> "audio/mpeg"
        "wav" -> "audio/wav"
        "flac" -> "audio/flac"
        "aac" -> "audio/aac"
        "ogg" -> "audio/ogg"
        "pdf" -> "application/pdf"
        "doc" -> "application/msword"
        "docx" -> "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        "xls" -> "application/vnd.ms-excel"
        "xlsx" -> "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        "ppt" -> "application/vnd.ms-powerpoint"
        "pptx" -> "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        "txt" -> "text/plain"
        "md" -> "text/markdown"
        "csv" -> "text/csv"
        "zip" -> "application/zip"
        "7z" -> "application/x-7z-compressed"
        "rar" -> "application/vnd.rar"
        "tar" -> "application/x-tar"
        "gz" -> "application/gzip"
        else -> "*/*"
    }
}

@Composable
fun StatusBadge(state: String) {
    val (color, label) = when (state) {
        "active" -> MaterialTheme.colorScheme.primary to "传输中"
        "pending" -> MaterialTheme.colorScheme.secondary to "等待中"
        "paused" -> MaterialTheme.colorScheme.secondary to "已暂停"
        "cancelling" -> MaterialTheme.colorScheme.secondary to "取消中"
        "done" -> MaterialTheme.colorScheme.tertiary to "已完成"
        "failed" -> MaterialTheme.colorScheme.error to "失败"
        "interrupted" -> MaterialTheme.colorScheme.error to "已中断"
        "instant" -> MaterialTheme.colorScheme.tertiary to "秒传"
        else -> MaterialTheme.colorScheme.surfaceVariant to state
    }

    Surface(
        color = color.copy(alpha = 0.1f),
        shape = MaterialTheme.shapes.small
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = color,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp)
        )
    }
}
