package com.localtrans.app.util

import org.junit.Assert.*
import org.junit.Test

class FileTypeIconsTest {
    @Test
    fun `maps media and docs by extension`() {
        assertNotNull(FileTypeIcons.iconFor("a.jpg"))
        assertNotNull(FileTypeIcons.iconFor("b.pdf"))
        assertNotNull(FileTypeIcons.iconFor("c.zip"))
        assertNotNull(FileTypeIcons.nothing())
    }
}
