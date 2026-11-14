// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Skyward Venus contributors
#include "backend.h"
#include <android/system/net/netd/1.1/INetd.h>
#include <hidl/HidlTransportSupport.h>
#include <hwbinder/IPCThreadState.h>
#include <fcntl.h>
#include <pthread.h>
#include <sys/file.h>
#include <sys/stat.h>

using android::hardware::Return;
using android::hardware::Void;
using android::hardware::hidl_string;
using android::system::net::netd::V1_1::INetd;
using Status = android::system::net::netd::V1_0::INetd::StatusCode;

static Status result(int error, const char* method) {
    if (error) std::fprintf(stderr, "VENUS_NETD_ERROR method=%s errno=%d\n", method, -error);
    switch (error) {
        case 0: return Status::OK;
        case -EINVAL: return Status::INVALID_ARGUMENTS;
        case -ENONET: return Status::NO_NETWORK;
        case -EEXIST: return Status::ALREADY_EXISTS;
        case -EPERM: case -EACCES: return Status::PERMISSION_DENIED;
        default: return Status::UNKNOWN_ERROR;
    }
}
static bool allowed() {
    uid_t uid = android::hardware::IPCThreadState::self()->getCallingUid();
    return uid == 0 || uid == 1001; // Recovery owner and vendor radio daemon only.
}

class Netd final : public INetd {
    venus_netd::Manager& manager_;
public:
    explicit Netd(venus_netd::Manager& manager) : manager_(manager) {}
    Return<void> createOemNetwork(createOemNetwork_cb callback) override {
        uint64_t handle = 0; uint32_t mark = 0;
        int error = allowed() ? manager_.create(handle, mark) : -EPERM;
        callback(handle, mark, result(error, "create"));
        return Void();
    }
    Return<Status> destroyOemNetwork(uint64_t h) override {
        return result(allowed() ? manager_.destroy(h) : -EPERM, "destroy");
    }
    Return<Status> addInterfaceToOemNetwork(uint64_t h, const hidl_string& name) override {
        return result(allowed() ? manager_.add_interface(h, std::string(name.c_str(), name.size())) : -EPERM, "add-interface");
    }
    Return<Status> removeInterfaceFromOemNetwork(uint64_t h, const hidl_string& name) override {
        return result(allowed() ? manager_.remove_interface(h, std::string(name.c_str(), name.size())) : -EPERM, "remove-interface");
    }
    Return<Status> addRouteToOemNetwork(uint64_t h, const hidl_string& name,
                                      const hidl_string& destination, const hidl_string& gateway) override {
        return result(allowed() ? manager_.route(h, std::string(name.c_str(), name.size()),
            std::string(destination.c_str(), destination.size()), std::string(gateway.c_str(), gateway.size()), true) : -EPERM, "add-route");
    }
    Return<Status> removeRouteFromOemNetwork(uint64_t h, const hidl_string& name,
                                           const hidl_string& destination, const hidl_string& gateway) override {
        return result(allowed() ? manager_.route(h, std::string(name.c_str(), name.size()),
            std::string(destination.c_str(), destination.size()), std::string(gateway.c_str(), gateway.size()), false) : -EPERM, "remove-route");
    }
    Return<Status> setIpForwardEnable(bool) override {
        // This recovery owns no tethering session. Do not claim to set global forwarding.
        return result(allowed() ? -EOPNOTSUPP : -EPERM, "forwarding-unsupported");
    }
    Return<Status> setForwardingBetweenInterfaces(const hidl_string&, const hidl_string&, bool) override {
        return result(allowed() ? -EOPNOTSUPP : -EPERM, "forwarding-between-unsupported");
    }
};

int main(int argc, char**) {
    if (argc != 1 || geteuid() != 0) return 2;
    // Signals must be blocked before Binder creates worker threads. Main remains a
    // synchronous shutdown owner; no allocator or networking in a signal handler.
    sigset_t signals; sigemptyset(&signals); sigaddset(&signals, SIGTERM); sigaddset(&signals, SIGINT);
    if (pthread_sigmask(SIG_BLOCK, &signals, nullptr)) return 3;
    const int lock = open("/tmp/skyward-network-netd.lock", O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    struct stat st{};
    if (lock < 0 || fstat(lock, &st) || !S_ISREG(st.st_mode) || st.st_uid != 0 ||
        (st.st_mode & 0777) != 0600 || flock(lock, LOCK_EX | LOCK_NB)) return 4;
    try {
        venus_netd::LinuxBackend backend;
        venus_netd::Manager manager(backend, "skyward-oem-" + std::to_string(getpid()));
        android::hardware::configureRpcThreadpool(2, false);
        if (INetd::tryGetService("default") != nullptr ||
            android::system::net::netd::V1_0::INetd::tryGetService("default") != nullptr) return 5;
        android::sp<Netd> service = new Netd(manager);
        if (service->registerAsService("default") != android::OK) return 6;
        std::printf("VENUS_NETD_READY pid=%ld\n", static_cast<long>(getpid())); std::fflush(stdout);
        for (;;) {
            int signal = 0;
            if (sigwait(&signals, &signal)) continue;
            int ret = manager.shutdown();
            if (ret) {
                std::fprintf(stderr, "VENUS_NETD_STOP_FAILED pid=%ld errno=%d\n", static_cast<long>(getpid()), -ret);
                // Retain both HIDL owner and receipts. Parent may retry SIGTERM,
                // but must not kill this process while kernel cleanup is unproved.
                continue;
            }
            std::printf("VENUS_NETD_STOPPED pid=%ld\n", static_cast<long>(getpid())); std::fflush(stdout);
            _exit(0); // Binder workers cannot outlive the stack-backed manager.
        }
    } catch (const std::exception& error) {
        std::fprintf(stderr, "VENUS_NETD_FATAL %s\n", error.what());
        return 7;
    }
}
