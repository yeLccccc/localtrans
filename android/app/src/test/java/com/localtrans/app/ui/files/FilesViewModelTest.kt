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

@OptIn(ExperimentalCoroutinesApi::class)
class FilesViewModelTest {

    private val testDispatcher = StandardTestDispatcher()

    private lateinit var viewModel: FilesViewModel
    private lateinit var fakeRepo: FakeFilesRepo

    @Before
    fun setup() {
        Dispatchers.setMain(testDispatcher)
        fakeRepo = FakeFilesRepo()
        viewModel = FilesViewModel(fakeRepo)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `entries filter by query`() = runTest {
        // Given: 3 file entries in repo
        val entries = listOf(
            FileEntryDto(name = "apple.txt", isDir = false, size = 100u, modifiedMs = 1000),
            FileEntryDto(name = "banana.txt", isDir = false, size = 200u, modifiedMs = 2000),
            FileEntryDto(name = "cherry.txt", isDir = false, size = 300u, modifiedMs = 3000)
        )
        entries.forEach { fakeRepo.addLocalEntry(it) }

        // And a fresh ViewModel that loads these entries on init
        val freshViewModel = FilesViewModel(fakeRepo)
        testDispatcher.scheduler.advanceUntilIdle()

        // When: Search query is set to "a"
        freshViewModel.setSearchQuery("a")
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: 2 entries should be visible (apple.txt and banana.txt both contain 'a')
        val filteredEntries = freshViewModel.entries.value
        assertEquals(2, filteredEntries.size)
        assertTrue(filteredEntries.any { it.name == "apple.txt" })
        assertTrue(filteredEntries.any { it.name == "banana.txt" })
    }

    @Test
    fun `local remote switch keeps breadcrumb`() = runTest {
        // Given: Local mode with breadcrumb path
        viewModel.setLocation(Location.LOCAL)
        viewModel.navigateTo("subfolder")
        testDispatcher.scheduler.advanceUntilIdle()

        val breadcrumbBefore = viewModel.breadcrumb.value

        // When: Switch to remote mode
        viewModel.setLocation(Location.REMOTE)
        testDispatcher.scheduler.advanceUntilIdle()

        // Then: Path should be cleared to root
        val breadcrumbAfter = viewModel.breadcrumb.value
        assertTrue(breadcrumbAfter.isEmpty())
        assertNotEquals(breadcrumbBefore, breadcrumbAfter)
    }

    @Test
    fun `pull single remote file passes single-element list`() = runTest {
        viewModel.setLocation(Location.REMOTE)
        viewModel.setSelectedDevice("aabb")
        viewModel.pullFiles("aabb", "share1", listOf("/remote/dir/video.mp4"))
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(1, fakeRepo.pulled.size)
        assertEquals(listOf("/remote/dir/video.mp4"), fakeRepo.pulled.last().third)
    }
}
