package com.localtrans.app.ui.devices

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.*
import org.junit.Before
import org.junit.After
import org.junit.Test
import uniffi.localtrans_ffi.AppEvent
import com.localtrans.app.data.FakeDevicesRepo
import uniffi.localtrans_ffi.DeviceDto

@OptIn(ExperimentalCoroutinesApi::class)
class DevicesViewModelTest {

    private val testDispatcher = StandardTestDispatcher()

    private lateinit var viewModel: DevicesViewModel
    private lateinit var fakeRepo: FakeDevicesRepo

    @Before
    fun setup() {
        Dispatchers.setMain(testDispatcher)
        fakeRepo = FakeDevicesRepo()
        viewModel = DevicesViewModel(fakeRepo)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `consent event shows dialog`() = runTest {
        // Given: A consent requested event
        val event = AppEvent.ConsentRequested("FP123", "Test Device")

        // When: Event is emitted
        fakeRepo.emitEvent(event); testDispatcher.scheduler.advanceUntilIdle()

        // Then: Pairing dialog should be visible
        val uiState = viewModel.uiState.value
        assertNotNull(uiState.pairing)
        assertEquals("FP123", uiState.pairing?.fingerprint)
        assertEquals("Test Device", uiState.pairing?.peerName)
        assertEquals(PairingState.CONSENT_REQUESTED, uiState.pairing?.state)
    }

    @Test
    fun `code shown event displays code`() = runTest {
        // Given: A pairing code shown event
        val event = AppEvent.PairingCodeShown("FP123", "123456")

        // When: Event is emitted
        fakeRepo.emitEvent(event); testDispatcher.scheduler.advanceUntilIdle()

        // Then: Pairing code should be displayed
        val uiState = viewModel.uiState.value
        assertNotNull(uiState.pairing)
        assertEquals("FP123", uiState.pairing?.fingerprint)
        assertEquals("123456", uiState.pairing?.code)
        assertEquals(PairingState.CODE_SHOWN, uiState.pairing?.state)
    }

    @Test
    fun `session up marks connected`() = runTest {
        // Given: A device exists and session up event
        val device = DeviceDto(
            fingerprint = "FP123",
            name = "Test Device",
            addr = "192.168.1.100",
            online = true,
            connected = false,
            viaRelay = false,
            forceRelay = false
        )
        fakeRepo.addDevice(device)
        fakeRepo.emitEvent(AppEvent.DevicesChanged)
        testDispatcher.scheduler.advanceUntilIdle()

        // When: Session up event is emitted
        fakeRepo.emitEvent(AppEvent.SessionUp("FP123", "Test Device"))
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: Device should be marked as connected
        val devices = viewModel.devices.value
        val connectedDevice = devices.find { it.fingerprint == "FP123" }
        assertNotNull(connectedDevice)
        assertTrue(connectedDevice?.connected == true)
    }

    @Test
    fun `devices changed refreshes list`() = runTest {
        // Given: New devices list
        val devices = listOf(
            DeviceDto(
                fingerprint = "FP123",
                name = "Device 1",
                addr = "192.168.1.100",
                online = true,
                connected = false,
                viaRelay = false,
            forceRelay = false
            ),
            DeviceDto(
                fingerprint = "FP456",
                name = "Device 2",
                addr = "192.168.1.101",
                online = false,
                connected = false,
                viaRelay = false,
            forceRelay = false
            )
        )
        devices.forEach { fakeRepo.addDevice(it) }

        // When: Devices changed event is emitted
        fakeRepo.emitEvent(AppEvent.DevicesChanged)
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: Devices list should be updated
        val viewModelDevices = viewModel.devices.value
        assertEquals(2, viewModelDevices.size)
        assertEquals("Device 1", viewModelDevices[0].name)
        assertEquals("Device 2", viewModelDevices[1].name)
    }

    @Test
    fun `code entry times out to failed after consent timeout`() = runTest {
        val repo = FakeDevicesRepo()
        val vm = DevicesViewModel(repo)
        // Let the ViewModel initialize fully
        testDispatcher.scheduler.runCurrent()
        repo.emitEvent(AppEvent.PairingCodeEntry(fingerprint = "fp1", name = "PC"))
        testDispatcher.scheduler.runCurrent()
        assertEquals(PairingState.CODE_ENTRY, vm.uiState.value.pairing?.state)
        testDispatcher.scheduler.advanceTimeBy(31_000)
        testDispatcher.scheduler.runCurrent()
        assertEquals(PairingState.FAILED, vm.uiState.value.pairing?.state)
        assertEquals("等待对端确认超时", vm.uiState.value.pairing?.reason)
    }

    @Test
    fun `submit code error surfaces as failed`() = runTest {
        val repo = FakeDevicesRepo()
        repo.submitError = RuntimeException("控制流已关闭")
        val vm = DevicesViewModel(repo)
        testDispatcher.scheduler.runCurrent()
        repo.emitEvent(AppEvent.PairingCodeEntry(fingerprint = "fp1", name = "PC"))
        testDispatcher.scheduler.runCurrent()
        vm.submitPairingCode("fp1", "123456")
        testDispatcher.scheduler.runCurrent()
        assertEquals(PairingState.FAILED, vm.uiState.value.pairing?.state)
        assertTrue(vm.uiState.value.pairing?.reason?.contains("控制流已关闭") == true)
    }

    @Test
    fun `local ip loaded into state`() = runTest {
        fakeRepo.fakeLocalIp = "10.0.0.5"
        val vm = DevicesViewModel(fakeRepo)
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals("10.0.0.5", vm.uiState.value.localIp)
    }

    @Test
    fun `probe finds device after delay`() = runTest {
        fakeRepo.addDevice(DeviceDto(fingerprint = "FP1", name = "PC", addr = "192.168.1.100:47601", online = true, connected = false, viaRelay = false, forceRelay = false))
        viewModel.probeDevice("192.168.1.100")
        assertEquals(ProbeState.PROBING, viewModel.uiState.value.manualProbe?.state)
        testDispatcher.scheduler.advanceTimeBy(5000)
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(ProbeState.FOUND, viewModel.uiState.value.manualProbe?.state)
        assertEquals("192.168.1.100", fakeRepo.lastProbedAddr)
    }

    @Test
    fun `probe reports not found when no device matches`() = runTest {
        viewModel.probeDevice("192.168.1.200")
        testDispatcher.scheduler.advanceTimeBy(5000)
        testDispatcher.scheduler.advanceUntilIdle()
        val p = viewModel.uiState.value.manualProbe
        assertNotNull(p)
        assertEquals(ProbeState.NOT_FOUND, p?.state)
        assertTrue(p?.message?.contains("隐身") == true)
    }

    @Test
    fun `probe reports error when send fails`() = runTest {
        fakeRepo.probeError = RuntimeException("boom")
        viewModel.probeDevice("192.168.1.200")
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(ProbeState.ERROR, viewModel.uiState.value.manualProbe?.state)
    }

    @Test
    fun `clear manual probe cancels pending job`() = runTest {
        viewModel.probeDevice("192.168.1.200")
        viewModel.clearManualProbe()
        testDispatcher.scheduler.advanceTimeBy(6000)
        testDispatcher.scheduler.advanceUntilIdle()
        assertNull(viewModel.uiState.value.manualProbe)
    }

    // ===== M3c T2 通道面板:手动单对端快检 =====

    @Test
    fun `probe peer now calls repo and clears probing state`() = runTest {
        viewModel.probeNowPeer("FP1")
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals("FP1", fakeRepo.lastProbedPeer)
        assertNull("快检返回后探测态清除", viewModel.probingPeer.value)
    }

    @Test
    fun `probe peer now error surfaces on snackbar`() = runTest {
        fakeRepo.probePeerError = RuntimeException("设备未连接,无法探测")
        viewModel.probeNowPeer("FP1")
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals("设备未连接,无法探测", viewModel.uiState.value.error)
        assertNull("失败同样清除探测态", viewModel.probingPeer.value)
    }

    // ===== M3c T3 强制走中继 =====

    @Test
    fun `set force relay persists via repo and updates list`() = runTest {
        // init 的 loadDevices 是协程,StandardTestDispatcher 下需推进才装载初始列表
        testDispatcher.scheduler.advanceUntilIdle()
        fakeRepo.addDevice(
            DeviceDto(
                fingerprint = "FP1",
                name = "PC",
                addr = "192.168.1.100:47601",
                online = true,
                connected = false,
                viaRelay = false,
                forceRelay = false
            )
        )
        // addDevice 只进 fakeRepo 表——用 DevicesChanged 事件触发 VM 重拉
        // (通过的同类测试同款手法: emitEvent + advanceUntilIdle)
        fakeRepo.emitEvent(AppEvent.DevicesChanged)
        testDispatcher.scheduler.advanceUntilIdle()
        viewModel.setForceRelay("FP1", true)
        testDispatcher.scheduler.advanceUntilIdle()
        assertTrue("repo 应持久化开关", fakeRepo.isForceRelay("FP1"))
        assertTrue("本地列表应乐观更新", viewModel.devices.value.first { it.fingerprint == "FP1" }.forceRelay)

        viewModel.setForceRelay("FP1", false)
        testDispatcher.scheduler.advanceUntilIdle()
        assertFalse("关闭后 repo 清除", fakeRepo.isForceRelay("FP1"))
        assertFalse("关闭后本地列表更新", viewModel.devices.value.first { it.fingerprint == "FP1" }.forceRelay)
    }
}

