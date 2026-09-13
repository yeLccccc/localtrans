package com.localtrans.app.util

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.ui.graphics.vector.ImageVector

object FileTypeIcons {
    fun iconFor(name: String): ImageVector {
        val ext = name.substringAfterLast('.', "").lowercase()
        return when (ext) {
            "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp" -> Icons.Default.Image
            "mp4", "mov", "mkv", "avi", "3gp", "webm" -> Icons.Default.Videocam
            "mp3", "wav", "flac", "aac", "ogg" -> Icons.Default.AudioFile
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv" -> Icons.Default.Description
            "zip", "7z", "rar", "tar", "gz" -> Icons.Default.FolderZip
            else -> Icons.Default.InsertDriveFile
        }
    }

    /** "其他"占位(测试引用锚点) */
    fun nothing(): ImageVector = Icons.Default.InsertDriveFile
}
