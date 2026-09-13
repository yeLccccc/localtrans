package com.localtrans.app.util

import android.Manifest
import android.os.Build
import org.junit.Assert.*
import org.junit.Test

/**
 * Tests for storage permission set selection by API level
 */
class StoragePermissionsTest {

    @Test
    fun `api 33+ requires read media permissions`() {
        val perms = StoragePermissions.requiredPermissions(33)
        assertTrue(perms.contains(Manifest.permission.READ_MEDIA_IMAGES))
        assertTrue(perms.contains(Manifest.permission.READ_MEDIA_VIDEO))
        assertTrue(perms.contains(Manifest.permission.READ_MEDIA_AUDIO))
        assertFalse(perms.contains(Manifest.permission.READ_EXTERNAL_STORAGE))
    }

    @Test
    fun `api 32 and below requires read external storage`() {
        val perms = StoragePermissions.requiredPermissions(29)
        assertTrue(perms.contains(Manifest.permission.READ_EXTERNAL_STORAGE))
        assertFalse(perms.contains(Manifest.permission.READ_MEDIA_IMAGES))
    }

    @Test
    fun `notification permission only on api 33+`() {
        assertTrue(StoragePermissions.requiredPermissions(33).contains(Manifest.permission.POST_NOTIFICATIONS))
        assertFalse(StoragePermissions.requiredPermissions(31).contains(Manifest.permission.POST_NOTIFICATIONS))
    }
}
