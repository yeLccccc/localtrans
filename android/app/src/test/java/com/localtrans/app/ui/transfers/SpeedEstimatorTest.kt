package com.localtrans.app.ui.transfers

import org.junit.Assert.*
import org.junit.Test

class SpeedEstimatorTest {
    @Test
    fun `computes bps from sliding window`() {
        // 3s 窗内:0ms→0B, 3000ms→3MB → 1MB/s = 1_048_576 B/s 附近(整数除法 1048576)
        val s = listOf(0L to 0L, 3000L to 3_145_728L)
        assertEquals(1_048_576L, SpeedEstimator.estimate(s))
    }

    @Test
    fun `single sample yields zero`() {
        assertEquals(0L, SpeedEstimator.estimate(listOf(1000L to 500L)))
    }

    @Test
    fun `eta divides remaining by speed`() {
        assertEquals(2L, SpeedEstimator.etaSecs(remainingBytes = 2_097_152L, bps = 1_048_576L))
        assertEquals(-1L, SpeedEstimator.etaSecs(2_097_152L, 0L))
    }
}
