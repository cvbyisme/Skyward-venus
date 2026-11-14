# Venus 自动菜单测试 Boot v1

## 本轮确认与范围

用户已经实机确认上一轮 `ui-probe` 的「显示触摸滑动都正常」。这一结论针对通过 ADB 在 RAM 中启动的菜单，不是本轮新镜像自动启动的验收。

本轮把相同的隔离菜单入口集成进临时 Boot。仅当打包命令同时指定 `--partition boot --venus-probe --venus-ui` 才启用。普通打包与原纯探测模式不自动启动菜单。

本轮没有刷写、切槽、重启手机，也没有停止正在运行的用户菜单测试。新 Android 程序另复制到 `/tmp/skyward-autostart-cli-check`，仅运行帮助与拒绝路径检查。前后手机 boot_id、ADB 状态及挂载列表相同。

## 输出与备份

- 新镜像：`/home/abc/SKYWARD/skyward/target/venus-ui-autostart.eiQASW/boot.img`
- 镜像大小：201326592 bytes（192 MiB）。
- 镜像 SHA-256：`f2e72fad90667695cc8c65dcf6f8773c05db0e6654ee92f451e55206679a8e28`
- DEBUG Kernel 大小：20605025 bytes，与源 `venus_dump/boot_a.img` 逐字节一致。
- Kernel SHA-256：`cb4030cc537f7082f6c791d567512acbb73bafa866f5b839979e60798eaea7b4`
- 修改前源码及 Android 程序备份：`/home/abc/SKYWARD/skyward-before-ui-autostart.jBAAZy`
- 本轮增量补丁：新输出目录下 `ui-autostart-changes.patch`。
- 构建和校验日志、输入快照、用户确认记录都在新输出目录。

## 启动行为

原 `recovery` 服务仍只运行诊断脚本。独立 `venus_ui` 服务在 boot 事件启动，在自己的进程里最多等待45秒，确认诊断完成标记与 DRM 节点存在后，执行：

```sh
/system/bin/skyward ui-probe --config /skyward/device.toml --until-stopped
```

不调用 `/system/bin/recovery` 的完整流程，不处理 BCB 请求，也不调用生产 `ui::run`。ADB 与原 DEBUG 日志服务保持独立。菜单服务为 disabled + oneshot，不使用 critical/reboot_on_failure，退出后不会自动循环重启；可通过 `setprop ctl.start venus_ui` 手动再启动。

`--until-stopped` 只有在存在 `/skyward/ui-autostart.txt` 且标记正确时可用。手动 `--seconds` 模式仍为1至1800秒，默认900秒。两者不能同时显式传入。

自动版不再15分钟退出，初始空闲熄屏设置为120秒。菜单可改变内存中的显示设置，进程退出后丢失。背光仍限制在测试用较低范围。退出时恢复原亮度。此设置及启动时序尚待新镜像冷启动验证。

## 功能边界

保留原先默认拒绝的 effect handler，函数内容未改变。菜单中安装、清除、Data解密、写分区、切槽、重启、终端和USB重配等操作仍不执行。底部测试标识保留。它是可自动显示菜单的移植测试版本，不是完整可用 Recovery。

不主动改动 Data。原独立日志服务可能挂载 Metadata 并写诊断日志，所以不能称为完全只读启动。调试 ADB 属性保持开启，仅用可信 USB 连接。

## 离线验证

- 22项 skyward-pack 测试通过。
- 332项 skyward-ui 库测试通过，其中包括持续运行授权和默认拒绝策略测试。
- packer Clippy `-D warnings` 通过。
- Host release 与 Android ARM64 release 构建通过。
- 新镜像 GZIP、新旧CPIO原条目元数据、内核、Boot Header、init两处8字节修改、自动启动脚本和服务均通过独立验证。
- 新镜像 AVB 自校验通过；Algorithm NONE 不代表获得厂商签名。
- 普通模式及纯探测模式各自生成镜像并验证，自动菜单文件和服务没有泄漏进去。
- 新CLI在当前旧镜像上拒绝无授权标记的常驻运行；显式 `--seconds` 与 `--until-stopped` 同时使用返回参数错误，均在打开显示之前退出。
- 源Boot、Vendor Boot、VBMeta和上一份已验证entryfix镜像校验值未变。

## 手动临时启动（本轮未执行）

先把当前RAM日志拉取到电脑，再进入Bootloader。以下是用户后续操作，不是构建脚本的行为：

```sh
adb reboot bootloader
fastboot devices
fastboot getvar is-userspace
# 确认为 no 后：
fastboot boot /home/abc/SKYWARD/skyward/target/venus-ui-autostart.eiQASW/boot.img
```

不使用 flash、不切槽、不改 vbmeta。该测试镜像带有强制Recovery分流，不可刷成日常Boot。

启动后检查：

```sh
adb shell 'getprop init.svc.venus_ui; getprop init.svc.adbd; cat /tmp/skyward-ui-probe/ui.log'
```

预期看到 `VENUS_UI_AUTOSTART_EXEC` 与 `UI_PROBE_FIRST_FRAME_SETTLED`。还需要用户确认无需电脑推送、无需执行 `start-ui-probe.sh` 即自动出现菜单。若只有 ADB 而无菜单，优先保留 `/tmp/skyward-ui-probe` 和 `/tmp/skyward-probe` 的日志，不刷写其他分区。

重新构建：

```sh
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-ui-autostart.sh
```

脚本仅在全部检查成功后更新 `target/venus-ui-autostart-latest.txt`，输入是当时的最新 `venus_dump/boot_a.img`，输出始终在新目录，不覆盖旧产物。
