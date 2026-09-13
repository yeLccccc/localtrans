package com.localtrans.app.ui.files

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Send
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.localtrans.app.util.Formatters

fun sendBarSummary(count: Int, bytes: Long): String =
    "已选 $count 项 · ${Formatters.formatFileSize(bytes.toULong())}"

