package com.localtrans.app.util

import org.junit.Assert.*
import org.junit.Test

class FileResolverTest {

    @Test
    fun `format file size bytes`() {
        assertEquals("0 B", Formatters.formatFileSize(0uL))
        assertEquals("512 B", Formatters.formatFileSize(512uL))
        assertEquals("1000 B", Formatters.formatFileSize(1000uL))
    }

    @Test
    fun `format file size KB`() {
        assertEquals("1.00 KB", Formatters.formatFileSize(1024uL))
        assertEquals("1.50 KB", Formatters.formatFileSize(1536uL))
        assertEquals("100.00 KB", Formatters.formatFileSize(102400uL))
    }

    @Test
    fun `format file size MB`() {
        assertEquals("1.00 MB", Formatters.formatFileSize(1048576uL))
        assertEquals("1.50 MB", Formatters.formatFileSize(1572864uL))
        assertEquals("100.00 MB", Formatters.formatFileSize(104857600uL))
    }

    @Test
    fun `format file size GB`() {
        assertEquals("1.00 GB", Formatters.formatFileSize(1073741824uL))
        assertEquals("1.50 GB", Formatters.formatFileSize(1610612736uL))
    }

    @Test
    fun `format duration seconds`() {
        assertEquals("0:00", Formatters.formatDuration(0))
        assertEquals("0:30", Formatters.formatDuration(30))
        assertEquals("0:59", Formatters.formatDuration(59))
    }

    @Test
    fun `format duration minutes seconds`() {
        assertEquals("1:00", Formatters.formatDuration(60))
        assertEquals("1:30", Formatters.formatDuration(90))
        assertEquals("59:59", Formatters.formatDuration(3599))
    }

    @Test
    fun `format duration hours`() {
        assertEquals("1:00:00", Formatters.formatDuration(3600))
        assertEquals("1:30:45", Formatters.formatDuration(5445))
    }

    @Test
    fun `sanitize filename removes invalid chars`() {
        // Test removal of invalid characters
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file/name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file\\name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file:name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file*name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file?name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file\"name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file<name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file>name.txt"))
        assertEquals("file_name.txt", FileResolver.sanitizeFilename("file|name.txt"))
    }

    @Test
    fun `sanitize filename handles multiple invalid chars`() {
        assertEquals("file_na_me__test.txt", FileResolver.sanitizeFilename("file/na*me?:test.txt"))
    }

    @Test
    fun `sanitize filename preserves valid chars`() {
        assertEquals("file-name_123.txt", FileResolver.sanitizeFilename("file-name_123.txt"))
        assertEquals("file.name.txt", FileResolver.sanitizeFilename("file.name.txt"))
    }

    @Test
    fun `get file extension`() {
        assertEquals("txt", Formatters.getFileExtension("file.txt"))
        assertEquals("jpg", Formatters.getFileExtension("photo.jpg"))
        assertEquals("pdf", Formatters.getFileExtension("document.pdf"))
    }

    @Test
    fun `get file extension no extension`() {
        assertEquals("", Formatters.getFileExtension("filename"))
        assertEquals("", Formatters.getFileExtension("path/to/filename"))
    }

    @Test
    fun `get file extension multiple dots`() {
        assertEquals("txt", Formatters.getFileExtension("file.name.txt"))
        assertEquals("gz", Formatters.getFileExtension("file.tar.gz"))
    }

    // Note: URI resolution tests require Android Context and ContentResolver
    // These must be verified on emulator/device. The following behaviors are tested manually:
    // - resolveUris() copies SAF URIs to cacheDir
    // - cleanup() removes cached files
    // - getCacheDir() creates saf_files subdirectory
}
