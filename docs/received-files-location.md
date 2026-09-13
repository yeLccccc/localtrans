# 接收文件存储位置与可见性

> v0.11.0 审计低危批补充说明。后续打包时请把本节要点并入分发侧 usage/README。

## Android

接收的文件默认落在**公共下载目录**:

```
/sdcard/Download/LocalTrans/
```

- 公共目录:其他 App(文件管理器、相册、微信等)可以直接读取,不需要 root。
- 图片/视频类文件在落盘后由 MediaScanner 扫描,系统相册可见。
- 可在"设置"里更换接收目录(`set_inbox_dir`);更换后新传输落新目录。
- 若注入公共目录失败(权限被拒等),自动回落到应用私有目录
  (`/data/data/com.localtrans.app/files/localtrans/`),此时仅本 App 可见。

## PC(Windows / Linux)

接收的文件落在程序数据目录:

```
<data>/            # Windows 通常是 %APPDATA%/com.localtrans.app/
├── inbox/         # 默认接收目录(随安装位置)
└── transfers.json # 传输任务历史
```

- 接收目录可在设置中修改;修改对**新开始**的传输生效,进行中的任务仍落原目录。
- `data/.localtrans-parts/<job_id>/` 为断点续传的分片临时区,任务完成后清理;
  非正常退出遗留的分片会在下次启动时 GC(超 7 天或位图全真未 finalize)。

## 与其他应用的交互

- Android 端发送走 SAF/MediaStore 选取,不复制文件、只读原路径。
- PC 端"打开所在文件夹"使用系统 opener 直接定位到已落盘文件。
