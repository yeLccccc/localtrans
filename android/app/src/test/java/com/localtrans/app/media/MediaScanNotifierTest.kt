package com.localtrans.app.media

import org.junit.Assert.*
import org.junit.Test

class MediaScanNotifierTest {
    @Test
    fun `image and video extensions are scannable`() {
        assertTrue(MediaScanNotifier.isScannableExtension("photo.JPG"))
        assertTrue(MediaScanNotifier.isScannableExtension("clip.mp4"))
        assertTrue(MediaScanNotifier.isScannableExtension("img.heic"))
    }

    @Test
    fun `document extensions are not scannable`() {
        assertFalse(MediaScanNotifier.isScannableExtension("report.pdf"))
        assertFalse(MediaScanNotifier.isScannableExtension("archive.zip"))
        assertFalse(MediaScanNotifier.isScannableExtension("noext"))
    }
}
