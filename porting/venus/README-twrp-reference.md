# venus：参考 TWRP 的解密生命周期调整

## 状态与边界

本轮直接在 `/home/abc/SKYWARD/skyward` 修改源码。参照的是实际克隆、固定提交的 TWRP 源码，不是网上刷机教程；没有替换 DEBUG Kernel、TWRP 的预编译内核、厂商固件或用户密钥。没有给手机发送重启、切槽或刷写命令。

**这不是已验证的内核卸载修复。** 本轮把非必要的“挂载后立刻卸载”的探测流程，改为 TWRP 那样保留已解密设备/挂载，供下一步 DE 使用。本实现仍使用只读 Device Mapper 和只读 F2FS，且不接管 `/data`，不同于 TWRP 的一般读写运行路径。CE、锁屏密码验证、Data 读写及日常刷入都未开放或未验收。

## 对照来源

完整提交和本地文件 SHA-256 存在 `twrp-reference-lock.json`；本机参考目录由 `target/venus-twrp-reference-latest.txt` 指定。

| 仓库 | 分支 | 固定提交 | 对照用途 |
| --- | --- | --- | --- |
| https://github.com/TeamWin/android_device_xiaomi_venus | android-11 | 30f2f2cd4d187dab64cc89fa98bc25fd18de05fa | 官方 venus 布局、公共树依赖 |
| https://github.com/TeamWin/android_device_xiaomi_sm8350-common | android-11 | 696cf9fdf563f83830e99a04dedb9e61d9746546 | 早期 FBE/fstab 配置 |
| https://github.com/sekaiacg/android_device_xiaomi_venus_TWRP | android-13 | 0fcc985756e69b68c8d5eef988ca22b1ee1a3186 | venus QSEE/Keymaster 4.1 启动时序、Metadata、wrappedkey_v0、fscrypt v2 |
| https://github.com/TeamWin/android_bootable_recovery | android-12.1 | 5c3d206a5eeb3d446bcda8248a405a4b278bab5c | 分区持有与 DE 后续流程 |
| https://github.com/TeamWin/android_system_vold | android-12.1 | a164ba05c5fef288059774a776b2e6e1119957cf | 元数据映射与临时硬件包装存储密钥转换 |

这些源码跨不同年代和分支，并不是宣称它们共同组成一套已经支持当前 Android 17 移植系统的完整发行版。设备专有服务是预编译二进制；设备树不提供 QTI TEE 实现源码。

## 实际差异与采用的调整

### 1. QSEE 就绪再启动 Keymaster

`venus-sekai/recovery/root/init.recovery.qcom.rc` 在 `vendor.sys.listeners.registered=true` 时才启动 Keymaster 4.1 和 Gatekeeper 1.0。原 Skyward 辅助脚本只在 QSEE 启动后睡眠一秒。

现在 `km41-runtime.sh` 等待自己拥有的 hwservicemanager / QSEE 进程存活且真实就绪属性出现，超时或进程退出则失败。启动前只把遗留就绪状态清为 false；true 必须由真实服务设置。重复使用环境也检查两个依赖仍然存活/就绪。不伪造 `listeners.registered=true`。Gatekeeper 的实际凭据流程本轮没有启用。

### 2. 保留 Data，而非解密后立即卸载

`recovery-core/partitionmanager.cpp` 的 `TWPartitionManager::Decrypt_Data()`（586 行起）先调用 `fscrypt_mount_metadata_encrypted()`，保留解密设备，挂载并继续 `Decrypt_FBE_DE()`。`vold/MetadataCrypt.cpp` 也没有在成功解密后立即销毁这个设备。

现在新增 `venus_data_session.rs`，在 `/tmp/skyward-km41-data-session.txt` 保存仅含 boot ID、源设备/映射设备编号、mount ID 的 root 私有记录。不保存密钥和凭据。下次仅在同一次启动、设备和 mount ID 不变、F2FS 和 DM 都仍只读时才复用。未知挂载、过期记录、非只读状态均拒绝，绝不覆盖/移除未知资源。

成功挂载即先标记保留；后面的验证发生错误也不经 Drop 自动进入卸载路径。只有验证全部通过才输出 SESSION_READY/RETAINED 成功标记。DE 保留模式要求已经存在一份验证过的会话，不会直接从未挂载状态跳到 DE。

### 3. 不把 TWRP 当成卸载崩溃的证明

`recovery-core/partition.cpp` 的 `TWPartition::UnMount()`（1737 行起）也调用 `umount2()`，并没有一个可直接套入 Skyward 的 venus 专用卸载补丁。TWRP 使用自己的预编译内核，不能据此证明当前 DEBUG Kernel 的 F2FS 卸载安全。

最新 `keymaster41-retry.b2IIJT/result.txt` 在 `DATA_RO_MOUNTED` 截断，但同 boot ID 的 `inspection/phase-and-state.txt` 保存了 `DATA_RO_UMOUNT_BEGIN`。这是“到达卸载调用前”的证据，仍不是具体内核 panic 栈。新逻辑避免在解密准备成功后无意义地走这条路径，不声称消除了内核问题。

### 4. 保留已经验证的加密格式，不照抄危险配置

venus TWRP 的 fstab 与此手机均使用 `metadata_encryption=aes-256-xts:wrappedkey_v0` 和 fscrypt v2 的 `wrappedkey_v0`。不改变已成功使用的 Keymaster 4.1 密钥格式。没有照抄 `formattable`、文件系统修复、读写挂载、格式化、2099-12-31 补丁级别或自动密钥升级；保持读取实际安装系统版本。

## 可用入口

新的 Boot 仍自动启动原基础菜单，解密实验只通过显式命令执行。手机处于正常 Android 时，脚本在推送文件/启动 HAL 前拒绝执行。

启动新的临时 Recovery 后，可执行：

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/prepare-keymaster41.sh --data-ro-keep
```

只有本轮返回 `VERIFIED --data-ro-keep` 且手机仍在同一次 Recovery，才表示保留会话通过，**不代表卸载或 CE 通过**。只读路径是 `/mnt/skyward-data-ro`；不要手动 `umount` 或移除 DM。

后续显式的用户 0 DE 入口为 `--de-keep`；它要求上一步的有效会话，不读取/猜测锁屏密码。第一轮保留模式尚未验证前不要自动串联执行。

旧 host 参数 `--data-ro`、`--de` 已停用，避免重复触发自动卸载。底层高风险 teardown 入口仍保留显式授权门槛，但不作为本次建议步骤。

输出 `summary.json` 单独记录 `lifecycle=retained`、`cleanup_verified=false`、`ce_verified=false`。出现卸载/移除映射标记会使保留模式验收失败。

## 回退与重建

修改前完整源码备份路径记录在 `target/venus-twrp-backup-latest.txt`。本次构建输出带增量补丁和校验值；回退应先 `git apply --reverse --check`，有后续修改时不要强行覆盖。

```bash
bash porting/venus/build-keymaster41.sh
```

构建不操作手机，只生成新目录。`target/venus-keymaster41-latest.txt` 仅在所有离线测试和镜像校验成功后更新。普通打包模式隔离仍由原验证器检查。新镜像保持原 DEBUG Kernel 逐字节一致。
