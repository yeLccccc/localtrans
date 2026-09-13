package com.localtrans.app.util

import android.Manifest
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.core.content.ContextCompat

/**
 * Storage permission helpers for the local file browser.
 *
 * The file browser reads real paths (e.g. /storage/emulated/0) through the
 * Rust FFI layer, which has no access to MediaStore — plain runtime storage
 * permissions are therefore required, selected by API level:
 * - API 33+: READ_MEDIA_* covers media files only; full directory browsing
 *   through FUSE needs MANAGE_EXTERNAL_STORAGE (special app-access setting).
 * - API <= 32: READ_EXTERNAL_STORAGE.
 */
object StoragePermissions {

    /** Runtime permission strings requested via the dialog flow */
    fun requiredPermissions(sdkInt: Int): Array<String> {
        val perms = mutableListOf<String>()
        if (sdkInt >= 33) {
            perms.add(Manifest.permission.READ_MEDIA_IMAGES)
            perms.add(Manifest.permission.READ_MEDIA_VIDEO)
            perms.add(Manifest.permission.READ_MEDIA_AUDIO)
            perms.add(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            perms.add(Manifest.permission.READ_EXTERNAL_STORAGE)
        }
        return perms.toTypedArray()
    }

    /** All storage-related permissions that must be granted for local browsing */
    fun requiredForBrowsing(context: Context): Array<String> =
        requiredPermissions(Build.VERSION.SDK_INT)
            .filter { it != Manifest.permission.POST_NOTIFICATIONS }
            .toTypedArray()

    /**
     * Whether every permission needed for local file browsing is granted.
     * On API 30+ this additionally requires the "All files access" special
     * grant — without it FUSE hides directories and non-media files even
     * when READ_MEDIA_* are granted (verified on Android 16 device).
     */
    fun hasBrowsingPermissions(context: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 30 &&
            !Environment.isExternalStorageManager()
        ) {
            return false
        }
        return requiredForBrowsing(context).all {
            ContextCompat.checkSelfPermission(context, it) == android.content.pm.PackageManager.PERMISSION_GRANTED
        }
    }

    /** Intent opening the system "All files access" page for this app (API 30+) */
    fun allFilesAccessIntent(context: Context): Intent =
        Intent(
            Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
            Uri.fromParts("package", context.packageName, null)
        )
}
