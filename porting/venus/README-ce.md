# venus：Gatekeeper 1.0 与只读 CE 解锁入口

## CE 本地输入通道修复（2026-11-08）

此前 `ce-local.py` 错把 `adb exec-out` 用于双向凭据输入。在本机 ADB 28.0.2 与当前 Recovery 上，实测旧 `exec:` 通道的远端标准输入仍是 PTY；手机程序因此在 `read_credential()` 的 `isatty(0)` 保护处退出，未调用 Gatekeeper 凭据验证。主机旧版本的 `exec-out` 也只接收输出，不适合此处的标准输入传输。

现在检查与解锁统一使用 `adb shell -T -e none` 并要求 `shell_v2`。手机端终端检查保持原样，未删除、绕过认证或修改加密代码。所有无输入命令显式获得 EOF，不再继承用户本地终端。

在显示 PIN 提示前，脚本先用公开的非凭据测试数据检查远端非 TTY、二进制数据完整性、EOF 和远端退出码。成功输出：

```text
VENUS_CE_TRANSPORT_OK shell_v2=true stdin_tty=false binary_stdin=true eof=true exit_status=true
```

自检失败则在读取 PIN 之前停止，没有旧通道回退、没有自动认证重试。修复仅涉及 Ubuntu 脚本、回归测试和本文档，不需要重新打包或启动 Boot。实际 CE 解锁仍须用户在本地输入当前凭据后验证；通道测试通过不代表 CE 已解锁。

相关 AOSP 实现参考：`platform/system/core` 的 `adb/commandline.cpp`（`exec-in`/`exec-out` 单向转发）和 `adb/shell_service.cpp`（无 shell 协议的 raw 子进程采用 raw PTY）；`platform/packages/modules/adb/docs/user/adb.1.md` 的 `-T`/`-e`/`features` 说明。

## 本轮范围

在已验证的 Metadata 解密、保留式只读 Data 挂载和用户 0 DE 链路上，增加原厂 Gatekeeper HAL 运行环境、CE 预检查，以及用户在 Ubuntu 本地输入凭据的显式解锁入口。

**真机已经通过：Gatekeeper 服务注册/接口查询、当前 owner0 保护器和格式检查、源文件未改变检查、同一启动/挂载身份检查。尚未提交锁屏凭据，CE 解锁最终结果未验证。** 新 Boot 的离线校验不等于已经验证其冷启动。

这不是日常可刷 Boot。保留原 DEBUG Kernel、此前的临时 Recovery 入口补丁和基础菜单。不要 fastboot flash。没有修改 Vendor Boot、VBMeta 或原始 boot_a.img。

## 当前 Recovery 中使用

保留已通过的只读 Data/DE 会话，不需要重启，不要手动 umount。

只检查、不提交凭据：

```bash
cd /home/abc/SKYWARD/skyward
python3 porting/venus/ce-local.py --inspect
```

由设备所有者在自己的 Ubuntu 交互式终端执行一次解锁：

```bash
cd /home/abc/SKYWARD/skyward
python3 porting/venus/ce-local.py --unlock
```

脚本先部署校验过的程序并重新检查运行环境；然后要求输入 `UNLOCK` 确认，再无回显地输入当前锁屏 PIN。不要将 PIN 写在命令后面，不要发到聊天，不要启用外部键盘/终端输入录制。

凭据不作为进程参数、环境变量或脚本日志保存，仅通过 adb 标准输入传输，格式为 4 字节小端长度与凭据字节。程序不猜测、不遍历保护器、不自动重试，也不调用 enrollment/deletion。错误凭据可能触发硬件失败计数/限速；超时或 USB 中断时结果可能未知，先检查状态，不要反复运行。

成功的必要输出包括：

```text
VENUS_CE_SOURCE_FILES_UNCHANGED
KEYMASTER_PHASE CE_UNLOCK_COMPLETE
VENUS_CE_UNLOCK_OK user=0 shared_storage_unlocked=true readonly=true retained=true teardown=not_attempted
VENUS_CE_REMOTE_EXIT=0
VERIFIED CE_UNLOCK
```

只有预检查的 `VERIFIED CE_PREFLIGHT` **不表示解锁完成**。确认解锁后，内部共享存储位于 `/mnt/skyward-data-ro/media/0`，依然只读；当前入口不自动挂载 `/data` 或 `/sdcard`，不启用 MTP、安装和清除操作。

若已重新启动到一个全新的临时 Recovery，需要先建立 HAL 与保留式 Data/DE 会话：

```bash
bash porting/venus/prepare-keymaster41.sh --data-ro-keep
# 必须先确认上一条 VERIFIED，再执行下一条。
bash porting/venus/prepare-keymaster41.sh --de-keep
# 必须先确认 DE VERIFIED，再运行 ce-local.py。
```

旧的 `--data-ro` / `--de` 自动卸载入口保持禁用。

## 设计与实现

- `crates/skyward-crypto/src/gatekeeper.rs`：显式 HIDL 1.0 查询；有界回复读取；精确 token 长度；无符号重试毫秒数；错误不重试。
- `crates/skyward-crypto/src/ce_store.rs`：严格选择 owner0 当前 sp-handle；只接受已知 SP v3、Gatekeeper/软件 CE 包装；SQLite DB+WAL 在手机 tmpfs 的私有目录中查询副本，不使用会漏 WAL 的 immutable 视图。
- Keystore 选择限定 SELINUX domain=2、Locksettings namespace=103、live CLIENT key，拒绝同名歧义；不把应用命名空间的 key 当作系统保护器。
- `crates/skyward/src/venus_ce.rs`：复用并核验保留会话、独占锁、只读映射/挂载、DE 状态、原始文件哈希；解锁仅一次 Gatekeeper LSKF verify，后接 HAT、SP 双层解包、CE 包装解开、临时包装密钥与 fscrypt v2 安装验证。
- 原 QTI HAL 在对当前认证绑定保护器调用 getKeyCharacteristics 时返回 -26。依据 AOSP，此码是 KEY_USER_NOT_AUTHENTICATED。代码只在磁盘缓存的 SID 与当前 Gatekeeper handle 匹配时将其记为“等待认证”，并且后续真实硬件解密仍必须接受正确的 HAT；没有绕过认证，也没有假称该 key 已经能使用。
- 认证成功至 Keymaster begin 的短路径不进行 Metadata fsync，避免不必要地消耗认证 token 的有效时间。
- `token.rs` 的 Debug 不显示敏感字段；Token 和 HIDL Message 的临时缓冲区退出时清理。Python/库内部仍可能存在临时拷贝，这不是绝对内存擦除保证。
- `porting/venus/ce-runtime.sh`：仅在原有 QSEE/HwServiceManager/Keymaster 就绪后，启动原厂 Gatekeeper HAL；不启动 gatekeeperd，不修改凭据数据库。
- `porting/venus/ce-local.py`：仅交互终端允许 --unlock，确认提示与隐藏输入、stdin framing、明确成功标记、同一 boot ID 与挂载身份校验。

## 明确未验收的部分

- 用户真实 PIN/密码验证、硬件 SP 解包和 CE 最终解锁结果。
- CE 图形界面整合、Weaver、图案输入、其他用户/工作资料/应用分身。
- 可写 Data、格式化/刷包/密钥升级或修改，均不在本轮范围。
- 之前卸载 F2FS 时重启的根因仍未修复。本入口不调用卸载，不得把保留会话成功当作清理通过。
- 当前私有 HAL 运行环境仅在已有 permissive 调试 Recovery 验证，不代表 enforcing 正式版本已适配。

## 构建与回归

```bash
bash porting/venus/build-ce.sh
```

构建只在 Ubuntu 上进行，不访问或重启手机。输出目录和校验清单由构建脚本打印，`target/venus-ce-latest.txt` 仅在全部离线检查成功后更新。

本地输入测试使用虚构凭据及 PTY，不连接手机：

```bash
python3 porting/venus/test-ce-local.py
cargo test --locked -p skyward-binder -p skyward-crypto --lib
```

## 参考来源与本轮证据

保留此前 `twrp-reference-lock.json` 固定的 venus TWRP 设备树及 TeamWin Vold 源码。新增核对 AOSP 的 SyntheticPasswordManager.java、SyntheticPasswordCrypto.java 和 Gatekeeper types.hal；快照与 SHA-256 位于本轮证据目录的 `aosp/`。这些参考不是当前 Android 17 移植 ROM 已获上游验证的声明。

本轮源码备份：`/home/abc/SKYWARD/skyward-before-ce-9_qmm3fv`。

本轮证据目录：`/home/abc/SKYWARD/venus_dump/ce-bringup.cyfm_ojg`。

当前可用原镜像保持不变：`target/venus-twrp-lifecycle.81LHA1/boot.img`。
