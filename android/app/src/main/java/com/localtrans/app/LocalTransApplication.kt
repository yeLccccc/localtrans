package com.localtrans.app

import android.app.Application
import coil.ImageLoader
import coil.ImageLoaderFactory
import coil.decode.VideoFrameDecoder

/**
 * 全局 Application:注册带 VideoFrameDecoder 的 Coil ImageLoader。
 * 视频页缩略图(MediaGridTab)依赖它取视频首帧——Coil 默认只能解图片,
 * 不注册则视频格子全部空白(v0.9.2 实测)。
 */
class LocalTransApplication : Application(), ImageLoaderFactory {

    override fun newImageLoader(): ImageLoader =
        ImageLoader.Builder(this)
            .components {
                add(VideoFrameDecoder.Factory())
            }
            .crossfade(true)
            .build()
}
