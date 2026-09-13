package com.localtrans.app.ui.files

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp

/**
 * 文件项长按菜单。
 * 本机页:发送/重命名/删除/选择多项(操作作用于本机文件);
 * 远程页:下载到本机/重命名/选择多项——P3-T2 按 Android as-built 恢复远程
 * Rename 一项(协议 ShareRename 已通,FilesViewModel.renameEntry REMOTE 分支
 * 已有);Delete/Mkdir 不恢复(PC 端已定案砍远程删除/建目录 UI)。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FileEntryMenuSheet(
    isRemote: Boolean,
    onDismiss: () -> Unit,
    onDownload: () -> Unit,
    onSend: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
    onSelectMultiple: () -> Unit
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        if (isRemote) {
            MenuRow(icon = Icons.Default.Download, label = "下载到本机", onClick = onDownload)
            MenuRow(icon = Icons.Default.Edit, label = "重命名", onClick = onRename)
        } else {
            MenuRow(icon = Icons.Default.Send, label = "发送到设备", onClick = onSend)
            MenuRow(icon = Icons.Default.Edit, label = "重命名", onClick = onRename)
            MenuRow(icon = Icons.Default.Delete, label = "删除", onClick = onDelete)
        }
        MenuRow(icon = Icons.Default.Checklist, label = "选择多项", onClick = onSelectMultiple)
        Spacer(modifier = Modifier.height(24.dp))
    }
}

@Composable
private fun MenuRow(icon: ImageVector, label: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        Icon(icon, contentDescription = null)
        Text(text = label, style = MaterialTheme.typography.bodyLarge)
    }
}
