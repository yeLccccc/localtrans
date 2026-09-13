package com.localtrans.app.util

/**
 * Formatters - Pure utility functions for formatting values
 */
object Formatters {

    /**
     * Format file size in human-readable format
     */
    fun formatFileSize(bytes: ULong): String {
        return when {
            bytes < 1024uL -> "$bytes B"
            bytes < 1024uL * 1024uL -> String.format("%.2f KB", bytes.toDouble() / 1024)
            bytes < 1024uL * 1024uL * 1024uL -> String.format("%.2f MB", bytes.toDouble() / (1024 * 1024))
            else -> String.format("%.2f GB", bytes.toDouble() / (1024 * 1024 * 1024))
        }
    }

    /**
     * Format duration in seconds to MM:SS or H:MM:SS
     */
    fun formatDuration(seconds: Long): String {
        return when {
            seconds < 0 -> "∞"
            seconds < 3600 -> {
                val mins = seconds / 60
                val secs = seconds % 60
                String.format("%d:%02d", mins, secs)
            }
            else -> {
                val hours = seconds / 3600
                val mins = (seconds % 3600) / 60
                val secs = seconds % 60
                String.format("%d:%02d:%02d", hours, mins, secs)
            }
        }
    }

    /**
     * Format duration in milliseconds to MM:SS or H:MM:SS
     */
    fun formatDurationMs(ms: Long): String {
        return formatDuration(ms / 1000)
    }

    /**
     * Get file extension from filename
     */
    fun getFileExtension(filename: String): String {
        val lastDot = filename.lastIndexOf('.')
        return if (lastDot > 0 && lastDot < filename.length - 1) {
            filename.substring(lastDot + 1)
        } else {
            ""
        }
    }

    /**
     * Format speed in bytes per second to human-readable format
     */
    fun formatSpeed(bps: Long): String {
        return when {
            bps < 1024 -> "$bps B/s"
            bps < 1024 * 1024 -> String.format("%.1f KB/s", bps.toDouble() / 1024)
            else -> String.format("%.1f MB/s", bps.toDouble() / (1024 * 1024))
        }
    }

    /**
     * Format ETA in seconds to MM:SS or H:MM:SS
     */
    fun formatEta(secs: Long): String {
        return when {
            secs <= 0 -> "--"
            secs < 3600 -> {
                val mins = secs / 60
                val remainingSecs = secs % 60
                String.format("%d:%02d", mins, remainingSecs)
            }
            else -> {
                val hours = secs / 3600
                val mins = (secs % 3600) / 60
                val remainingSecs = secs % 60
                String.format("%d:%02d:%02d", hours, mins, remainingSecs)
            }
        }
    }
}
