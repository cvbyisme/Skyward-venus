# Venus 完整菜单与可写 Data：2026-11-09

本目录接续已有 Skyward Venus 移植。原来的探测版、只读 CE 版和本版是不同入口；历史 README 的验收范围不能代替本版实机结果。

## 用户指定的功能目标

2026-11-09 用户指定以两篇酷安移植帖的功能为目标：

- [Xiaomi 15，74941594](https://www.coolapk.com/feed/74941594)，TUSB_5834。
- [Xiaomi 15 Pro，74940888](https://www.coolapk.com/feed/74940888)，澪洛依。

网页请求被站点拦截；已从手机酷安读取正文与配图。原帖截图在
`/home/abc/SKYWARD/venus_dump/full-20261109/references/`。
第二帖对应公开分支 `Rinrroy/skyward-for-haotian`，参考提交
`a6cfaaa`（0.2.2）；上游参考提交 `ff1668f`（0.2.0）。
参考代码的机型配置、硬件依赖和发布地址必须按 Venus 适配，不能把 haotian 镜像用于 Venus。

2026-11-11 用户明确指定：以 [Skyward 官方上游 kirklin/skyward](https://github.com/kirklin/skyward) 为主，其他分支仅供参考。既有 0.2.2 迁移记录属于参考分支历史，不能将其行为视为官方标准；本次 USB 审查已按官方 `ff1668fdb72a46f22152a6540f49728c42081aba` 对照和修正。

| 功能目标 | Venus 验收要求与当前边界 |
| --- | --- |
| 安装 ZIP、OTA、IMG；内部存储、U 盘、ADB sideload | 使用当前原始 BOOT 验证写入与读回；动态分区释放仍需适配 |
| Boot、System、Data、EFS 备份恢复 | 通过 REC 本身备份、校验、恢复；先用原始启动分区验证 |
| 解密 Data、挂载、文件管理、终端 | owner0 PIN 与可写 CE 已实测；其他用户与凭据类型独立验收 |
| WLAN、MTP、ADB、fastbootd、重启与切槽 | MTP/ADB 已实测；其余需要 Venus HAL/驱动及生命周期验证 |
| 清除数据、恢复出厂、文件系统修复/格式化 | 先在独立测试文件系统验证，不能用个人 Data 做破坏性试验 |
| AVB 工具、SELinux 上下文修复、导出日志 | 验证工具实际行为、作用范围和错误返回 |
| AOD、双击亮屏、充电与息亮屏动画、振动、时间、主题和圆角 | 以两帖配图和功能列表为准，验证 Venus 面板、触摸和马达实际行为 |
| 双系统安装与切换 | 合入上游实现后适配 Recovery-as-Boot、原系统回退和中断恢复 |
| 系统在线更新、快照合并、Skyward 内更新 | 检查当前槽、快照状态、机型及镜像兼容性；不能使用 haotian 的发布源刷入其镜像 |

这些条目是待完成的验收目标，菜单存在或编译成功不代表实机已通过。

2026-11-10 用户新增 SIM 卡读取与蜂窝上网目标。验收需分别证明卡槽/卡就绪、驻网/信号、所选 SIM 的数据会话、rmnet/IPA 数据接口、路由/DNS 与真实 HTTP/TLS 联网，以及断开/重新连接和与 WLAN 切换。不得把读取到 SIM 状态算成已能上网；实现不包括自动拨号电话、短信或猜测 SIM PIN。

设备信息页按用户要求保持原版署名 `© 2026 Kirk Lin`，不添加移植者或其他署名；原有许可证声明保留。

用户补充界面要求：Wi-Fi 保留 Skyward 官方的设置、扫描、连接及密码输入界面；SIM 增加反映实际信号的图标，以及显示卡槽、卡状态、基础信息和网络状态的简易设置页。菜单必须读取真实后端状态，不能用示例数据代替设备结果。

2026-11-10 网络进展记录在 `/home/abc/SKYWARD/venus_dump/network-20261110/network-verification.json`。早期 `07bUXt` 与 `jVQZ1o` 的直接 QMAPv1 路径在首次收发时触发过 `kpanic`，没有 panic 栈，不能断言已定位内核故障。对照当前主系统后，改用原厂 IPACM + netmgrd 管理的 `rmnet_data0`、QMAPv5（WDA 2/9/9）路径；在 `jVQZ1o` 的同一次 REC 启动中，两次 HTTP 与 HTTPS 下载校验均通过，其中第二次使用基带默认 3GPP profile 3，未覆盖 APN。证据分别为 `network-capture.Le9i9o` 和 `network-capture.zW53J0`。这仍是显式 RAM 诊断：自动蜂窝服务管理、数据开关与 WLAN 切换没有验收通过。首次停止成功；最后一次 Stop 返回 error 26，独立 WDS 查询确认 disconnected 后，显式清除了本轮地址和路由，不能把它写成完整断开生命周期已通过。原始 modemst1/modemst2/fsg/fsc/persist 备份已保留，正常基带 NV 更新不能用旧备份盲目覆盖。

`target/venus-recovery.jVQZ1o` 已实机临时启动两次；六个安全/控制服务、原始逻辑分区和 modem 两处源别名完整释放、屏幕 PIN 与 8 KiB 写入测试通过。设备信息页已拍屏确认仅保留 `© 2026 Kirk Lin`。

`target/venus-recovery.gUFJAA` 接入 Venus 原厂 CNSS、原厂 supplicant 和官方 WLAN 工作者及所有原有页面，并增加只读 SIM/驻网/运营商/信号页及状态栏图标。实机临时启动、CE 解锁、停止 UI 后完整关闭 Data 均通过；由 UI 持有的原厂 WLAN 服务可扫描，独立同协议探测读到 72 个 BSS。首次启动曾因安全环境尚未就绪而失败，重试后成功；时序修复候选 `target/venus-recovery.7uinRm` 已临时启动验证：boot ID `246120e5-f8da-4f0f-b927-15bc0268252b`，无需手动重试即自动启动 WLAN，同协议扫描得到 47 个 BSS，官方列表页已拍屏确认。两个新候选的完整离线日志各汇总 773 项 Rust 测试，另有网络采集等脚本回归检查。SIM 页不读取 IMSI/ICCID，也没有尚未实现后端的数据开关或 APN 编辑器。NR 不可用测量值不会借用 LTE 值显示满格信号；信号显示参照 AOSP 的默认 LTE/NR 阈值，未套用运营商覆盖值。


补充实机验证：`7uinRm` 的官方 WLAN 开关可完整停止并重新创建四个网络辅助进程、恢复热点列表；同一 boot ID 内基带保持 ONLINE，六个安全/控制 HAL 保持原进程。只读 SIM 页实机看到 USIM ready、5G NR -98 dBm 与 LTE -92 dBm 的变化，第二卡槽保留基带返回的 card error/no ATR，未伪装成已确认的空卡槽。基带返回的无编码标签运营商字段曾出现非 UTF-8 字节，因此新版拒绝乱码并在 MCC/MNC 完全相同时读取 NAS Home Network；实机独立查询返回 460/15、CBN。该查询不读 IMSI/ICCID。

`target/venus-recovery.mReLwt`：775 项 Rust 测试和原有脚本/镜像验证通过；临时镜像 SHA-256 `9735593a635e80b1e9803226afc35ee30c7f3d2f4c4185998ed65f20b436a825`，程序 `04535a9c9310713445519bdd06827679a287fc1ce66c83fa0e93b649428ff20b`。已重试临时启动，boot ID `96f4ed17-72c5-457a-8f69-0e469fb287ee`，Data/DE 和原厂 supplicant 两个 HIDL 服务注册通过。此前第一次掉 USB 后重现 fastboot，缺少证据归因，不将其写成已定位的镜像崩溃。不要拿 `ewKAc3` 的旧解码结果代表严格运营商解码的新版本。

### 启动时读取锁屏方式的等待

等待的主要部分是同步准备约 2,833,759 KiB 原厂运行文件，尚未开始密码认证。现保留所有原始条目、权限/符号链接、完整源文件与 RAM 文件 SHA-256 校验和总内存预算，改为最多五个任务并行复制/校验，并将真实安全服务就绪轮询由 1 秒缩短为 100 毫秒。所有任务均须退出后才能报错或原子发布就绪记录；校验失败不启用缓存，也不遗留后台复制。Keymaster 查询保留单次 5 秒和总时间上限。

| 临时镜像 | 从内核启动到 Data/DE 准备完成 | 实机验证 |
| --- | --- | --- |
| `mReLwt` 基线 | 26.644 秒 | DE 就绪，未提交密码 |
| `cSRf6a` 四路复制 | 20.727 秒 | 一次屏幕 PIN、CE 文件读写、同步关闭 Data、官方 WLAN 列表均通过 |
| `EsKbly` 五路复制、细化轮询 | 15.609 秒 | 一次屏幕 PIN、8 KiB 文件往返、释放全部原始逻辑/固件源别名通过，六个安全/控制 HAL 保持原进程 |

`EsKbly` 临时镜像 SHA-256 `407974e92a22bbdf967bafecfe2fcde2d7ed5b47609da60e64e862080d7bcb83`，boot ID `94460194-cb63-433c-93e9-dd81361798ec`。比基线缩短 11.035 秒（41.4%），仍需约 15.6 秒完成冷启动准备，不能称为瞬时识别。分阶段时间及内核日志在 `venus_dump/network-20261110/logs/{mReLwt,cSRf6a,EsKbly}-startup-*`。

`EsKbly` 也验证了严格运营商字段解码及同 PLMN 名称补充：真实 SIM 页显示 CBN、460-15、5G NR -99 dBm、USIM ready；新建基带从 DMS mode 5 经一次 online 请求变为 mode 0 并驻网，异步等待修复通过。仍是显式 RAM 基带诊断，未自动创建蜂窝数据会话。

WLAN 出厂地址适配候选 `target/venus-recovery.T48gRX`：778 项 Rust 测试与原有构建/镜像检查通过。静态分析及实机日志确认驱动找不到 `wlan/qca_cld/wlan_mac.bin`，而本机 persist 中保存的是 `wlan0=<12 位十六进制>` 格式。新增严格解析后转换为当前 Venus 内核的 `Intf0MacAddress` 格式，只读挂载 persist 并在读取后卸载；在 RAM 中硬链接现存固件，再将包含转换文件的目录只读绑定给固件加载器。原始 cache/persist 内容不改，不硬编码实际 MAC；冷启动、地址匹配及开关循环结果待记录。

蜂窝服务自动接入还需独立 init 持有者、状态/控制通道、真实 WDS 会话和接口/DNS 释放，以及与 WLAN 路由、EFS 备份/恢复的协调。不能为了让 SIM 图标常亮就把修改 NV 的基带服务无条件塞进 UI 进程组：UI 停止必须先关闭数据会话和 modem 引用，再停止 RMT/TFTP；EFS 作业也需确认 radio 已退出。

## 本轮修复

### 2026-11-18：Keystore2 和 fscrypt 解密启动修复

修复私有 BinderFS 容量不足、私有 framework VINTF 缺少 Keystore2 声明、原载体 SELinux 策略缺少 Keystore2 类和命名空间，以及 Recovery 缺少 fscrypt 会话密钥环的问题。最后一项原本在文件系统已接收 wrapped key 后失败，又被非 wrapped 模式重试掩盖为错误 22；现在先准备会话密钥环，仅在 ENOKEY 时创建，并保留真实失败信息。

`target/venus-recovery.ufKbT7` 完整构建通过 952 项 Rust 测试（2 忽略）、脚本检查及镜像校验。独立 REC SHA-256 为 `dd0944cb607abd0c3acab383313969a1de8793eea213b2a950035ffb41f27eb5`。重新临时启动的 boot ID 为 `1787f585-8663-4b37-a55b-ce144cae9664`，18.037 秒达到 DE_READY，手机屏幕凭据输入后 27.028 秒达到 CE_STORAGE_READY。四处 DE/CE 策略的内核密钥状态、`/sdcard` 映射、8 KiB 写入/读回/fsync/清理和 ADB 往返均通过。boot_a/boot_b 哈希未改变；尚未验证持久安装及其他用户。

最终镜像与完整记录在 `/home/abc/SKYWARD/out/data-fix-20261118/final/`。该 REC 用于 `fastboot boot`，本次未刷入分区。

### 安全服务与菜单进程组

原 `venus_ui` 在解密工作线程内启动 QSEE、HwServiceManager、Keymaster 和 Gatekeeper。它们虽然使用私有挂载命名空间，仍属于 `venus_ui` 的 init 进程组。停止菜单时 init 清理整个进程组，四个 HAL 随之退出；重新启动菜单只能看到过期的 `runtime-ready`，无法再次解密。

现在由独立的 `venus_security` init 服务持有这些进程。菜单按需请求启动、等待就绪并核验现存服务，不再亲自创建 HAL 进程。安全服务在初始化完成后保持运行，不设置自动重启；Data 存活时不会自动重建安全环境。首次启动时 disabled 服务没有 `init.svc.*` 属性是合法初态，已加入回归测试。

新增显式命令：

```sh
skyward venus-data-rw --close-session
```

它要求菜单已停止、MTP 已关闭、ADB 已恢复，并核验本次 boot ID、块设备、挂载 ID 和私有会话记录。确认 `/sdcard` 指向本次 owner0 存储、没有额外挂载后，按顺序执行：

1. `syncfs(/data)`，随后关闭同步使用的文件描述符。
2. 普通卸载 `/sdcard`（若存在）和 `/data`。
3. 移除本次 `userdata` Device Mapper 映射。
4. 最后删除 RAM 会话记录，使下次打开能建立新会话。

任一步失败立即返回，不继续后续步骤，不强制/懒卸载，不格式化、不清除密钥、不猜测凭据。每一步都有开始/完成日志；失败时保留仍适用的现场信息，不自动重试。

## 构建

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-recovery.sh
```

Keystore2 版本的构建还需要主机 `checkpolicy` 和 `secilc`（已验证 Ubuntu 22.04 的 3.3）。默认路径为 `$VENUS_TOOLS_DIR/selinux/usr/bin/`，`VENUS_TOOLS_DIR` 未设置时使用 `$HOME/SKYWARD/tools`；也可用 `VENUS_CHECKPOLICY`、`VENUS_SECILC` 指定两个工具的完整路径。可将发行版 deb 解压到该工具目录，无需修改宿主机 SELinux 设置。

`prepare-crypto-policy.py` 从原始 boot 提取 Recovery 策略，加入 `crypto-policy.cil` 中的两个 Keystore2 类、命名空间类型和三个 Recovery 授权规则，再按原策略版本编译。审计保留原有类编号、属性和有效规则；仅允许编译器省略源/目标类型集合为空的无效规则。不添加 permissive 域，不修改输入 boot 或 TWRP 镜像。构建目录的 `policy/validation.json` 记录输入、输出哈希和完整增量。类定义及 vold 权限对应 [AOSP Android 12 access_vectors](https://android.googlesource.com/platform/system/sepolicy/+/refs/heads/android12-release/private/access_vectors) 和 [vold.te](https://android.googlesource.com/platform/system/sepolicy/+/refs/heads/android12-release/private/vold.te)。

完整物理 `super.img` 刷写行为、内部存储直读性能路径、安全检查及构建产物说明见 [`README-super-flashing.md`](README-super-flashing.md)。

只在构建、测试和镜像独立校验全部成功后更新 `target/venus-recovery-latest.txt`。以下为 2026-11-09 阶段的历史产物；2026-11-11 最新 USB 修复产物及实机结果见文末。历史产物目录：

`/home/abc/SKYWARD/skyward/target/venus-recovery.gfZcqd`

| 产物 | SHA-256 |
| --- | --- |
| `temporary/boot.img` | `8732e8fd0789268812a9008968474cd43bec25f3fdb5a8e0ec9d98750185b310` |
| `normal/boot.img` | `abd0636b28c36a29bcc0c9ca25bbffb1f3463fdca2f596dc1301f836e341be79` |
| `assets/skyward` | `57da47c98f7975e48df8dc4aa1a95b93d702bb82d9ddcd7da729d3de80be3966` |

两份镜像均为 201326592 bytes，保留输入 DEBUG Kernel。`temporary` 带已核对的强制 Recovery 分流，只用于 `fastboot boot`；`normal` 恢复原始 init，但尚未验证安装后的双模式启动，不能当作日常可刷版本。

离线验证：522 项 Rust 测试、运行脚本回归、ARM64 release 构建、packer Clippy、CPIO 条目/权限、原内核与 first-stage 文件不变、独立安全服务结构、AVB hash 校验。AVB Algorithm NONE 不是厂商签名。

## 实机证据

证据目录：`/home/abc/SKYWARD/venus_dump/resume-20261109`。

初始候选版已经完成屏幕 PIN 认证、CE 解锁、8 KiB 临时文件创建/写入/fsync/读回/重命名/删除，以及真实 USB MTP 上传/下载/删除。文件测试只操作新建的随机测试文件，不将用户目录内容保存到报告。

初始候选版的可写 Data 卸载和映射移除也成功，同一 boot ID 保持不变；随后复现了菜单停止导致 HAL 退出的问题。修复后的中间镜像已经证明 HAL 位于独立 cgroup，菜单停止后四个 HAL 保持原 PID；未解锁 CE 的 Data 也能正常关闭。

最终镜像 `venus-recovery.gfZcqd/temporary/boot.img` 已通过以下独立实机验收（Recovery boot ID 为 `12136fa6-a46a-4f1e-9f35-a1bf3544d572`）：

| 检查 | 结果与证据 |
| --- | --- |
| `fastboot boot` 后自动菜单、安全服务和 Data/DE 初始化 | PASS，`final-fastboot.txt`、`final-fresh-state.txt`；运行程序 SHA-256 与产物一致 |
| 屏幕 PIN → Gatekeeper → 保护器解包 → fscrypt CE 解锁 | PASS，`final-unlock-write.txt`；一次认证成功，无失败重试 |
| 8 KiB 临时文件写入、fsync、读回、重命名、删除 | PASS，`final-unlock-write.txt` |
| 真实 USB MTP 上传、下载及删除独立测试文件 | PASS，`final-mtp-result.json` |
| 关闭 MTP 后停止/重启菜单 | PASS，`final-before-ui-restart.txt` 与 `final-after-ui-restart.txt` 逐字节一致，HAL PID/cgroup、挂载会话和已解锁 CE 保持；`final-ui-reuse.log` 证明新菜单重新核验成功 |
| 菜单运行时拒绝关闭 Data | PASS，`final-close-refusal.txt`，返回 1，未卸载 |
| 菜单停止后同步、卸载 `/sdcard`/`/data`、移除映射及会话 | PASS，`final-close.txt`，返回 0，boot ID 不变，安全服务仍运行 |
| 再次启动菜单，重新建立 Data/DE | PASS，`final-reopen.txt`；CE 恢复为锁定状态，没有保留已卸载文件系统的解锁状态 |
| 从重启菜单滑动返回系统 | PASS，Android 17 `sys.boot_completed=1`、slot `_a`、SELinux Enforcing，`final-android-state.txt` |
| 原始分区未被刷写 | PASS，启动前后的 boot_a/vendor_boot_a/vbmeta_a SHA-256 完全一致 |

机器可读验收记录在证据目录的 `verification.json` 及产物目录的 `on-device-verification.json`。镜像的 `temporary/validation.json` 则保持离线校验含义，不篡改成实机记录。

修改前四个源码文件保存在证据目录的 `source-before/`，相对此次任务起点的增量补丁为 `this-run.patch`。已有移植改动未回退或清理。

## 后续实机验证与 0.2.2 迁移

以下是上述 `gfZcqd` 之后的独立验证，证据目录为
`/home/abc/SKYWARD/venus_dump/full-20261109`。

`target/venus-recovery.KcwcrQ/temporary/boot.img`（SHA-256
`8bee673064e896d8ed96a7bb1aee344714910bb93c209f9f163a85a69ad343c0`）已临时启动，并完成：

- 通过 REC 文件选择、目标分区选择与滑动确认，将当前原始 `boot_a` 镜像刷回当前 A 槽；写入后读回校验成功，A/B 两槽 boot 的 SHA-256 均与操作前一致。
- 通过 REC 界面执行 Boot 组备份，生成 `/sdcard/Skyward/backups/20261109-182544`；实际包含 boot、vendor_boot、dtbo、vbmeta、vbmeta_system 五个 A 槽分区。
- 将备份完整拉回主机，独立验证每个文件的长度、SHA-256 与备份清单及原始分区一致，见 `rec-backup/host-verification.json`。
- 通过 REC 界面恢复该 Boot 组，五个分区全部完成；恢复后逐分区 SHA-256 与原始值一致，见 `logs/restore-verification.json`。
- 从 REC 重启系统，Android 启动完成、槽位仍为 `_a`、SELinux 为 Enforcing；再次核验五个分区哈希一致，见 `logs/restored-android-state.txt`。

本次写入均为当前原始分区副本，未擦除 Data、未改写 super、未切槽。原始镜像保存在 `original/`，REC 备份保存在 `rec-backup/`。

随后已将参考分支 0.2.2 的系统管理、界面、AOD 等实现与现有 Venus 移植合并，保留 Venus 的 GZIP/Recovery-as-Boot、解密与独立安全服务适配。迁移后的 workspace 测试和 Android release 构建已通过。初次 0.2.2 打包目录为 `target/venus-recovery.jcqEVu`；它未进行实机启动验收，且之后新增的 Binder 域/服务管理器版本、AW8697 振动、信息刷新适配尚未重新打包实测，不能把前述旧版的实机结果记到该候选版。

该阶段手机已返回正常 Android。后续验证继续使用临时启动，未安装候选 Recovery 到 boot。VMware 自动连接辅助脚本在 `/home/abc/SKYWARD/tools/Set-VenusVmUsbAutoConnect.ps1`；宿主机连接工具内部错误，尚未写入宿主机 `.vmx` 或验证重启自动连接。

## 0.2.2 的自动服务、RAM 运行环境与刷包验证

以下记录接续上面的历史镜像，不能把不同候选的结果混为一次验收。

- BootControl 使用已安装 ROM 的 AIDL 服务与 Qualcomm 后端，按实际 Android 17 服务管理器使用 SYST Binder 域；独立链接命名空间保留系统与 VNDK30 的依赖边界。根据 Venus SoC 415 和已安装 qcv 配置恢复 lahaina SKU，实际 VINTF 声明与注册均核验。启动过程只读取槽位，不写 BCB/切槽。
- AW8697 使用 Venus 内核真实的 FF_CUSTOM 三个 i16 参数，并把固件放入内核直接请求的 `/lib/firmware`。等待固件与 FF 上传就绪后才开放马达；实机 click 20 ms、息屏 ticks 95 ms 已执行。
- 信息刷新按时间截止触发，解决持续电池事件使完整刷新饿死的问题。系统属性从 Venus 自己的挂载读取，解密后刷新空间；RAM 缓存完成后读缓存，避免后台信息线程重新占用安装器要卸载的原分区。
- 在任何 HAL 启动、Data 打开前，把明确列出的只读系统程序、库、配置、厂商/基带固件复制到 tmpfs，逐文件 SHA-256 核验，再原子发布当前 boot ID 的缓存凭据。当前 ROM 约占 2833759 KiB；预检保留至少 1 GiB 可用内存。不会复制 Data、metadata、凭据或 RPMB。
- 六个服务分别在私有命名空间内只读绑定 RAM 目录，并普通卸载各自继承的原逻辑分区、固件和存储挂载。仅 chroot 不会释放外部继承挂载，这正是此前动态分区仍被占用的原因。
- 修复设置页部分可见行在按下/滚动时缺少裁剪素材导致的 panic。主机回归测试先复现相同断言，修复后通过；实机同一路径滚动通过。

`target/venus-recovery.vRvrw6/temporary/boot.img`，SHA-256
`b495d04fb4903efd64c78aba752bd78fda1e34cb2b273babc0311ec6d0fbaad1`，
已 `fastboot boot` 启动。实际 boot ID 为
`0fcf30b0-91f8-46b2-8724-a5300b27e297`，运行程序与产物 SHA-256 一致。

| 验证 | 实机结果 |
| --- | --- |
| 自动六服务、RAM root 与独立 cgroup | PASS；system/vendor 的 statfs 均为 tmpfs |
| 卸载全部原系统逻辑分区并移除本次映射，卸载基带固件 | PASS；普通卸载，无 lazy/force |
| 原分区保持卸载，关闭后重新建立 Data/DE | PASS；使用缓存 fstab 与现存 Keymaster |
| 屏幕 PIN、CE、8 KiB 创建/写入/fsync/读回/重命名/删除 | PASS；一次屏幕认证成功 |
| 设置页部分行按压与滚动 | PASS；菜单持续运行 |
| 真实 USB MTP 上传/下载/校验/删除 8 KiB | PASS；`logs/cache-ui-mtp-result.json` |
| 六服务 PID/root/cgroup 前后相同 | PASS；`cache-ui-before-release.txt` 与 `cache-ui-after-release.txt` 的六服务记录相同 |
| 关闭 MTP、停止菜单后严格关闭已认证 Data 会话 | PASS；`cache-ui-authenticated-close.txt` |
| boot_a/vendor_boot_a/vbmeta_a/misc 原始哈希 | PASS；与 `original/` 一致 |

该镜像的 `on-device-verification.json` 记录上述边界；当时还没有后面的生产刷包适配和管道修复。

随后新增生产释放适配器 `venus-runtime`，在当前 boot 的 root 私有 tmpfs 缓存、六服务的独立 owner/cgroup、不同私有 namespace、只读 RAM bind 的设备号/inode、PID/starttime 与就绪属性全部核验后，才普通卸载自己的 `venus_ro_*` 映射与已核对的当前槽只读 modem 固件挂载。拒绝接管其他映射；释放后再次确认六服务不变。

实机故障测试使用独立的原始块设备只读 holder：

1. 把一份 PID 凭据临时指向外部进程，释放在任何卸载前拒绝，恢复原凭据后服务保持不变。
2. 故意持有 vendor 的原始 DM 块设备 FD，使卸载后 DM 移除返回 busy；适配器恢复完全相同的只读挂载并报告失败。
3. 关闭该测试 FD 后重试成功，再次释放也成功。证据为 `logs/runtime-adapter-device.txt`。

首次 ZIP 验证还发现安装器将请求管道作为 fd 24 传给 Android mksh，脚本 `>&24` 失败。现在仅在子进程将请求管道 dup 到 fd 3，保持父进程 FD/CLOEXEC 不变。包含高编号原始 FD 的真实 shell 回归和安装库 127 项测试通过。

`Venus-Original-Boot-Validation.zip` 内含当前原始 boot_a，脚本在任何写入前核验机型、当前 A 槽、RAM 服务、原分区已卸载，以及分区/包内 BOOT 的长度和原始 SHA-256。经 REC 文件选择与滑动确认后，将同一原始 BOOT 写回 A，fsync，再读回核验，脚本退出 0、界面显示安装成功。六服务、CE 和临时文件读写继续正常；其他分区未被脚本写入，A/B boot、vendor_boot_a、vbmeta_a、misc 原始哈希均不变。

这次 ZIP 结果来自 vRvrw6 临时启动后的 RAM 程序替换测试，运行程序 SHA-256 为
`10873e22215dc2d5dc8c89f704fd6789a9c98f08c94d7ce2c752ed3b807bc714`。
证据为 `logs/zip-pipe-fixed-verification.txt`、`zip-pipe-fixed-finished.png`、`zip-pipe-fixed-cleanup.txt`；它本身不等于重新打包镜像后的冷启动验收。验证包未签名，使用界面原有关闭的签名开关；没有跳过或修改签名验证实现。

ZIP/IMG 入口已接通；A/B OTA 仍在每个 OTA 进入 update_engine 前单独拒绝，待 metadata/engine 生命周期验收。System/Data 恢复、格式化、切槽、fastbootd、双系统等限制仍保留。

包含生产释放适配与 fd 3 修复的新打包目录是
`/home/abc/SKYWARD/skyward/target/venus-recovery.07bUXt`。
722 项 Rust 测试、运行脚本与故障夹具、ARM64 release、静态 system-init、packer Clippy、独立 CPIO/权限/旧模式隔离检查与 AVB 校验已通过：

| 产物 | SHA-256 |
| --- | --- |
| temporary/boot.img | `dc3d8396b96efe2487bf39d39bb4e97d35187ad8d5ffa494f0e37eda9407b7f2` |
| normal/boot.img | `3e24ddec4d7ea99a3f651b61e19cffb80da5075ba36a77b1598a62cfbec76589` |
| assets/skyward | `10873e22215dc2d5dc8c89f704fd6789a9c98f08c94d7ce2c752ed3b807bc714` |

该打包镜像随后已临时启动，boot ID 为 `137a2c1a-dce8-4226-a0a6-5d20c0038c33`，程序哈希与产物一致。完成挂载页 vendor 卸载/重新挂载、屏幕解密、同一 ZIP 的原始 BOOT 写入/fsync/读回校验，以及安装后的设置滚动、真实 MTP、振动与日志导出。安装器释放源分区并清理可写逻辑映射后，六服务 PID/root/cgroup 不变；关闭 Data 后在源分区保持卸载时重新建立 Data/DE，再次屏幕认证与文件读写通过，最后严格关闭会话。

手机随后返回原 Android 17，`sys.boot_completed=1`、`_a`、SELinux Enforcing，A/B boot、vendor_boot_a、vbmeta_a、misc 哈希均与原始副本一致。机器可读记录在本产物的 `on-device-verification.json` 和 `logs/source-release-verification.json`，返回系统记录是 `logs/source-release-android-return.txt`。normal 的原 init 双模式安装启动仍未验收。

## 2026-11-11：按官方上游修正 USB 并重新实机验证

本次工作以官方 USB 生命周期为准，保留 Venus 必需的硬件就绪检查。另一聊天已由用户暂停，未并行操作设备；没有提交或推送源码。

### 源码审查与修复

- `crates/skyward-pack/src/mtp.rc` 已逐字节恢复官方 `ff1668f` 版本。参考分支在 `init.svc.adbd=stopped` 时清除 `sys.usb.ffs.mtp.ready`，但 sideload 接管 USB 时 MTP 服务仍活着，返回时没有新的服务启动去重新设置该属性，导致 MTP/ADB 组合无法再次绑定。
- `crates/skyward-sys/src/usb.rs` 恢复官方 `none → target` 切换，去掉仅凭属性判断已完成、以及直接添加功能的捷径。保留实际 UDC、configfs 函数链接和 FunctionFS 生产者就绪检查；sideload 由安装器持有端点，不要求 adbd 正在运行。
- 保留 Venus UI 初始化时的一次 USB 重建和有界 USB 状态日志。UI 重新启动会使 ADB 重新枚举，不能承诺已有 ADB 会话不中断。
- 同轮 WLAN 修正只涉及失败清理：读取地址失败后清理已创建辅助进程，未关联时继续检查进程退出；固件暂存目录改为每次操作独占，失败只清理自己创建的目录。主机故障测试通过，本轮没有重新完成 WLAN 连接/地址/开关实机验收。

状态机复现证实旧规则的 sideload/fastboot 返回问题，修复后通过；其中 fastboot 仍只有主机模型证据。对旧镜像解包对照未发现内核、adbd 或 QCOM USB 初始化内容缺失，不能把之前所有“新启动没有 ADB”都归因于这个 MTP 缺陷。

### 当前产物

目录：`/home/abc/SKYWARD/skyward/target/venus-recovery.jdgSEE`。

| 产物 | SHA-256 |
| --- | --- |
| `temporary/boot.img` | `843b6179920668d188b50350d098a360eb567ce432351663960608346ad2d02c` |
| `normal/boot.img` | `da18baa025bd3b42198ca4f1d2a5445b3b8a338e783e325e7123302da71c3082` |
| `assets/skyward` | `e4f3e9c76b88c7bb3a07cdec8e8aa611e6f8de579734259ea8abf4ae41a4fd69` |

823 项 Rust 测试、运行脚本回归、Android ARM64 release、静态 system-init、packer Clippy、CPIO/权限/输入内核/旧模式隔离及 AVB 校验通过。输入 BOOT 已在用户恢复后重新读回核验，SHA-256 为 `66fe152c3bdaf9c0406107f4b8b2675c34dd9e203e4a0ba977bc4dcd08029c60`。本次仍只 `fastboot boot temporary/boot.img`；normal 未安装或实测。

### 本轮实机证据

证据目录：`/home/abc/SKYWARD/venus_dump/resume-20261111`。boot ID `7e41ea1f-954c-4f1c-9438-1eabf1b51c78`，实机程序 SHA-256 与当前产物一致。

| 检查 | 结果与证据 |
| --- | --- |
| 临时镜像启动后 ADB、菜单、安全服务自动就绪 | PASS；`jdgSEE-fresh.txt`，真实 UDC 为 `a600000.dwc3`，硬件 configured |
| 用户在屏幕输入密码解密 | PASS；`unlock-status.txt`，owner0 CE unlocked，不读取或保存密码 |
| MTP 与 ADB 同时可用，真实 8 KiB 上传/下载/校验/删除 | PASS；`mtp-active.txt`、`mtp-roundtrip.json`，只访问新建测试文件 |
| MTP → sideload 等待 → 屏幕返回 | PASS；`sideload-host-state.txt`、`sideload-return.txt`，未发送刷机包；sideload 期间没有 ADB shell 属正常设计 |
| 返回后再次真实 8 KiB MTP 往返与删除 | PASS；`mtp-after-sideload.json`，无需重启服务，boot ID 不变 |
| 关闭 MTP 后恢复 adb-only | PASS；`mtp-off-final.txt`，仅 f1=ffs.adb，MTP ready=0，六服务及已解锁 Data 会话仍有效 |
| Boot A/B、vendor_boot_a、vbmeta_a | 测试前后 SHA-256 相同；`preboot-confirmed.txt`、`final-partitions.txt` |
| misc | 存在 `bootonce-bootloader` 命令字段差异；只在主机内将前 32 字节归零即精确重现测试前 SHA-256，其余内容相同，见 `misc-preboot-comparison.json`。未为统一哈希回写 misc |

机器可读记录为证据目录 `verification.json` 和产物目录 `on-device-verification.json`；`temporary/validation.json` 保持离线校验含义。本轮修改前文件、增量补丁和最终源码哈希分别在 `source-before/`、`this-run.patch`、`source-after.sha256`。继承的 USB 日志、UI 调用等修改已保留，不属于该增量补丁的全部内容。

当前停留在 REC，MTP 已关闭，ADB 可用。尚未验收实际 sideload 传包安装、fastbootd 往返、UI 重启过程中的 USB 恢复、多次启动/长期稳定性，以及 normal 镜像安装后的持久启动；本次 USB 结果不替代其他功能的独立验收。

## 当前尚未完成

- 原始 IMG 刷写、Boot 组备份恢复、RAM 服务环境、07bUXt 镜像临时启动和 ZIP 脚本刷写已有上述独立实机证据；A/B OTA、System/Data 恢复、格式化、切槽、fastbootd 与双系统仍须继续验收。
- 只读诊断版此前的卸载重启根因仍未知。本轮验证的是完整 Recovery 的可写 Data 流程，不能据此宣称旧问题已修复。
- 本版仍基于 permissive DEBUG Recovery；没有完成 Enforcing 策略、全部用户/工作资料、图案/Weaver 或长时间稳定性验收。
- 前一 `gfZcqd` 阶段没有刷写分区；后续 `KcwcrQ` 阶段按用户授权完成当前原始副本的写回与 Boot 组恢复。凭据不保存在源码、脚本、命令参数或报告中。
