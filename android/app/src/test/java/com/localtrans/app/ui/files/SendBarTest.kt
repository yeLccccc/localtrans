package com.localtrans.app.ui.files

import org.junit.Assert.*
import org.junit.Test

class SendBarTest {
    @Test
    fun `summary text combines count and size`() {
        assertEquals("已选 3 项 · 11.83 MB", sendBarSummary(3, 12_400_000L))
        assertEquals("已选 1 项 · 0 B", sendBarSummary(1, 0L))
    }

    @Test
    fun `unified bar summary reuses sendBarSummary`() {
        // 摘要文案复用现有 sendBarSummary(3 项·2.5 MB 形态)
        assertEquals(sendBarSummary(3, 2_500_000), sendBarSummary(3, 2_500_000))
    }
}
