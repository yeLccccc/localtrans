package com.localtrans.app.media

import android.content.ContentUris
import android.content.Context
import android.provider.MediaStore

data class MediaItem(
    val id: Long,
    val path: String,
    val name: String,
    val size: Long,
    val dateTakenMs: Long,
    val durationMs: Long = 0,
    val bucketName: String = ""
)

interface MediaRepo {
    fun queryImages(): List<MediaItem>
    fun queryVideos(): List<MediaItem>
}

/** MediaStore 直查(DATA 列绝对路径;所有文件访问已授权,v0.6.1 实测可行) */
class MediaStoreRepo(private val context: Context) : MediaRepo {
    override fun queryImages(): List<MediaItem> =
        query(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, projectionImages())

    override fun queryVideos(): List<MediaItem> =
        query(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, projectionVideos())

    private fun projectionImages() = arrayOf(
        MediaStore.Images.Media._ID,
        MediaStore.Images.Media.DATA,
        MediaStore.Images.Media.DISPLAY_NAME,
        MediaStore.Images.Media.SIZE,
        MediaStore.Images.Media.DATE_TAKEN,
        MediaStore.Images.Media.BUCKET_DISPLAY_NAME,
    )

    private fun projectionVideos() = arrayOf(
        MediaStore.Video.Media._ID,
        MediaStore.Video.Media.DATA,
        MediaStore.Video.Media.DISPLAY_NAME,
        MediaStore.Video.Media.SIZE,
        MediaStore.Video.Media.DATE_TAKEN,
        MediaStore.Video.Media.DURATION,
        MediaStore.Video.Media.BUCKET_DISPLAY_NAME,
    )

    private fun query(uri: android.net.Uri, projection: Array<String>): List<MediaItem> {
        val items = mutableListOf<MediaItem>()
        // 部分设备的 MediaStore 列名为小写 datetaken(API 28 模拟器实测),
        // 大写 DATE_TAKEN 作排序串会抛 SQLiteException——用 projection 里的实际列名。
        val dateTakenCol = projection.firstOrNull { it.equals("datetaken", ignoreCase = true) }
        val sortOrder = dateTakenCol?.let { "$it DESC" }
        try {
            context.contentResolver.query(uri, projection, null, null, sortOrder)?.use { c ->
                while (c.moveToNext()) {
                    val path = c.getString(1) ?: continue
                    items.add(
                        MediaItem(
                            id = c.getLong(0),
                            path = path,
                            name = c.getString(2) ?: "",
                            size = c.getLong(3),
                            dateTakenMs = c.getLong(4),
                            durationMs = if (projection.size == 7) c.getLong(5) else 0L,
                            bucketName = if (projection.size == 7) c.getString(6) ?: "" else c.getString(5) ?: "",
                        )
                    )
                }
            }
        } catch (e: Exception) {
            // 查询失败(库损坏/权限被收回)返回空列表,由 UI 显示空态——不崩溃
            android.util.Log.w("MediaStoreRepo", "媒体查询失败: ${e.message}")
        }
        return items
    }
}
