package com.localtrans.app.util

import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream

/**
 * FileResolver - Resolves SAF content URIs to absolute file paths
 *
 * This utility handles Android Storage Access Framework (SAF) URIs by copying
 * content to app's cache directory and returning absolute paths.
 */
object FileResolver {

    /**
     * Resolve SAF URIs to absolute file paths
     *
     * @param context Android context
     * @param uris List of content URIs from SAF picker
     * @return List of absolute file paths in cache directory
     */
    fun resolveUris(context: Context, uris: List<Uri>): List<String> {
        val cacheDir = getCacheDir(context)
        val resolvedPaths = mutableListOf<String>()

        uris.forEach { uri ->
            try {
                // Get filename from URI
                val filename = getFilename(context, uri) ?: "saf_file_${System.currentTimeMillis()}"
                val sanitizedFilename = sanitizeFilename(filename)
                val targetFile = cacheDir.resolve(sanitizedFilename)

                // Copy content to cache
                context.contentResolver.openInputStream(uri)?.use { input ->
                    FileOutputStream(targetFile).use { output ->
                        input.copyTo(output)
                    }
                }

                resolvedPaths.add(targetFile.absolutePath)
            } catch (e: Exception) {
                // Skip files that fail to resolve
            }
        }

        return resolvedPaths
    }

    /**
     * Get filename from URI
     */
    private fun getFilename(context: Context, uri: Uri): String? {
        var filename: String? = null

        // Try to get filename from OpenableColumns
        context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (nameIndex >= 0) {
                    filename = cursor.getString(nameIndex)
                }
            }
        }

        return filename
    }

    /**
     * Get cache directory for file copies
     */
    fun getCacheDir(context: Context): File {
        return context.cacheDir.resolve("saf_files").also { it.mkdirs() }
    }

    /**
     * Clean up cached files
     */
    fun cleanup(context: Context) {
        val cacheDir = getCacheDir(context)
        cacheDir.listFiles()?.forEach { it.delete() }
    }

    /**
     * Sanitize filename by replacing invalid characters with underscores
     * Invalid characters: / \ : * ? " < > |
     */
    fun sanitizeFilename(filename: String): String {
        val invalidChars = setOf('/', '\\', ':', '*', '?', '"', '<', '>', '|')
        return filename.map { if (it in invalidChars) '_' else it }.joinToString("")
    }
}
