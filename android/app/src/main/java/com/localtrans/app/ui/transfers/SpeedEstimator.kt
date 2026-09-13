package com.localtrans.app.ui.transfers

/** 近 3 秒 (timestampMs, doneBytes) 滑动窗差分测速;纯函数可单测 */
object SpeedEstimator {
    fun estimate(samples: List<Pair<Long, Long>>): Long {
        if (samples.size < 2) return 0L
        val (t0, b0) = samples.first()
        val (t1, b1) = samples.last()
        val dtMs = t1 - t0
        if (dtMs <= 0) return 0L
        return (b1 - b0) * 1000L / dtMs
    }

    fun etaSecs(remainingBytes: Long, bps: Long): Long =
        if (bps <= 0) -1L else remainingBytes / bps
}
