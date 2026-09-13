package com.localtrans.app.ui.files

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.FolderZip
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.localtrans.app.util.Formatters
import java.io.File

val DOC_EXTENSIONS = setOf(
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "txt", "md", "csv", "zip", "7z", "rar", "json", "epub"
)

fun isDocFile(name: String): Boolean =
    name.substringAfterLast('.', "").lowercase() in DOC_EXTENSIONS

/** 文档行 UI 模型:v0.11.0 低危批把 size/mtime 的 stat IO 收敛到扫描线程,
 *  Composable 渲染行时直接读字段,不再每帧 length()/lastModified() 打盘 */
data class DocItem(
    val file: File,
    val name: String,
    val isArchive: Boolean,
    val sizeBytes: Long,
    val modifiedMs: Long,
)

/** 扫描根下常见文档(非递归进隐藏目录;深度上限 3 层防遍历整盘) */
fun collectDocs(roots: List<File>, maxDepth: Int = 3): List<DocItem> {
    val out = mutableListOf<DocItem>()
    fun walk(dir: File, depth: Int) {
        if (depth > maxDepth) return
        val entries = dir.listFiles() ?: return
        for (f in entries) {
            if (f.name.startsWith(".")) continue
            if (f.isDirectory) walk(f, depth + 1)
            else if (isDocFile(f.name)) out.add(
                DocItem(
                    file = f,
                    name = f.name,
                    isArchive = f.extension.lowercase() in setOf("zip", "7z", "rar"),
                    sizeBytes = f.length(),
                    modifiedMs = f.lastModified(),
                )
            )
        }
    }
    roots.forEach { if (it.exists()) walk(it, 0) }
    return out.sortedByDescending { it.modifiedMs }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DocsTab(
    files: List<DocItem>,
    selected: Set<String>,
    onToggle: (String) -> Unit,
    onLongClick: (path: String) -> Unit,
    modifier: Modifier = Modifier
) {
    LazyColumn(modifier = modifier.fillMaxSize()) {
        items(files, key = { it.file.absolutePath }) { f ->
            ListItem(
                modifier = Modifier.combinedClickable(
                    onClick = { onToggle(f.file.absolutePath) },
                    onLongClick = { onLongClick(f.file.absolutePath) }
                ),
                leadingContent = {
                    Icon(
                        if (f.isArchive) Icons.Default.FolderZip else Icons.Default.Description,
                        null
                    )
                },
                headlineContent = { Text(f.name) },
                supportingContent = {
                    // stat 字段在 collectDocs(IO 线程)已算好,渲染零磁盘 IO
                    Text("${Formatters.formatFileSize(f.sizeBytes.toULong())} · " +
                        java.text.SimpleDateFormat("yyyy-MM-dd", java.util.Locale.getDefault())
                            .format(java.util.Date(f.modifiedMs)))
                },
                trailingContent = {
                    if (f.file.absolutePath in selected) {
                        Icon(Icons.Default.CheckCircle, null, tint = MaterialTheme.colorScheme.primary)
                    }
                }
            )
        }
    }
}
