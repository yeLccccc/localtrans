package com.localtrans.app.ui.files

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Send
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp

/**
 * 四 Tab 统一多选底部栏:发送+删除+单选重命名+取消。
 * 远程页 isRemote=true:P3-T3 起主操作为「下载(N)」批量拉取(pull_files 聚合
 * 单卡),重命名/删除仍隐藏(远程操作只恢复 Rename 长按菜单项,见
 * FileEntryMenuSheet)。
 * 重命名按钮仅在恰好选中 1 项时渲染;onSelectAll 非空时渲染「全选」。
 */
@Composable
fun UnifiedSelectionBar(
    selectedCount: Int,
    totalBytes: Long,
    onSend: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
    onClear: () -> Unit,
    isRemote: Boolean = false,
    onSelectAll: (() -> Unit)? = null
) {
    Surface(tonalElevation = 8.dp, shadowElevation = 8.dp) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(sendBarSummary(selectedCount, totalBytes), Modifier.weight(1f))
            if (onSelectAll != null) {
                TextButton(
                    onClick = onSelectAll,
                    modifier = Modifier.testTag("files-select-all-btn")
                ) {
                    Text("全选")
                }
            }
            if (isRemote) {
                // P3-T3:远程批量下载按钮(pull_files 多路径聚合单卡)
                Button(
                    onClick = onSend,
                    modifier = Modifier.testTag("files-remote-download-all-btn")
                ) {
                    Text("下载($selectedCount)")
                }
            } else {
                if (selectedCount == 1) {
                    IconButton(onClick = onRename) {
                        Icon(Icons.Default.Edit, "重命名")
                    }
                }
                IconButton(onClick = onDelete) {
                    Icon(Icons.Default.Delete, "删除")
                }
                IconButton(onClick = onSend) {
                    Icon(Icons.Default.Send, "发送")
                }
            }
            IconButton(onClick = onClear) {
                Icon(Icons.Default.Close, "取消选择")
            }
        }
    }
}
