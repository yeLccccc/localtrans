# PC ↔ Android 全功能覆盖测试清单

> 2026-09-09。逐项真机测试，每项标注 PASS/FAIL/SKIP + 证据。
> 环境：huss_pc (test-api 39872) + huss_phone (adb f9f8b0e, 已配对) + huss_laptop (可选中转对照)。

## 一、发现与连接
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 1.1 | PC 发现 Android(UDP) | PC list_devices 含手机 | |
| 1.2 | Android 发现 PC | 手机设备页 dump 有 huss_pc | |
| 1.3 | PC→Android 连接建立 | invoke connect → sessions trusted | |
| 1.4 | Android→PC 连接建立 | 手机点 PC 卡 → 已连接 | |
| 1.5 | 断线后自动重连(连接记忆制) | kill app → relaunch → 自动重连 | |
| 1.6 | 隐身模式(开启后 PC 看不到手机) | 手机开隐身 → PC devices 不见手机 | |
| 1.7 | 隐身关闭恢复互见 | 手机关隐身 → PC 恢复发现 | |

## 二、配对与信任
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 2.1 | 手机发起配对(全新) → PC 同意门 → 输码 → trusted | factoryReset + UI 全链 | |
| 2.2 | PC 发起配对 → 手机同意门(auto-consent) → PC 亮码 → 手机输码 | factoryReset + UI | |
| 2.3 | 错码第 1 次 → 提示"还可重试" | 输错误码 → 断言文案 | |
| 2.4 | 错码第 3 次 → 冷却 5 分钟 | 连错 3 次 → 断言冷却文案 | |
| 2.5 | 对端拒绝 → toast + 断连 | 手机拒绝 → PC toast | |
| 2.6 | 移除信任 → 对端断连 + 降级 | PC 移除手机 → 手机断连 | |
| 2.7 | 重复配对(已 trusted) → 直连不弹门 | 已配对设备点连接 → 直连 | |

## 三、推送(PC → Android)
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 3.1 | 推送单文件(1MB) → OfferSheet → 接收 → done | push_files + UI 点接收 | |
| 3.2 | 推送中文件(16MB) → 接收 → done + 字节核对 | push_files + UI | |
| 3.3 | 推送拒绝 → PC 卡 failed"对方拒绝" | 点拒绝 → PC 终态 | |
| 3.4 | 推送超时(无人接收) → PC 卡 failed"超时" | 不点弹窗 → PC 60s 超时 | |
| 3.5 | 推送完成后手机传输页可见 done 卡 | nav-transfers dump | |
| 3.6 | 推送完成通知+深链跳转 | dumpsys 通知 + 点击切页 | |
| 3.7 | 文件落盘位置正确 | run-as ls downloads | |

## 四、拉取(Android → PC)
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 4.1 | 手机浏览 PC 共享区 → 列出文件 | files-remote-tab → 选 PC → dump | |
| 4.2 | 单文件下载 → PC source-pull 卡 done | 长按文件 → 下载到本机 | |
| 4.3 | 多文件多选下载 → 聚合单卡 done | 长按→选择多项→全选→下载 | |
| 4.4 | 文件落盘手机本地可查 | run-as ls downloads | |

## 五、传输管理
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 5.1 | 传输卡进度推进(Rust 4Hz 泵) | 推大文件 → state 采样 | |
| 5.2 | 速度显示合理(平滑无尖刺) | speed-curve 场景 | |
| 5.3 | 暂停 → 进度停滞 → 恢复 → 完成 | transfer_action pause/resume | |
| 5.4 | 取消 → 卡终态 | transfer_action cancel | |
| 5.5 | 推送失败重试 | 失败卡 retry 按钮 | |
| 5.6 | 清除已完成 → 视图清空 | clear_completed | |
| 5.7 | 单进度条(R1) | 截图确认无第二条进度条 | |
| 5.8 | 排队位次显示(并发满时) | 推 4 个任务 → 第 4 个显示位次 | |

## 六、设备管理
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 6.1 | 设备卡状态徽章(在线/已连接/待配对) | dump 对照 | |
| 6.2 | 通道标签(直连 · Nms) | dump 对照(M3c T1) | |
| 6.3 | 设备名显示(别名>广播名) | 改别名 → dump | |
| 6.4 | 推送进行中角标 | 推大文件 → dump 卡片 | |
| 6.5 | 移除信任菜单可用 | 点移除 → 确认 | |

## 七、设置
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 7.1 | Android 设备名修改 → 即时广播 | settings-input → save → PC 发现 | |
| 7.2 | PC 设备名修改 → Android 发现表更新 | set_settings → dump | |
| 7.3 | 推送策略查看(Ask) | 信任列表 → dump | |
| 7.4 | Android 接收目录正确 | FilesSaved 事件路径断言 | |

## 八、通知与系统集成
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 8.1 | 推送完成 → 系统通知 | dumpsys 通知 | |
| 8.2 | 通知深链点击 → 切传输页 | 点击通知 → dump 当前页 | |
| 8.3 | MediaScanner 扫描(图片/视频) | 推图片 → MediaStore 查询 | |
| 8.4 | FilesSaved 事件 → 事件桥 | logcat LocalTransBridge | |

## 九、稳定性
| # | 测试项 | 方法 | 结果 |
|---|---|---|---|
| 9.1 | 连续推送×5 → 全部 done | 循环推 | |
| 9.2 | 推送中途断网 → interrupted → 恢复 → 续传 | WiFi toggle | |
| 9.3 | App 杀进程重启 → transfers.json 恢复 | forceStop → launch → dump | |
| 9.4 | 并发推送×2 → 都 done | 两个 push_files 同时 | |

## 十、已知限制(不测)
- 相册备份(v0.8 已下线)
- 远程文件操作 UI(PC 砍掉定案)
- 中继路径(需公网 relay)
