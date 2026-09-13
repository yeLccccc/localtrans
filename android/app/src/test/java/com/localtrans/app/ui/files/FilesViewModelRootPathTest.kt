package com.localtrans.app.ui.files

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.*
import org.junit.Before
import org.junit.After
import org.junit.Test
import uniffi.localtrans_ffi.FileEntryDto
import com.localtrans.app.data.FakeFilesRepo

/**
 * Tests that FilesViewModel lists from a real absolute root path
 * instead of the meaningless empty string.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FilesViewModelRootPathTest {

    private val testDispatcher = StandardTestDispatcher()

    @Before
    fun setup() {
        Dispatchers.setMain(testDispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `init lists local entries from default root`() = runTest {
        val fakeRepo = FakeFilesRepo()
        fakeRepo.addLocalEntry(FileEntryDto(name = "download", isDir = true, size = 0u, modifiedMs = 0))

        val viewModel = FilesViewModel(fakeRepo, defaultLocalRoot = "/storage/emulated/0")

        testDispatcher.scheduler.advanceUntilIdle()

        // The repo must have been queried with the default root, not ""
        assertEquals("/storage/emulated/0", fakeRepo.lastLocalDir)
        assertEquals(1, viewModel.entries.value.size)
    }

    @Test
    fun `navigate down and up uses joined paths`() = runTest {
        val fakeRepo = FakeFilesRepo()
        val viewModel = FilesViewModel(fakeRepo, defaultLocalRoot = "/storage/emulated/0")
        testDispatcher.scheduler.advanceUntilIdle()

        viewModel.navigateTo("/storage/emulated/0/DCIM")
        testDispatcher.scheduler.advanceUntilIdle()

        assertEquals("/storage/emulated/0/DCIM", fakeRepo.lastLocalDir)

        viewModel.navigateUp()
        testDispatcher.scheduler.advanceUntilIdle()

        assertEquals("/storage/emulated/0", fakeRepo.lastLocalDir)
    }

    @Test
    fun `navigate to root resets to default root`() = runTest {
        val fakeRepo = FakeFilesRepo()
        val viewModel = FilesViewModel(fakeRepo, defaultLocalRoot = "/storage/emulated/0")
        testDispatcher.scheduler.advanceUntilIdle()

        viewModel.navigateTo("/storage/emulated/0/DCIM")
        testDispatcher.scheduler.advanceUntilIdle()
        viewModel.navigateToRoot()
        testDispatcher.scheduler.advanceUntilIdle()

        assertEquals("/storage/emulated/0", fakeRepo.lastLocalDir)
    }
}
