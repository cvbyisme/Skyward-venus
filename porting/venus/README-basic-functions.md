# Venus 基础功能版 v1

本轮建立在用户已确认自动启动菜单、显示、触摸、滑动均正常的版本之上。它仍是强制进入 Recovery 的临时 Boot，**不得当作日常 Boot 刷入**。没有执行刷写、清数据、切槽或手机重启。

## 已接入的实际功能

- 挂载页：列出当前槽位真实的逻辑分区，在 `/mnt/skyward-ro/<分区名>` 独立挂载/卸载，只读 DM 映射，不遮盖 Recovery 自己的 `/system`、`/vendor` 等目录。只接受当前 super 中存在且列入白名单的系统逻辑分区。OTA 快照状态必须明确为 None。
- 文件管理：真实列出目录、文件信息，查看普通文本；不编辑、删除、复制或移动用户文件，特殊设备和内核接口不作为文本打开。
- 文件系统页：显示 userdata/metadata 的真实大小及可识别的文件系统状态。未解密、未挂载的 Data 不伪装成已打开；不提供格式化操作。
- 设置：语言、主题、亮度、熄屏时间等存到 `/metadata/skyward/venus-settings.json`，0600 权限、原子替换、同步；不改变 Metadata 已有挂载状态。Metadata 不正确或不可写时显示错误，不自动格式化或重挂载。
- 时钟：通过只读的 persist/time/ats_2 与 RTC 计算界面时间。不改 RTC，也不更改系统全局时钟，内核日志时间基准不变。
- 电池/设备状态：每五秒重新读取，而不是停留在启动时的快照。
- 截图：真实 Canvas 保存为 PNG 到 `/tmp/skyward-basic/screen-*.png`；首帧自动保存一张用于核验，菜单截图操作也接到此入口。
- 日志：导出 UI/探测/内核日志与挂载表到 `/tmp/skyward-basic/logs-*`，单个文本日志限长。RAM 文件重启会丢失，请通过 ADB 拉取。
- 用户确认的重启系统、Bootloader、关机操作接到 init 的 sys.powerctl。**代码已接通，但本轮未执行这三项操作，未声称已实机验收。** Fastbootd、再次重启 Recovery、EDL 暂不开放，避免对临时启动镜像作错误承诺。

设置保存会正常写入 Metadata，现有 DEBUG 日志也继续写该分区；这不等于整个 Recovery 都是只读。明确未写入/解密/清除的是用户 Data，未刷写的是手机启动与系统分区。

## 实机验证

日志目录：`/home/abc/SKYWARD/venus_dump/base-functions.GVQavT`

1. `mi_ext`、`odm`、`product`、`system`、`system_ext`、`vendor` 均完成只读映射、挂载、读取根目录与卸载。`dm::readonly` 均为 true；CLI 自测结束后挂载表恢复。
2. 修复了上游文件系统探测的启动依赖问题：上游调用 System 内的工具，而此时 System 尚未挂载。venus 基础入口直接读取 EROFS/F2FS/ext 超级块标识，完整验证仍由内核挂载执行；不修改上游生产路径。
3. ext4 使用 ro,noload；f2fs 使用 ro,norecovery，DM 设备也以只读建立。未知格式不尝试修复。
4. 设置自测写入、读回、比对并删除专用自测文件，未覆盖已有设置。基础 UI 随后实际保存了自己的 502 字节 JSON，已拉回并解析。
5. RTC/persist 界面时间读取成功；截图为实际菜单的 1440×3200 PNG，并已检查预览。
6. 新程序从 RAM 启动，日志有 `VENUS_BASIC_READY`、`UI_PROBE_FIRST_FRAME_SETTLED`、`VENUS_BASIC_SETTINGS_SAVED`。用户随后已有真实页面触摸记录。
7. ADB 一直 running，boot_id 未变化。当前改为 RAM 中的基础 UI 进程，旧 `venus_ui` 服务停止以避免争用显示；独立 ADB 和 DEBUG 日志未停止。

已验证配置落盘和解析，尚未做这份新 Boot 的重新启动后设置恢复测试。文本浏览、部分菜单交互仍需继续实际使用覆盖；不把 CLI 自测等同于全部 UI 路径验收。

## 解密审计结果（不是已修复）

实际读取的 Vendor 服务：

- `android.hardware.keymaster@4.1-service-qti`：HIDL Keymaster 4.1。
- `android.hardware.gatekeeper@1.0-service-qti`：HIDL Gatekeeper 1.0。
- `vendor.boot-hal-aidl`：用户现有 `android.hardware.boot-service.venus`。
- QSEE 服务及 keymasterd 也在 Vendor 中。

当前 Skyward `skyward-crypto/src/keymint.rs` 请求的是 AIDL `android.hardware.security.keymint.IKeyMintDevice/default`。Gatekeeper 模块已有 AIDL/HIDL 分支，但不能据此认为 Keymaster 4.1 已经适配。Recovery 当前没有可用的 `/dev/binder`、`/dev/hwbinder` 等节点，`wait_for_keymaster` 在重启等待。内核配置含 Binder IPC、BinderFS、binder/hwbinder/vndbinder、DM_DEFAULT_KEY 及 FS_ENCRYPTION。

Vendor fstab 的 Data 为 F2FS，使用 `aes-256-xts:aes-256-cts:v2+inlinecrypt_optimized+wrappedkey_v0`，Metadata encryption 为 `aes-256-xts:wrappedkey_v0`。这只是配置读取，不证明实际 Data 已解密。未读取或导出密钥文件内容、未尝试密码、未升级密钥、未格式化 Data。

接下来需要单独完成 Binder/硬件服务环境、Keymaster4.1 到现有密钥调用的适配、Vendor 运行库及加密参数匹配，才能开始解密验证。安装、恢复、清除、切槽、MTP、终端、Wi-Fi、振动尚未开放；这些项目不能通过删除测试拦截代码直接宣称可用。

## 源码与构建

新增核心模块：`crates/skyward/src/venus_basic.rs`；现有 `ui-probe` 通过显式 `--basic-features` 接入，未加参数时仍为原来的隔离 UI。`ui::run` 的生产流程没有被调用。

打包使用显式 `--venus-probe --venus-ui --venus-basic`，在原启动脚本中追加基础功能开关及对应标记。不带基础开关的 UI 镜像、普通镜像都做了隔离验证。

构建：

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-basic-functions.sh
```

本次输出：

```
/home/abc/SKYWARD/skyward/target/venus-basic-functions.Km9dfM/boot.img
```

SHA-256：`7b242871dbd45492f54be3c0dc4eecb1538376b7b870c070c4868aba8c4a27da`

Kernel SHA-256：`cb4030cc537f7082f6c791d567512acbb73bafa866f5b839979e60798eaea7b4`，与本次最新 DEBUG 底包逐字节一致。

8 项基础模块测试、24 项打包器测试、332 项 UI 测试通过。打包器 Clippy -D warnings、ARM64 Release 编译、AVB 自校验、CPIO 元数据与头部验证通过。新镜像尚未冷启动验证，本轮验证的是其中同版程序在当前 Recovery 中实际运行。

备份：`/home/abc/SKYWARD/skyward-before-base-functions.GdqlbK`

本轮增量补丁与构建输入快照、完整验证日志放在上述新输出目录。旧可用 `venus-ui-autostart.eiQASW/boot.img` 及用户源 Boot/Vendor Boot/VBMeta 校验值未变。

## 当前会话再次启动或回退

当前 Recovery 已有新版基础菜单，不需要现在再次启动。进程退出后可用：

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/run-basic-functions.sh
```

这个脚本部署到 `/tmp`，只替换菜单进程，不刷写或重启手机。重复执行时会拒绝已有进程，不覆盖运行中的 ELF。

回退当前菜单到旧服务（不回退源码，不删除设置）：

```bash
adb -s 535e451a shell '
p=$(cat /tmp/skyward-basic/pid)
case "$p" in ""|*[!0-9]*) exit 1;; esac
if [ -r /proc/$p/cmdline ] && grep -q skyward-basic-v1 /proc/$p/cmdline; then
    kill -TERM "$p"
fi
'
# 确认旧进程已退出后：
adb -s 535e451a shell 'setprop ctl.start venus_ui'
```

新的临时 Boot 需要用户自行决定何时测试，使用 fastboot boot，不使用 flash。本轮没有替用户执行重启。
