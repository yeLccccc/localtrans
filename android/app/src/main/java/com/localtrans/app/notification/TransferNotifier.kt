package com.localtrans.app.notification

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.localtrans.app.MainActivity
import com.localtrans.app.R
import uniffi.localtrans_ffi.AppEvent

/**
 * Manager for transfer completion notifications
 */
class TransferNotifier(private val context: Context) {

    companion object {
        private const val CHANNEL_ID = "transfer_complete"
        private const val NOTIFICATION_ID = 1001

        // Singleton instance
        @Volatile
        private var instance: TransferNotifier? = null

        fun getInstance(context: Context): TransferNotifier {
            return instance ?: synchronized(this) {
                instance ?: TransferNotifier(context.applicationContext).also {
                    instance = it
                }
            }
        }
    }

    private val notificationManager: NotificationManager =
        context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    init {
        createNotificationChannel()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "传输完成",
                NotificationManager.IMPORTANCE_DEFAULT
            ).apply {
                description = "文件传输完成通知"
                setShowBadge(false)
            }

            notificationManager.createNotificationChannel(channel)
        }
    }

    /**
     * Check if POST_NOTIFICATIONS permission is granted (API 33+)
     */
    private fun hasNotificationPermission(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            ContextCompat.checkSelfPermission(
                context,
                android.Manifest.permission.POST_NOTIFICATIONS
            ) == android.content.pm.PackageManager.PERMISSION_GRANTED
        } else {
            true
        }
    }

    /**
     * Handle transfer done event
     *
     * v0.11.0 低危批修复:此前任何成功的 TransferDone(含 push 发送方向)
     * 都会弹"文件接收完成"——语义错位。现按传输行 direction 过滤:
     * 仅接收方向(rx/pull)成功时提示,发送方向静默。
     */
    fun handleTransferDoneEvent(event: AppEvent.TransferDone) {
        if (!hasNotificationPermission()) {
            // Silently skip if permission not granted
            return
        }

        // Only notify for successful incoming transfers
        if (!event.ok) {
            return
        }

        if (!isReceivingJob(event.jobId)) {
            return
        }
        showTransferCompleteNotification()
    }

    /**
     * 查传输表判断该 job 是否本机接收方向。查表失败/行不存在时保守不弹
     * (宁缺勿滥:避免把发送误报成"接收完成")。
     */
    private fun isReceivingJob(jobId: ULong): Boolean {
        return try {
            val app = com.localtrans.app.LocalTransBridge.app
            val row = kotlinx.coroutines.runBlocking {
                kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    app.transfers().firstOrNull { it.jobId == jobId }
                }
            }
            when (row?.direction) {
                "rx", "pull" -> true
                else -> false
            }
        } catch (e: Exception) {
            false
        }
    }

    /**
     * Show transfer complete notification
     */
    private fun showTransferCompleteNotification() {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK
            putExtra("open_tab", "transfers")
        }

        val pendingIntent = PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )

        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setContentTitle("传输完成")
            .setContentText("文件接收完成")
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentIntent(pendingIntent)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()

        notificationManager.notify(NOTIFICATION_ID, notification)
    }

    /**
     * Cancel active notifications
     */
    fun cancelNotifications() {
        notificationManager.cancel(NOTIFICATION_ID)
    }
}
