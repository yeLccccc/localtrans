package com.localtrans.app.ui.transfers

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FailReasonsTest {

    // ---- localizeFailReason ----

    @Test
    fun `removed maps to manual removal copy`() {
        assertEquals("已手动移除", localizeFailReason("removed"))
    }

    @Test
    fun `refused maps to peer rejected copy`() {
        assertEquals("对方拒绝了本次传输", localizeFailReason("offer refused by peer"))
    }

    @Test
    fun `timeout is case insensitive`() {
        assertEquals("等待超时", localizeFailReason("timeout"))
        assertEquals("等待超时", localizeFailReason("MetaTimeout"))
    }

    @Test
    fun `cancelled and canceled both map to cancelled copy`() {
        assertEquals("传输已取消", localizeFailReason("cancelled by peer"))
        assertEquals("传输已取消", localizeFailReason("canceled"))
    }

    @Test
    fun `disk full maps to storage copy`() {
        assertEquals("存储空间不足", localizeFailReason("disk full"))
    }

    @Test
    fun `illegal filename maps to character copy`() {
        assertEquals("文件名含目标系统不允许的字符", localizeFailReason("非法文件名(含分隔符或非法字符)"))
    }

    @Test
    fun `unknown chinese reason passes through`() {
        assertEquals("对方拒绝连接", localizeFailReason("对方拒绝连接"))
    }

    @Test
    fun `empty reason falls back to generic failure`() {
        assertEquals("传输失败", localizeFailReason(""))
    }

    // ---- isRetryableFailure ----

    @Test
    fun `removed is not retryable`() {
        assertFalse(isRetryableFailure("removed"))
    }

    @Test
    fun `refused is retryable`() {
        assertTrue(isRetryableFailure("offer refused"))
    }

    @Test
    fun `chinese timeout keyword is retryable`() {
        assertTrue(isRetryableFailure("等待超时"))
    }

    @Test
    fun `disk full is not retryable`() {
        assertFalse(isRetryableFailure("disk full"))
    }

    @Test
    fun `illegal filename is not retryable`() {
        assertFalse(isRetryableFailure("非法文件名(含分隔符或非法字符)"))
    }

    @Test
    fun `empty reason is not retryable`() {
        assertFalse(isRetryableFailure(""))
    }
}
