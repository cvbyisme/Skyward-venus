# venus Gatekeeper 验证失败：RPMB 节点权限修复

## 已确认的证据

失败期间同一条事件链依次为：`CE_GATEKEEPER_VERIFY_BEGIN`、QSEE 进程的 `rpmb_ufs: Unable to open /dev/0:0:0:49476 (error no: 13)`、Gatekeeper 进程的 `rsp_header->status: -1`、`CE_UNLOCK_FAILED`。

QSEE 进程以 uid/gid 1000 (`system`) 运行；节点却是 `root:root 0600`。原厂 `/mnt/skyward-ro/vendor/ueventd.rc` 与固定版本的 venus TWRP `recovery/root/vendor/ueventd.rc` 第 74 行都是：

```text
/dev/0:0:0:49476          0600   system     system
```

TWRP 参考：`sekaiacg/android_device_xiaomi_venus_TWRP`，提交 `0fcc985756e69b68c8d5eef988ca22b1ee1a3186`。AOSP 的 Gatekeeper `ERROR_GENERAL_FAILURE=-1` 不足以单独判定 PIN 错误。旧客户端将所有非成功/非超时的状态翻译为“安全世界不认识该凭据”，现在区分一般失败、限速、不支持和未知状态。

## 本轮修复

新增 `km41-rpmb.c`。它只在临时 venus Recovery 中工作，先验证已安装 Vendor 的精确权限规则及 sysfs 对应的字符设备身份，再把这个节点设置为 `system:system 0600`。不使用宽泛通配符或 `chmod 777`，不改 ueventd、Vendor、Data 或原始密钥文件。

随后派生一个清空补充组、降到 uid/gid 1000 的子进程，验证 `open(O_RDWR)` 后立即 `close()`。测试不对设备执行 read、write 或 ioctl，不提交凭据、不发送 RPMB 命令。真实 Gatekeeper 验证本身可能正常访问安全存储并维护失败计数，不能把凭据验证称为完全无副作用。

实测 `--check` 修复前返回 `errno=13`，`--apply` 后、重复 `--check` 均通过。已运行的 QSEE 私有环境看到的是同一个已修正节点，因此不需要重启服务或手机。

新检查已加入：

- `km41-runtime.sh`：首次部署及复用已有服务均检查。
- `ce-runtime.sh`：准备 Gatekeeper、提示 PIN 之前检查。
- `ce-local.py`：校验、推送新的静态辅助程序。
- `venus_ce.rs`：直接调用设备端入口也会在读取凭据之前拒绝错误节点身份/权限。

## 验证范围

已经对当前 ROM 的 `services.jar` 做定向字节码检查，核对用户 0 的 fake UID、scrypt 与 personalized SHA-512 流程。另用已安装 VNDK30 的原生 `Parcel` 写入公开样本，比对 Rust 的 HIDL verify 编码；该程序不连接 Binder、不调用任何 HAL。公开派生样本使用 Python OpenSSL 独立计算，并加入回归测试。

这些验证不能代替用户真实 PIN 的端到端验证。本轮没有重新提交真实或猜测的 PIN，CE 尚未解锁；Data 仍为原来的保留式只读挂载，卸载问题没有修复。

## 当前 Recovery 中继续

无需重启或重新启动 Boot：

```bash
cd /home/abc/SKYWARD/skyward
python3 porting/venus/ce-local.py --unlock
```

先确认输出含 `VENUS_RPMB_ACCESS_OK uid=1000 gid=1000 mode=0600`，再由用户在本地确认并输入当前锁屏 PIN。不得把 PIN 放在参数、日志或聊天中。程序不自动重试。失败后保留现场和日志，不连续提交、不卸载 Data。

日志及公开对照资料保存在：
`/home/abc/SKYWARD/venus_dump/gk-verify-audit.6zitw67k`

修改前备份：
`/home/abc/SKYWARD/skyward-before-gk-verify.ourfqtup`
