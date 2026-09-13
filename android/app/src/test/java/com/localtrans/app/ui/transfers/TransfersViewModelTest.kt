package com.localtrans.app.ui.transfers

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.*
import org.junit.Before
import org.junit.After
import org.junit.Test
import uniffi.localtrans_ffi.AppEvent
import uniffi.localtrans_ffi.TransferDto
import com.localtrans.app.data.FakeTransfersRepo

@OptIn(ExperimentalCoroutinesApi::class)
class TransfersViewModelTest {

    private val testDispatcher = StandardTestDispatcher()

    private lateinit var viewModel: TransfersViewModel
    private lateinit var fakeRepo: FakeTransfersRepo

    @Before
    fun setup() {
        Dispatchers.setMain(testDispatcher)
        fakeRepo = FakeTransfersRepo()
        viewModel = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `offer requested no longer drives page-level sheet`() = runTest {
        // 弹窗已上移 AppNav 根层全局处理,ViewModel 不再写 offerSheet
        val deadline = System.currentTimeMillis() + 60_000
        val event = AppEvent.OfferRequested(
            jobId = 123u,
            peerName = "Test Device",
            fileCount = 3u,
            totalSize = 1024000u,
            deadlineEpochMs = deadline
        )

        fakeRepo.emitEvent(event)
        testDispatcher.scheduler.advanceUntilIdle()

        assertNull(viewModel.uiState.value.offerSheet)
    }

    @Test
    fun `transfer updated for unknown job inserts new row`() = runTest {
        // 接收侧 bug 修复:事件先于列表加载到达(或任务从未进过表)时,
        // TransferUpdated 必须插行而不是丢弃——否则对方推送的任务在传输页凭空消失
        val event = AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 777u,
                name = "incoming.mp4",
                total = 2000u,
                done = 500u,
                state = "active",
                speedBps = 0u,
                peer = "Test Device",
                direction = "rx",
                localRole = "receiver",
                progressPercent = 25u,
                etaSecs = -1,
                failReason = "",
                localPath = null,
                remoteDone = 0u,
                instant = false,
                // M2 T1 契约扩展新字段(本阶段恒空/None)
                queuePos = null, batchId = null, children = emptyList(),
                partsId = null, startedAtMs = null, finishedAtMs = null,
                sourcePath = null, health = null
            )
        )

        fakeRepo.emitEvent(event)
        testDispatcher.scheduler.advanceUntilIdle()

        val inserted = viewModel.transfers.value.find { it.jobId == 777L }
        assertNotNull("未知任务的 TransferUpdated 应插入新行", inserted)
        assertEquals("active", inserted?.state)
    }

    @Test
    fun `transfer updated for existing job replaces row`() = runTest {
        fakeRepo.addTransfer(TransferDto(
            jobId = 123u, name = "old.txt", total = 1000u, done = 100u,
            state = "active", speedBps = 0u, peer = "Test Device",
            direction = "rx", localRole = "receiver", progressPercent = 10u,
            etaSecs = -1, failReason = "", localPath = null,
            remoteDone = 0u, instant = false,
            queuePos = null, batchId = null, children = emptyList(),
            partsId = null, startedAtMs = null, finishedAtMs = null,
            sourcePath = null, health = null
        ))
        val fresh = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        fakeRepo.emitEvent(AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 123u, name = "old.txt", total = 1000u, done = 900u,
                state = "active", speedBps = 0u, peer = "Test Device",
                direction = "rx", localRole = "receiver", progressPercent = 90u,
                etaSecs = -1, failReason = "", localPath = null,
                remoteDone = 0u, instant = false,
                queuePos = null, batchId = null, children = emptyList(),
                partsId = null, startedAtMs = null, finishedAtMs = null,
                sourcePath = null, health = null
            )
        ))
        testDispatcher.scheduler.advanceUntilIdle()

        val list = fresh.transfers.value
        assertEquals(1, list.count { it.jobId == 123L })
        assertEquals(900L, list.first { it.jobId == 123L }.done)
    }

    @Test
    fun `transfer done ok updates state`() = runTest {
        // Given: A transfer exists in repo
        val transfer = TransferDto(
            jobId = 123u,
            name = "test.txt",
            total = 1000u,
            done = 1000u,
            state = "running",
            speedBps = 0u,
            peer = "Test Device",
            direction = "rx",
            localRole = "receiver",
            progressPercent = 100u,
            etaSecs = -1,
            failReason = "",
            localPath = null,
            remoteDone = 0u,
            instant = false,
            queuePos = null, batchId = null, children = emptyList(),
            partsId = null, startedAtMs = null, finishedAtMs = null,
            sourcePath = null, health = null
        )
        fakeRepo.addTransfer(transfer)

        // And a fresh ViewModel that loads the transfer on init
        val freshViewModel = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        // When: Transfer done event with ok=true
        val event = AppEvent.TransferDone(
            jobId = 123u,
            ok = true,
            failReason = ""
        )
        fakeRepo.emitEvent(event)
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: Transfer state should be done
        val transfers = freshViewModel.transfers.value
        val doneTransfer = transfers.find { it.jobId == 123L }
        assertNotNull(doneTransfer)
        assertEquals("done", doneTransfer?.state)
        assertEquals("", doneTransfer?.failReason)
    }

    @Test
    fun `failed with reason shows fail reason`() = runTest {
        // Given: A transfer exists
        val transfer = TransferDto(
            jobId = 123u,
            name = "test.txt",
            total = 1000u,
            done = 500u,
            state = "failed",
            speedBps = 0u,
            peer = "Test Device",
            direction = "rx",
            localRole = "receiver",
            progressPercent = 50u,
            etaSecs = -1,
            failReason = "",
            localPath = null,
            remoteDone = 0u,
            instant = false,
            queuePos = null, batchId = null, children = emptyList(),
            partsId = null, startedAtMs = null, finishedAtMs = null,
            sourcePath = null, health = null
        )
        fakeRepo.addTransfer(transfer)

        // And a fresh ViewModel that loads the transfer on init
        val freshViewModel = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        // When: Transfer done event with ok=false and reason
        val event = AppEvent.TransferDone(
            jobId = 123u,
            ok = false,
            failReason = "对方超时未确认"
        )
        fakeRepo.emitEvent(event)
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: Transfer should show failed with reason
        val transfers = freshViewModel.transfers.value
        val failedTransfer = transfers.find { it.jobId == 123L }
        assertNotNull(failedTransfer)
        assertEquals("failed", failedTransfer?.state)
        assertEquals("对方超时未确认", failedTransfer?.failReason)
    }

    @Test
    fun `transfer ui exposes remote done and instant`() = runTest {
        fakeRepo.emitEvent(AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 88u, name = "big.mp4", total = 1000u, done = 700u,
                state = "active", speedBps = 0u, peer = "PC", direction = "push",
                localRole = "source-push", progressPercent = 70u, etaSecs = -1,
                failReason = "", localPath = null,
                remoteDone = 500u, instant = false,
                queuePos = null, batchId = null, children = emptyList(),
                partsId = null, startedAtMs = null, finishedAtMs = null,
                sourcePath = null, health = null
            )
        ))
        testDispatcher.scheduler.advanceUntilIdle()
        val t = viewModel.transfers.value.first { it.jobId == 88L }
        assertEquals(500L, t.remoteDone)

        fakeRepo.emitEvent(AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 89u, name = "dup.bin", total = 10u, done = 10u,
                state = "done", speedBps = 0u, peer = "PC", direction = "rx",
                localRole = "receiver", progressPercent = 100u, etaSecs = -1,
                failReason = "", localPath = null,
                remoteDone = 0u, instant = true,
                queuePos = null, batchId = null, children = emptyList(),
                partsId = null, startedAtMs = null, finishedAtMs = null,
                sourcePath = null, health = null
            )
        ))
        testDispatcher.scheduler.advanceUntilIdle()
        assertTrue(viewModel.transfers.value.first { it.jobId == 89L }.instant)
    }

    // ===== M2 T5:两级删除 / removed 事件 / 磁盘历史 =====

    private fun dto(
        jobId: Long,
        state: String,
        queuePos: UInt? = null
    ) = TransferDto(
        jobId = jobId.toULong(), name = "n-$jobId.bin", total = 1000u, done = if (state == "done") 1000u else 100u,
        state = state, speedBps = 0u, peer = "PC", direction = "rx", localRole = "receiver",
        progressPercent = 10u, etaSecs = -1, failReason = "", localPath = null,
        remoteDone = 0u, instant = false,
        queuePos = queuePos, batchId = null, children = emptyList(),
        partsId = null, startedAtMs = null, finishedAtMs = null,
        sourcePath = null, health = null
    )

    @Test
    fun `removed event drops row instead of marking failed`() = runTest {
        fakeRepo.addTransfer(dto(42, "done"))
        val fresh = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        // 两级删除/取消收尾:FFI 补发 TransferDone(removed) —— 行应消失而非变 failed
        fakeRepo.emitEvent(AppEvent.TransferDone(jobId = 42u, ok = false, failReason = "removed"))
        testDispatcher.scheduler.advanceUntilIdle()

        assertNull(fresh.transfers.value.find { it.jobId == 42L })
    }

    @Test
    fun `queue pos from dto is surfaced`() = runTest {
        fakeRepo.emitEvent(AppEvent.TransferUpdated(transfer = dto(9, "pending", queuePos = 3u)))
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(3, viewModel.transfers.value.first { it.jobId == 9L }.queuePos)
    }

    @Test
    fun `terminal remove with view level calls repo and drops row`() = runTest {
        fakeRepo.addTransfer(dto(7, "done"))
        val fresh = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        fresh.removeTransfer(7uL, "view")
        testDispatcher.scheduler.advanceUntilIdle()

        assertEquals(listOf(7uL to "view"), fakeRepo.removeLevelCalls)
        assertNull(fresh.transfers.value.find { it.jobId == 7L })
    }

    @Test
    fun `cancel on active card routes to destroy level`() = runTest {
        fakeRepo.addTransfer(dto(5, "active"))
        val fresh = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        fresh.cancelTransfer(5uL)
        testDispatcher.scheduler.advanceUntilIdle()

        assertEquals(listOf(5uL to "destroy"), fakeRepo.removeLevelCalls)
        assertNull(fresh.transfers.value.find { it.jobId == 5L })
    }

    @Test
    fun `clear finished walks view level per terminal card`() = runTest {
        fakeRepo.addTransfer(dto(1, "active"))
        fakeRepo.addTransfer(dto(2, "done"))
        fakeRepo.addTransfer(dto(3, "failed"))
        val fresh = TransfersViewModel(fakeRepo, enableCountdownTicker = false)
        testDispatcher.scheduler.advanceUntilIdle()

        fresh.clearFinishedTransfers()
        testDispatcher.scheduler.advanceUntilIdle()

        // 只清终态,活动卡不受影响;每张终态卡走 view 级
        assertEquals(
            listOf(2uL to "view", 3uL to "view"),
            fakeRepo.removeLevelCalls
        )
        assertEquals(1, fresh.transfers.value.size)
        assertEquals("active", fresh.transfers.value.first().state)
    }

    @Test
    fun `disk history load restore destroy`() = runTest {
        val diskJob = uniffi.localtrans_ffi.DiskJobDto(
            jobId = 11u, displayName = "disk.bin", total = 100u, done = 100u,
            state = "interrupted", direction = "pull", peerHex = "ff",
            createdAtMs = 1_700_000_000_000, removedFromView = true
        )
        fakeRepo.setDiskJobs(listOf(diskJob))

        viewModel.loadDiskJobs()
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(1, viewModel.diskJobs.value.size)

        viewModel.restoreDiskJob(11uL)
        testDispatcher.scheduler.advanceUntilIdle()
        // Fake restore 清空磁盘列表(真实现走 TransferUpdated 事件回列表)
        assertTrue(viewModel.diskJobs.value.isEmpty())

        fakeRepo.setDiskJobs(listOf(diskJob))
        viewModel.destroyDiskJob(11uL)
        testDispatcher.scheduler.advanceUntilIdle()
        assertTrue(viewModel.diskJobs.value.isEmpty())
    }
}
