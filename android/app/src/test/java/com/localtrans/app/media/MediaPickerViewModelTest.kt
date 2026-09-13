package com.localtrans.app.media

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

@OptIn(ExperimentalCoroutinesApi::class)
class MediaPickerViewModelTest {
    private val testDispatcher = StandardTestDispatcher()

    @Before fun setup() { Dispatchers.setMain(testDispatcher) }
    @After fun tearDown() { Dispatchers.resetMain() }

    private class FakeMediaRepo(initial: List<MediaItem> = emptyList()) : MediaRepo {
        private var _items = initial.toMutableList()
        override fun queryImages() = _items.filter { it.durationMs == 0L }
        override fun queryVideos() = _items.filter { it.durationMs > 0L }
        fun setItems(items: List<MediaItem>) { _items = items.toMutableList() }
    }

    private fun repoWith(vararg items: MediaItem) = FakeMediaRepo(items.toList())

    @Test
    fun `filter switches between photos and videos`() = runTest {
        val vm = MediaPickerViewModel(repoWith(
            MediaItem(1, "/a.jpg", "a.jpg", 1, 1),
            MediaItem(2, "/v.mp4", "v.mp4", 1, 1, durationMs = 3000),
        ), ioDispatcher = testDispatcher)
        testDispatcher.scheduler.advanceUntilIdle()
        vm.setFilter(MediaFilter.PHOTOS); testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(listOf("/a.jpg"), vm.items.value.map { it.path })
        vm.setFilter(MediaFilter.VIDEOS); testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(listOf("/v.mp4"), vm.items.value.map { it.path })
    }

    @Test
    fun `toggle select and selectedPaths`() = runTest {
        val vm = MediaPickerViewModel(repoWith(MediaItem(1, "/a.jpg", "a.jpg", 1, 1)), ioDispatcher = testDispatcher)
        testDispatcher.scheduler.advanceUntilIdle()
        vm.toggleSelect(1)
        assertEquals(setOf(1L), vm.selected.value)
        assertEquals(listOf("/a.jpg"), vm.selectedPaths())
        vm.clearSelection()
        assertTrue(vm.selected.value.isEmpty())
    }

    @Test
    fun `refresh reloads from repo dropping deleted items`() = runTest {
        val fakeRepo = repoWith(
            MediaItem(1, "/a.jpg", "a.jpg", 1, 1),
            MediaItem(2, "/b.jpg", "b.jpg", 1, 1)
        )
        val viewModel = MediaPickerViewModel(fakeRepo, ioDispatcher = testDispatcher)
        testDispatcher.scheduler.advanceUntilIdle()
        viewModel.toggleSelect(1)
        fakeRepo.setItems(listOf(MediaItem(2, "/b.jpg", "b.jpg", 1, 1)))
        viewModel.refresh()
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(listOf(2L), viewModel.items.value.map { it.id })
        assertTrue(viewModel.selected.value.isEmpty())
    }
}
