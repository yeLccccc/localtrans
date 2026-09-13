package com.localtrans.app.media

import android.content.Context
import android.media.MediaScannerConnection
import android.util.Log
import java.io.File

/**
 * 接收落盘后的媒体索引通知:图片/视频经 MediaScanner 扫描进系统相册。
 * 文档类不处理(Download/LocalTrans 文件管理器天然可见)。
 */
object MediaScanNotifier {
    private const val TAG = "MediaScanNotifier"
    private val SCANNABLE = setOf(
        "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp",
        "mp4", "mov", "mkv", "avi", "3gp", "webm"
    )

    fun isScannableExtension(name: String): Boolean {
        val ext = name.substringAfterLast('.', "").lowercase()
        return ext in SCANNABLE
    }

    /** 即发即忘:过滤媒体扩展名后交系统扫描,失败只打日志 */
    fun scanPaths(context: Context, paths: List<String>) {
        val media = paths.filter { isScannableExtension(File(it).name) }
        if (media.isEmpty()) return
        try {
            MediaScannerConnection.scanFile(context, media.toTypedArray(), null) { _, _ -> }
            Log.d(TAG, "媒体扫描请求已发出: ${media.size} 项")
        } catch (e: Exception) {
            Log.w(TAG, "媒体扫描失败(不影响传输): ${e.message}")
        }
    }
}
