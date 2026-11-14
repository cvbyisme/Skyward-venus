// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Skyward Venus contributors
// Implements the AOSP android.system.suspend@1.0 contract against the real
// kernel wake_lock/wake_unlock interface. Does not drive autosuspend.
#include "locks.h"
#include <android/system/suspend/1.0/ISystemSuspend.h>
#include <android/system/suspend/1.0/IWakeLock.h>
#include <hidl/HidlTransportSupport.h>
#include <sys/file.h>
#include <sys/stat.h>

using android::sp;
using android::hardware::Return;
using android::hardware::Void;
using android::hardware::hidl_string;
using android::system::suspend::V1_0::ISystemSuspend;
using android::system::suspend::V1_0::IWakeLock;
using android::system::suspend::V1_0::WakeLockType;
using venus_suspend::fatal;

class WakeLock final : public IWakeLock {
    venus_suspend::Lease lease_;
  public:
    WakeLock(std::shared_ptr<venus_suspend::Operations> backend, std::shared_ptr<venus_suspend::Tokens> tokens)
        : lease_(std::move(backend), std::move(tokens)) {}
    Return<void> release() override {
        try { lease_.release(); }
        catch (const std::exception& error) { fatal("release", error.what()); }
        catch (...) { fatal("release", "unknown backend failure"); }
        return Void();
    }
};

class SystemSuspend final : public ISystemSuspend {
    const std::shared_ptr<venus_suspend::Operations> backend_;
    const std::shared_ptr<venus_suspend::Tokens> tokens_ = std::make_shared<venus_suspend::Tokens>(static_cast<long>(getpid()));
  public:
    explicit SystemSuspend(std::shared_ptr<venus_suspend::Operations> backend) : backend_(std::move(backend)) {}
    const std::string& prefix() const { return tokens_->prefix(); }
    Return<sp<IWakeLock>> acquireWakeLock(WakeLockType type, const hidl_string&) override {
        // The HIDL contract requires both defined types to block CPU suspend;
        // FULL does not require this HAL to control the panel.
        if (type != WakeLockType::PARTIAL && type != WakeLockType::FULL)
            return android::hardware::Status::fromExceptionCode(android::hardware::Status::EX_ILLEGAL_ARGUMENT, "unsupported wake lock type");
        try {
            sp<IWakeLock> lock = new WakeLock(backend_, tokens_);
            return lock;
        } catch (const std::exception& error) { fatal("acquire", error.what()); }
        catch (...) { fatal("acquire", "unknown backend failure"); }
    }
};

int main(int argc, char**) {
    if (argc != 1 || geteuid() != 0) fatal("startup", "no arguments and root required");
    // The launcher restricts this helper to the verified Recovery RAM root.
    // Refuse multiple owners instead of replacing an existing service.
    const int lock = open("/tmp/skyward-network-suspend.lock", O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    struct stat st{};
    if (lock < 0 || fstat(lock, &st) || !S_ISREG(st.st_mode) || st.st_uid != 0 ||
        (st.st_mode & 0777) != 0600 || flock(lock, LOCK_EX | LOCK_NB)) fatal("startup", "cannot exclusively own suspend service lock");
    try {
        auto backend = std::make_shared<venus_suspend::Sysfs>();
        android::hardware::configureRpcThreadpool(2, true);
        if (ISystemSuspend::tryGetService("default") != nullptr) fatal("startup", "SystemSuspend/default already has an owner");
        sp<SystemSuspend> service = new SystemSuspend(std::move(backend));
        if (service->registerAsService("default") != android::OK) fatal("register", "cannot register SystemSuspend/default");
        std::printf("VENUS_SUSPEND_READY pid=%ld prefix=%s\n", static_cast<long>(getpid()), service->prefix().c_str());
        std::fflush(stdout);
        android::hardware::joinRpcThreadpool();
        fatal("binder", "RPC threadpool unexpectedly returned");
    } catch (const std::exception& error) { fatal("startup", error.what()); }
    catch (...) { fatal("startup", "unknown backend failure"); }
}
