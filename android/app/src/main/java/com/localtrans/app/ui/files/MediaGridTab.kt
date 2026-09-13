package com.localtrans.app.ui.files

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.localtrans.app.media.MediaItem
import com.localtrans.app.util.Formatters

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun MediaGridTab(
    items: List<MediaItem>,
    selected: Set<Long>,
    onToggle: (Long) -> Unit,
    onLongClick: (path: String) -> Unit,
    modifier: Modifier = Modifier
) {
    LazyVerticalGrid(
        columns = GridCells.Fixed(3),
        modifier = modifier.fillMaxSize().padding(2.dp),
    ) {
        items(items, key = { it.id }) { item ->
            Box(Modifier.padding(2.dp)) {
                AsyncImage(
                    model = java.io.File(item.path),
                    contentDescription = item.name,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f)
                        .combinedClickable(
                            onClick = { onToggle(item.id) },
                            onLongClick = { onLongClick(item.path) }
                        )
                )
                if (item.id in selected) {
                    Icon(
                        Icons.Default.CheckCircle, null,
                        modifier = Modifier.align(Alignment.TopEnd).padding(4.dp).size(24.dp),
                        tint = MaterialTheme.colorScheme.primary
                    )
                }
                if (item.durationMs > 0) {
                    Text(
                        Formatters.formatDurationMs(item.durationMs),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurface,
                        modifier = Modifier.align(Alignment.BottomEnd).padding(4.dp)
                    )
                }
            }
        }
    }
}
