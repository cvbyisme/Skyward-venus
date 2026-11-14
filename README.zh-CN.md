# Skyward — 小米 11（venus）移植版

**Skyward-venus** 是基于 [Skyward Recovery 上游项目](https://github.com/kirklin/skyward)维护的小米 11（代号 `venus`）设备移植与开发仓库。这里集中维护 Venus 设备配置、Recovery 集成、构建打包流程和机型专属的安装功能。它不是上游官方发布版本，也不是适用于其他小米机型的通用镜像。

[English](README.md) · [Venus 构建说明](porting/venus/README-recovery.md) · [物理 super 镜像刷写说明](porting/venus/README-super-flashing.md)

> **开发状态：实验性机型移植。** 主机端编译成功或镜像静态校验通过，不代表手机上的启动、解密、挂载、无线服务或 Recovery 操作全部安全可用。测试时必须使用与目标机型和分区布局匹配的构建，并按项目说明逐项验收。本 README 不提供可直接刷入的预编译镜像。

## 目标设备

| 项目 | 配置 |
| --- | --- |
| 设备 | Xiaomi Mi 11 / 小米 11 |
| 代号 | `venus` |
| SoC | Qualcomm Snapdragon 888 / 高通骁龙 888 |
| 分区布局 | A/B |
| 屏幕配置 | 1440 × 3200，逻辑 density 560 |
| 设备配置 | [`devices/venus.toml`](devices/venus.toml) |
| Vendor 集成来源 | `vendor_boot`、`vendor` |

上述内容描述的是本仓库的目标硬件配置，不代表所有功能已经在所有固件版本上完成独立实机验证。不同固件版本的 vendor 文件和分区布局可能不同，禁止将其他 codename 的镜像或设备 dump 用于 Venus。

## 本仓库维护的机型功能

- **Venus Recovery 打包。** 将 Recovery 程序、Venus 设备配置、选定的 boot 镜像、运行时资源和安装 capsule 组合成机型专用镜像。
- **机型适配。** 包含 Venus 显示、触摸、vendor 运行环境和部分硬件服务的集成工作。Keymaster 4.1、Gatekeeper、fscrypt 相关代码也在仓库中，但凭据类型、冷启动、挂载模式和卸载流程必须分别进行实机验收，不能只凭编译结果判断可用。
- **完整物理 `super.img` 安装。** 安装器识别 raw 和 Android sparse 格式的完整物理 super 镜像，校验 LP Metadata 与目标分区几何信息，并在写入前检查相关 snapshot 和块设备状态。
- **内部存储直读路径。** 从 Recovery 内部存储选择物理 super 镜像时，直接使用已打开的文件描述符读取，绕过 sideload FUSE 数据通道。写入使用 4 MiB I/O 缓冲，并保留源数据哈希和目标分区全量读回 SHA-256 校验。ADB sideload 仍使用原有传输路径。

物理 super 安装针对完整的 `super.img`，不是单独的 `system.img`、`vendor.img` 或其他逻辑分区镜像。该操作会替换两个 metadata slot 所描述的物理 super 内容；镜像错误可能导致系统无法启动。使用前必须阅读[物理 super 刷写说明](porting/venus/README-super-flashing.md)。

## 构建 Venus Recovery

当前维护的统一离线构建入口：

```bash
cd /path/to/Skyward-venus
bash porting/venus/build-recovery.sh
```

脚本不会连接手机、挂载 userdata 或刷写任何分区。它会运行主机侧回归测试，构建主机端 packer 和 Android ARM64 Recovery 组件，打包三种 Venus 镜像，并执行镜像、capsule 和 AVB 校验。成功后会更新 `target/venus-recovery-latest.txt`。

### 主机环境依赖

- 仓库 [`rust-toolchain.toml`](rust-toolchain.toml) 指定的 Rust 工具链，以及项目需要的 Android target。
- `cargo-ndk` 和 Android NDK r29。
- Python 3、主机 C 编译器、`readelf`，以及本地工具目录中的 `avbtool.py`。
- 下文列出的设备提取文件和运行时资源。这些文件仅作为本机构建输入，不存储在本仓库中。

### 构建输入目录

构建脚本支持环境变量覆盖。以下默认值与 `porting/venus/build-recovery.sh` 保持一致：

| 环境变量 | 默认值 |
| --- | --- |
| `ANDROID_NDK_HOME` | `$HOME/Android/Sdk/ndk/29.0.14206865` |
| `VENUS_DUMP_DIR` | `$HOME/SKYWARD/venus_dump` |
| `VENUS_TOOLS_DIR` | `$HOME/SKYWARD/tools` |
| `VENUS_KM41_CACHE` | `$VENUS_DUMP_DIR/keymaster41.sSBzw7` |
| `VENUS_TETHERING_IMAGE` | `$VENUS_DUMP_DIR/network-20261110/apex-ro/com.android.tethering.img` |

Dump 目录必须包含 `boot_a.img`、`vendor_boot_a.img`、`vbmeta_a.img`；Keymaster cache 必须包含 `runtime.img` 和 `vndk30.img`。此外还需要配置的 tethering 镜像、`avbtool.py`，以及仓库内的 Venus framework/suspend 构建输入。缺少文件或固定哈希校验失败时，脚本会停止。若本机目录不同，请通过对应环境变量指定路径，不要为了适配单台电脑而把个人绝对路径写进脚本。

### 查找构建产物

构建成功后执行：

```bash
OUT="$(cat target/venus-recovery-latest.txt)"
printf 'Standalone REC: %s\n' "$OUT/standalone/skyward-rec.img"
printf 'Temporary carrier: %s\n' "$OUT/temporary/boot.img"
printf 'Normal-entry candidate: %s\n' "$OUT/normal/boot.img"
sha256sum \
  "$OUT/standalone/skyward-rec.img" \
  "$OUT/temporary/boot.img" \
  "$OUT/normal/boot.img"
```

只进行 RAM 临时启动测试时，使用 standalone 镜像：

```bash
fastboot boot "$OUT/standalone/skyward-rec.img"
```

`standalone/skyward-rec.img` 才是供 `fastboot boot` 使用的独立 REC 镜像。`temporary/boot.img` 和 `normal/boot.img` 是启动流程不同的打包/安装候选，不能互相替代。本仓库的构建脚本不会执行 `fastboot flash`；在对应构建完成必要的实机验证、并确认具备恢复手段之前，不要将候选镜像写入持久分区。

## 测试与校验

完整构建脚本会执行 Venus Python/C 回归测试、相关 Rust crate 测试、installer 和 packer 的严格 Clippy 检查、Android ARM64 Release 构建、capsule 检查及 AVB 验证。

针对 super 镜像代码的快速检查命令：

```bash
cargo test --locked -p skyward-lp -p skyward-vab
cargo test --locked -p skyward-install --lib
cargo clippy --locked -p skyward-install --all-targets -- -D warnings
```

这些都是主机侧检查，不能代替目标手机上的受控实测。尤其需要分别验证启动、触摸、PIN 凭据处理、Data 挂载与卸载，以及 super 写入和读回校验。

## 目录结构

| 路径 | 用途 |
| --- | --- |
| `crates/skyward/` | Recovery 主程序与 Venus 运行时入口 |
| `crates/skyward-install/` | 安装流程与物理 super 镜像处理 |
| `crates/skyward-lp/` | Android Logical Partition Metadata 解析与校验 |
| `crates/skyward-vab/` | Virtual A/B 与逻辑分区集成 |
| `crates/skyward-ui/` | Recovery 用户界面 |
| `crates/skyward-pack/` | 主机端镜像打包工具 |
| `devices/venus.toml` | 小米 11 设备配置 |
| `porting/venus/` | Venus 运行时资源、构建脚本、验证工具和移植记录 |

## 固件、密钥与构建产物

构建过程会从本机目录读取设备提取的固件和运行时文件。未经明确许可，不要提交个人设备 dump、userdata、基带/NV 备份、私钥、签名材料、账号凭据或不可再分发的闭源二进制。构建产物位于 `target/`，该目录已从 Git 跟踪中排除。分享二进制镜像前，必须确认其中每个外部组件的许可证和再分发条件。

## 上游来源与许可证

本项目基于 [Kirk Lin 的 Skyward Recovery](https://github.com/kirklin/skyward)。上游提供 Recovery 基础实现；本仓库单独维护小米 11（`venus`）的设备适配及其专属修改。它是独立开发分支，不代表上游官方分支或官方发布版本。上游通用功能和发行状态请以原项目为准。

本仓库采用 **GPL-3.0-or-later**，详见 [`LICENSE`](LICENSE)。原有上游代码的版权和许可证声明继续适用于其覆盖的代码；第三方固件和二进制组件可能另有许可条件。
