package com.localtrans.app.media

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class MediaFilter { PHOTOS, VIDEOS }

class MediaPickerViewModel(
    private val repo: MediaRepo,
    private val ioDispatcher: kotlinx.coroutines.CoroutineDispatcher = Dispatchers.IO,
) : ViewModel() {
    private val _items = MutableStateFlow<List<MediaItem>>(emptyList())
    val items: StateFlow<List<MediaItem>> = _items.asStateFlow()

    private val _selected = MutableStateFlow<Set<Long>>(emptySet())
    val selected: StateFlow<Set<Long>> = _selected.asStateFlow()

    private val _filter = MutableStateFlow(MediaFilter.PHOTOS)
    val filter: StateFlow<MediaFilter> = _filter.asStateFlow()

    init { refresh() }

    fun setFilter(f: MediaFilter) { if (_filter.value != f) { _filter.value = f; refresh() } } // M-C8: 同值短路避免重组风暴

    fun refresh() {
        viewModelScope.launch {
            val list = withContext(ioDispatcher) {
                when (_filter.value) {
                    MediaFilter.PHOTOS -> repo.queryImages()
                    MediaFilter.VIDEOS -> repo.queryVideos()
                }
            }
            _items.value = list
            _selected.value = _selected.value.intersect(list.map { it.id }.toSet())
        }
    }

    fun toggleSelect(id: Long) {
        _selected.update { if (id in it) it - id else it + id }
    }

    fun clearSelection() { _selected.value = emptySet() }

    /** 单文件直发用:清空现有选中后仅选中 path 对应的 id */
    fun selectOnly(path: String) {
        val id = _items.value.find { it.path == path }?.id
        if (id != null) {
            _selected.value = setOf(id)
        } else {
            _selected.value = emptySet()
        }
    }

    fun selectedPaths(): List<String> =
        _items.value.filter { it.id in _selected.value }.map { it.path }
}

/**
 * Factory for creating MediaPickerViewModel with MediaStoreRepo
 */
class MediaPickerViewModelFactory(
    private val repo: MediaRepo
) : androidx.lifecycle.ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : androidx.lifecycle.ViewModel> create(modelClass: Class<T>): T {
        if (modelClass.isAssignableFrom(MediaPickerViewModel::class.java)) {
            return MediaPickerViewModel(repo) as T
        }
        throw IllegalArgumentException("Unknown ViewModel class")
    }
}
