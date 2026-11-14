# venus：HIDL Keymaster 4.1 解密链路进度

本轮在 ubuntu3 连接的真实源码和手机上续接，不是对外发布的完整 Recovery。没有发送刷写、切槽、格式化、清数据或重启命令。设备在一次只读 DE 测试途中发生了启动切换，原因未确定，详见下文。

## 已证实的关键进展

1. BinderFS/HwBinder 节点创建，原厂 QCOM Keymaster 4.1 服务注册，硬件信息查询成功：`Keymaster HAL: 4` / `QTI` / security_level=1。
2. 显式 HIDL 客户端已实现 getHardwareInfo、getKeyCharacteristics、begin/update/finish/abort、RAW 包装存储密钥转换，以及默认关闭的 RAM-only upgrade 选项。本轮没有启用 upgrade。
3. 已解决 metadata 解包的 `-38`：legacy `begin()` 的 purpose 已是独立参数，重复放入 KeyParameter 列表在这台 QTI HAL 上被拒绝。删除重复项后，真实现有密钥解包成功。保留原 AIDL 编码，不将厂商行为泛化到所有 KeyMint 实现。
4. 完整元数据测试成功返回：

```text
KEYMASTER_METADATA_UNWRAP_OK bytes=236
KEYMASTER_EPHEMERAL_OK bytes=68; material never logged or saved
KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED
```

236 字节是包装存储密钥 blob，68 字节是本次启动可用的硬件包装临时密钥；不是把硬件 AES 明文密钥导出了。转换采用 AOSP km_compat 对旧 Keymaster 的 RAW export 路径。原始 metadata key 文件只读，指纹比较在进程内完成，不保存密钥内容。

## 只读 Data / DE 试验：有中间成功，但没有完整通过

随后的一次 `--metadata --data-ro --de` 返回：

```text
KEYMASTER_DM_DEFAULT_KEY_PRESENT version=[2, 1, 0]
KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK
KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched
KEYMASTER_DEVICE_ENCRYPTED_KEY_OK identifier_matches=true
KEYMASTER_USER_DE_OK user=0 system_de_entries=10 shared_storage_unlocked=false
```

因此可以确认：这次运行确实建立了只读 dm-default-key 映射，读到了正确的 F2FS 超级块，成功只读挂载，并让设备级 fscrypt 与用户 0 的 DE 密钥通过校验。没有提交任何锁屏凭据，没有解锁 `/data/media/0` 的 CE 内容。

但紧接着 ADB 断开，终端没有返回 `KEYMASTER_DE_CHAIN_OK`、`KEYMASTER_DATA_RO_CLEANUP_OK` 或这一轮的末尾原密钥文件校验标记。虽然 adb 进程返回了 0，也不能据此算成功。恢复连接后为主系统，boot_id 从 `8097e00f-ab1a-4010-a971-572d43e0b1ea` 变为 `aede4ca8-75c1-43b0-82ac-13415743dcdd`。读取到的 boot reason 为 bootloader，旧 kmsg B 为空，pstore 为空，没有足够证据区分用户主动重启、系统重启或内核异常。

**不能把本轮表述为完整 DE 生命周期或 CE 解密完成。** 本助手没有发送重启命令。进入主系统后停止了设备侧测试，仅做只读日志查询和主系统拒绝路径测试。

为下一次定位已增加：
- DE 测试只处理 owner 0，不再隐式遍历其他用户。
- 各阶段写 kmsg 和 `/metadata/skyward-probe/keymaster41-last-phase.txt`，仅含 boot_id 和阶段名称，不含密钥/凭据。
- 主机重试入口要求末尾成功标记及前后相同 boot_id，不能只依赖 adb 退出码。
- 只读诊断固定使用 `/mnt/skyward-data-ro` 与 `skyward-km41-data-ro`，不遮盖 `/data`，不运行 fsck，不进行日志恢复。
- 最终 owner-0 限定和阶段持久化版本已编译、单元测试；尚未在重新进入的 Recovery 完整运行。

## 新产物与保留内容

```text
/home/abc/SKYWARD/skyward/target/venus-keymaster41.mdOGlf/boot.img
```

Boot SHA-256：`82328e342d81758d71393f845046f0ed6817a20da442782ee9c35a16bc795057`

Kernel SHA-256：`cb4030cc537f7082f6c791d567512acbb73bafa866f5b839979e60798eaea7b4`

Boot 大小 201326592 bytes，保留最新 DEBUG Kernel。原始 Boot、Vendor Boot、VBMeta 未改变。新 Boot 仍是**强制 Recovery 分流的临时镜像，禁止刷成日常 Boot**。

新 Boot 内保留基础菜单自动启动行为，加入新 CLI，但不自动启动 Keymaster 环境，不自动解密 Data。完整解密菜单尚未开放。常规 `KeyMint::open()` 仍走 AIDL，原 AIDL 实现除 Rust 类型重命名外完全一致。

`assets/` 保存客户端、两个静态辅助程序、从此机实际系统 APEX 提取的 runtime/VNDK30 payload 与服务配置。仅用于匹配这台已安装 ROM；脚本验证 APEX 和 payload 哈希，ROM 更换就拒绝继续，不混用其他版本库。

## 复现入口

构建（不接触手机）：

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-keymaster41.sh
```

在用户手动重新临时启动上述 Boot、ADB 处于 Recovery 后，逐级运行。不要在主系统执行测试；脚本会先拒绝，并已实际验证这种拒绝发生在任何文件推送之前。

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/prepare-keymaster41.sh --hardware
bash porting/venus/prepare-keymaster41.sh --metadata
```

只有上述成功并准备记录本轮完整日志后，继续单独的只读生命周期测试：

```bash
bash porting/venus/prepare-keymaster41.sh --data-ro
bash porting/venus/prepare-keymaster41.sh --de
```

这些调用不会猜密码、写 key upgrade、刷分区、切槽或格式化。服务本身会与 QSEE/安全存储正常通信；不把启动安全服务称为绝对无副作用。数据映射和挂载只读，Metadata 仍可能被既有日志服务与阶段记录写入。

`km41-runtime.sh` 的 private mount namespace/chroot 是运行库隔离，不是安全沙箱：其中保留 /dev 供 HAL 工作。仅在已验证的 Permissive 临时 Recovery 使用，不主动改变 SELinux。

同次 Recovery 可请求停止本工具拥有的 HAL 进程：

```bash
adb shell sh /tmp/skyward-km41-stage/km41-runtime.sh stop
```

此停止命令不是完整回滚：Binder 端点、只读挂载和暂时属性仍保留供检查，重启临时环境才清除；不会误杀未由本次配置创建的进程。标准系统分区镜像没有被改写。

## 验证记录

已运行通过：22 项 Binder 测试、49 项 crypto 测试、12 项 Skyward host 测试、24 项 packer 测试、332 项 UI 测试，共 439 项。新增包括嵌套缓冲区父对象验证、Binder 地址边界、HIDL purpose 回归、只读挂载参数、4 KiB 对齐等。

Host packer Clippy `-D warnings` 通过；Android ARM64 release 构建通过；两个 C helper 按 `-Wall -Wextra -Werror -static` 编译通过。Shell 语法检查通过。新镜像 Header、内核、CPIO 原条目元数据和 AVB 自校验通过；普通模式的 init/服务未被探测模式修改。

整合后的服务准备脚本经过语法/主系统拒绝检查，其完整冷启动复现仍待验收，不能因为先前手工分步启动成功就称整合脚本已实机验证。

主要日志：

```text
/home/abc/SKYWARD/venus_dump/keymaster41.sSBzw7/resume/metadata-1.txt
/home/abc/SKYWARD/venus_dump/keymaster41.sSBzw7/resume/data-ro-first.txt
/home/abc/SKYWARD/venus_dump/keymaster41.sSBzw7/resume/after-disconnect-state.txt
/home/abc/SKYWARD/venus_dump/keymaster41.sSBzw7/resume/release-build.log
```

修改前完整源码：`/home/abc/SKYWARD/skyward-before-keymaster41.AudctD/source`
本次续接前额外备份：`/home/abc/SKYWARD/skyward-keymaster41-resume.FXyx59`
新输出目录中的 `keymaster41-changes.patch` 为相对完整源码备份的增量，逆向应用只做检查、不自动回退工作区。

## 剩余链路

先复测只读映射/DE 的完整结束和清理，确认启动切换原因。然后接入并验证 Gatekeeper 1.0 注册、HMAC 共享初始化、正确凭据对应的 Synthetic Password / Keystore2 blob / CE fscrypt 路径；凭据应由用户在手机本地输入，不写命令行、日志或对话。当前没有将这些未验证流程接到“解密成功”状态，也没有声称内部存储已打开。

接口来源的固定副本位于 `venus_dump/keymaster41.sSBzw7/reference/`：AOSP Keymaster 4.0/4.1 HAL、VNDK30 生成的 types.h、km_compat.cpp 和 vold KeyStorage；sources.txt 记录来源地址。


## 2026-11-08：冷启动脚本复现修复（已完成 metadata 验收）

本次用户从新 Recovery 运行 `prepare-keymaster41.sh --metadata` 后，在 `awk: inaccessible or not found` 停止。

修复包括：

- `km41-runtime.sh` 的属性解析改为 Shell `read/case` 加 Recovery 已有 `printf`，不再依赖 awk；保持精确键匹配、首个匹配、CRLF/无末尾换行处理，不执行属性值。
- 添加需要的命令预检，显式使用已验证存在的 `toybox nohup`。
- 继续真机测试发现 `umask 077` 使 launcher 的 chroot 根目录成为 0700。QSEECom 降到 system 后不能访问路径，无法发布 listeners ready 属性。`km41-launch.c` 现通过 no-follow 目录 FD 设置 chroot 根和内部目录为 0755，外层 `/tmp/skyward-km41` 保持 0700；没有设置 0777、没有伪造 ready 属性。
- 源码与当前构建目录的 assets 副本及 ASSETS.sha256 同步更新；boot.img 的内容保持不变，不必重新启动或重打包 Boot。
- 新增 11 项属性解析测试和 5 项目录权限测试，均在 Ubuntu 与实际 Android Recovery 上运行通过；构建脚本也已加入回归测试。

完整入口真实输出：

```text
KEYMASTER41_RUNTIME_READY; metadata/DE tests require explicit commands
KEYMASTER_METADATA_UNWRAP_OK bytes=236
KEYMASTER_EPHEMERAL_OK bytes=68; material never logged or saved
KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED
VERIFIED --metadata; logs=/home/abc/SKYWARD/venus_dump/keymaster41-retry.I6RB3I
```

验证前后 boot_id 均为 `53a24e4f-b7aa-430a-9b71-894bfcd0d0dd`，ADB 和 venus_ui 均 running，QSEE listeners 和 hwservicemanager ready 均 true。原 `/data` 和诊断 `/mnt/skyward-data-ro` 均未挂载。本轮没有执行 --data-ro、--de 或任何凭据验证，没有启用密钥升级；日志仅记录非敏感阶段和长度。

原始 boot_a/vendor_boot_a/vbmeta_a 和本轮 boot.img 均通过原校验清单。修改前备份与本轮详细检查保存在 `/home/abc/SKYWARD/skyward-km41-shellfix.ue9YVa`。此前 DE 测试中断、CE 尚未完成的边界不变，不能将本次 metadata 通过当作完整用户存储解锁。

## Data 只读清理中断：诊断版更新（2026-11-08）

用户执行 `--data-ro` 后输出停在 `KEYMASTER_DATA_RO_MOUNT_OK`。
本轮从手机读取的持久化标记：

```
boot_id=53a24e4f-b7aa-430a-9b71-894bfcd0d0dd
phase=DATA_RO_CLEANUP_BEGIN
```

当前主系统启动ID为 `a4b32aa8-9517-4383-bd60-b9d620bc0c99`。
这证明测试期间发生了启动切换，且最后记录在清理入口。尚不能区分
`umount2`、Device Mapper remove 或其他并发事件，也未取得相应panic栈。
本轮只读取了主系统状态和日志，没有在主系统运行解密/卸载操作，没有
主动重启、刷写、切槽、改内核或修改用户密钥。

`/metadata/venus-kmsg-A.log`包含另一轮正常系统重启到bootloader记录，
B文件为当前主系统启动，不能把它们当作本次Recovery清理的崩溃证据。
只读获取的oops/mtd日志也没有对应本次阶段的崩溃栈。证据目录：
`/home/abc/SKYWARD/venus_dump/data-ro-exit.i9WYqp`。

### 本次代码改动（诊断增强，不代表重启根因已修复）

- 每个阶段打印到stdout并flush，同时保留kmsg和Metadata阶段记录。
- 清理分为 `DATA_RO_UMOUNT_BEGIN/OK` 和 `DATA_RO_DM_REMOVE_BEGIN/OK`。
- 卸载失败绝不移除仍在使用的映射；已失败的清理不在Drop中静默重试。
- 新增只读、限时/限量的km41-kmsg程序，在测试前直接向Ubuntu传输内核日志。
- 主机入口记录远端退出标记、ADB退出码、前后boot ID和最后阶段。缺任何必需
  证据均明确报错，并生成summary.json；不再只用grep后静默退出。
- 不使用懒卸载、强制卸载、fsck、可写Data挂载或任何清数据绕过。

### 新产物与使用

镜像：`/home/abc/SKYWARD/skyward/target/venus-keymaster41.Y97lDW/boot.img`

手机处于Bootloader Fastboot时可由用户临时启动此镜像，不要flash；本轮没有
启动它。进入Recovery后仍使用：

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/prepare-keymaster41.sh --data-ro
```

脚本将先部署最新assets并启动日志采集，日志目录在开始时打印。
本次只做一轮data-ro定位，暂不运行`--de`或CE。旧问题尚未消除，再次测试仍
可能重启。不要将本版解释为已修复的稳定解密版。

验收标记应完整包含UMOUNT_OK、DM_REMOVE_OK、CLEANUP_OK、原密钥未改、
REMOTE_EXIT=0、VERIFIED --data-ro，并且启动ID不变。
正常Android下脚本会在推送/运行前拒绝执行。

备份：`/home/abc/SKYWARD/skyward-before-cleanupdiag.6MsbPk`。
