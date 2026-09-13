package com.localtrans.app.ui.transfers

import org.junit.Assert.*
import org.junit.Test

/**
 * 传输卡展示纯函数单测(M2 T5,移植桌面 transferDisplay.test.ts 口径)
 */
class TransferDisplayTest {

    private fun t(
        jobId: Long = 1,
        state: String = "active",
        total: Long = 1000,
        done: Long = 500,
        remoteDone: Long = 0,
        localRole: String = "receiver",
        direction: String = "rx",
        queuePos: Int? = null,
        startedAtMs: Long? = null,
        finishedAtMs: Long? = null
    ) = TransferUi(
        jobId = jobId, name = "f.bin", total = total, done = done, state = state,
        speedBps = 0, peer = "aa9988112233", direction = direction, localRole = localRole,
        progressPercent = 50, etaSecs = -1, failReason = "", localPath = null,
        remoteDone = remoteDone, instant = false,
        queuePos = queuePos, batchId = null, children = emptyList(),
        partsId = null, startedAtMs = startedAtMs, finishedAtMs = finishedAtMs,
        sourcePath = null
    )

    // ===== progressPair(R1 单进度条主进度口径) =====

    @Test
    fun `receiver main progress is done`() {
        val pair = progressPair(t(localRole = "receiver", done = 500))
        assertEquals(500L, pair.mainDone)
        assertEquals(0.0, pair.backlogPct, 0.001)
    }

    @Test
    fun `sender main progress is remote done`() {
        val pair = progressPair(t(localRole = "source-push", done = 900, remoteDone = 600))
        assertEquals(600L, pair.mainDone)
        assertEquals(30.0, pair.backlogPct, 0.001)
    }

    @Test
    fun `sender without mirror data falls back to done`() {
        // 旧任务无 remote_done 镜像:done>0 且 remote==0 且非终态 → 回退 done,绝不显示 0%
        val pair = progressPair(t(localRole = "source-push", done = 400, remoteDone = 0))
        assertEquals(400L, pair.mainDone)
    }

    @Test
    fun `sender terminal keeps remote as main`() {
        val pair = progressPair(
            t(localRole = "source-push", state = "done", done = 1000, remoteDone = 0)
        )
        assertEquals(0L, pair.mainDone)
    }

    // ===== senderDisplayState / isAwaitingConfirmText =====

    @Test
    fun `full but unconfirmed is awaiting`() {
        val state = senderDisplayState(
            t(localRole = "source-push", done = 1000, remoteDone = 800)
        )
        assertEquals(SenderDisplayState.AWAITING_CONFIRM, state)
    }

    @Test
    fun `backlog over 20 pct is backlog`() {
        val state = senderDisplayState(
            t(localRole = "source-push", done = 500, remoteDone = 100)
        )
        assertEquals(SenderDisplayState.BACKLOG, state)
    }

    @Test
    fun `awaiting text only for non terminal sender`() {
        val active = t(localRole = "source-push", done = 1000, remoteDone = 900)
        assertTrue(isAwaitingConfirmText(active))
        val receiver = t(localRole = "receiver", done = 1000, remoteDone = 900)
        assertFalse(isAwaitingConfirmText(receiver))
        val doneCard = t(
            localRole = "source-push", state = "done", done = 1000, remoteDone = 900
        )
        assertFalse(isAwaitingConfirmText(doneCard))
    }

    // ===== queueText =====

    @Test
    fun `queue text with position`() {
        assertEquals("排队中 · 第 3 位", queueText(3, "push"))
    }

    @Test
    fun `queue text fallback by direction`() {
        assertEquals("排队中，连接对端...", queueText(null, "pull"))
        assertEquals("等待对方接收...", queueText(null, "push"))
    }

    // ===== splitActiveHistory =====

    @Test
    fun `split separates active and history`() {
        val jobs = listOf(
            t(jobId = 1, state = "done"),
            t(jobId = 2, state = "active"),
            t(jobId = 3, state = "pending", queuePos = 2),
            t(jobId = 4, state = "pending", queuePos = 1),
            t(jobId = 5, state = "failed"),
            t(jobId = 6, state = "paused"),
            t(jobId = 7, state = "cancelling")
        )
        val split = splitActiveHistory(jobs)
        // active 排序:active > cancelling > pending(queue_pos 升序) > paused
        assertEquals(listOf(2L, 7L, 4L, 3L, 6L), split.active.map { it.jobId })
        // history 按 finished_at 降序(新的在前,null 靠后)
        assertEquals(setOf(1L, 5L), split.history.map { it.jobId }.toSet())
    }

    @Test
    fun `history sorted by finished desc nulls last`() {
        val jobs = listOf(
            t(jobId = 1, state = "done", finishedAtMs = 100),
            t(jobId = 2, state = "done", finishedAtMs = 300),
            t(jobId = 3, state = "done", finishedAtMs = null),
            t(jobId = 4, state = "done", finishedAtMs = 200)
        )
        val split = splitActiveHistory(jobs)
        assertEquals(listOf(2L, 4L, 1L, 3L), split.history.map { it.jobId })
    }

    // ===== elapsed / fingerprint / child state =====

    @Test
    fun `elapsed freezes at finish`() {
        val card = t(startedAtMs = 1000, finishedAtMs = 61000)
        assertEquals(60L, elapsedSeconds(card, nowMs = 999_000))
        assertNull(elapsedSeconds(t(startedAtMs = null), nowMs = 999_000))
    }

    @Test
    fun `fingerprint abbreviation`() {
        assertEquals("aa9988...112233", fingerprintAbbr("aa9988112233".repeat(2)))
        assertEquals("short", fingerprintAbbr("short"))
    }

    @Test
    fun `peer display name prefers name over fingerprint`() {
        val peers = listOf(PeerNameSource("AA99881122334455", "我的电脑", ""))
        assertEquals("我的电脑", peerDisplayName("aa99881122334455", peers))
        assertEquals("aa9988...334455", peerDisplayName("aa99881122334455", emptyList()))
    }

    @Test
    fun `child state text`() {
        assertEquals("完成", childStateText("done"))
        assertEquals("失败", childStateText("failed"))
        assertEquals("weird", childStateText("weird"))
    }
}
