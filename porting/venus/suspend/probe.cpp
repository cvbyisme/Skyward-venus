// SPDX-License-Identifier: Apache-2.0
// Direct HIDL/kernel-wakelock checks. Never requests autosuspend or writes sysfs.
// exercise/client-exit require an otherwise quiet service for exact-set
// assertions. hold observes bridge-wide tokens without claiming attribution.
#include <android/system/suspend/1.0/ISystemSuspend.h>
#include <hidl/HidlTransportSupport.h>
#include <hwbinder/IPCThreadState.h>

#include <cerrno>
#include <chrono>
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <sys/wait.h>
#include <signal.h>
#include <unistd.h>

using android::sp;
using android::system::suspend::V1_0::ISystemSuspend;
using android::system::suspend::V1_0::IWakeLock;
using android::system::suspend::V1_0::WakeLockType;
using Tokens = std::set<std::string>;
using Clock = std::chrono::steady_clock;
static volatile sig_atomic_t hold_stopped = 0;

static void finish_hold(int) { hold_stopped = 1; }

static Tokens active(const std::string& prefix) {
    std::ifstream file("/sys/power/wake_lock");
    if (!file) throw std::runtime_error("cannot read /sys/power/wake_lock");
    Tokens result;
    std::string name;
    while (file >> name) {
        if (name.compare(0, prefix.size(), prefix) == 0) result.insert(name);
    }
    if (file.bad()) throw std::runtime_error("read /sys/power/wake_lock failed");
    return result;
}

static std::string describe(const Tokens& tokens) {
    std::string text;
    for (const auto& token : tokens) text += (text.empty() ? "" : ",") + token;
    return text.empty() ? "<empty>" : text;
}

static void await_exact(const std::string& prefix, const Tokens& expected,
                        const char* phase) {
    const auto deadline = Clock::now() + std::chrono::seconds(3);
    Tokens observed;
    do {
        observed = active(prefix);
        if (observed == expected) {
            std::printf("PASS phase=%s tokens=%s\n", phase, describe(observed).c_str());
            std::fflush(stdout);
            return;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
    } while (Clock::now() < deadline);
    throw std::runtime_error(std::string(phase) + ": expected=" + describe(expected) +
                             " observed=" + describe(observed));
}

static sp<ISystemSuspend> service() {
    android::hardware::configureRpcThreadpool(1, false);
    auto suspend = ISystemSuspend::tryGetService("default");
    if (!suspend) throw std::runtime_error("HIDL suspend service is not registered");
    return suspend;
}

struct Held {
    sp<IWakeLock> lock;
    std::string token;
};

static Held acquire(const sp<ISystemSuspend>& suspend, const std::string& prefix,
                    Tokens& expected, const std::string& debug_name) {
    await_exact(prefix, expected, "before-acquire");
    auto reply = suspend->acquireWakeLock(WakeLockType::PARTIAL, debug_name);
    if (!reply.isOk())
        throw std::runtime_error("acquire transaction: " + reply.description());
    sp<IWakeLock> lock = reply;
    if (!lock) throw std::runtime_error("acquire returned a successful null lock");
    const auto observed = active(prefix);
    Tokens added = observed;
    for (const auto& token : expected) added.erase(token);
    if (added.size() != 1 || observed.size() != expected.size() + 1)
        throw std::runtime_error("acquire did not add exactly one independent kernel token: " +
                                 describe(observed));
    const auto token = *added.begin();
    expected.insert(token);
    std::printf("ACQUIRED token=%s debug_name=%s\n", token.c_str(), debug_name.c_str());
    std::fflush(stdout);
    return {lock, token};
}

static void release(const sp<IWakeLock>& lock) {
    auto reply = lock->release(); // oneway: acceptance alone is not proof of release.
    if (!reply.isOk()) throw std::runtime_error("release transaction: " + reply.description());
    android::hardware::IPCThreadState::self()->flushCommands();
}

static void exercise(const std::string& prefix) {
    auto suspend = service();
    const auto baseline = active(prefix);
    auto expected = baseline;
    const std::string same_name = "skyward probe same name " + std::to_string(getpid());
    // A third, independently owned token proves releasing either equal-name
    // lock does not delete a lock belonging to a different object.
    auto guard = acquire(suspend, prefix, expected, "skyward-probe-guard");
    auto a = acquire(suspend, prefix, expected, same_name);
    auto b = acquire(suspend, prefix, expected, same_name);
    if (a.token == b.token || a.token == guard.token || b.token == guard.token)
        throw std::runtime_error("different acquire calls shared a kernel token");
    release(a.lock);
    expected.erase(a.token);
    await_exact(prefix, expected, "release-first-preserves-second-and-guard");
    release(a.lock);
    // Keep the released object alive and check service responsiveness. Ping
    // is not an ordering fence for an oneway release; observe stability too.
    auto ping = a.lock->ping();
    if (!ping.isOk()) throw std::runtime_error("released-object ping: " + ping.description());
    for (int check = 0; check < 15; ++check) {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        if (active(prefix) != expected)
            throw std::runtime_error("duplicate release changed a different kernel token");
    }
    await_exact(prefix, expected, "duplicate-release-is-idempotent");
    a.lock.clear();
    b.lock.clear();
    android::hardware::IPCThreadState::self()->flushCommands();
    expected.erase(b.token);
    await_exact(prefix, expected, "last-reference-drop-preserves-guard");
    guard.lock.clear();
    android::hardware::IPCThreadState::self()->flushCommands();
    await_exact(prefix, baseline, "all-probe-tokens-gone-baseline-preserved");
}

static void client_exit(const std::string& prefix) {
    // Fork before opening Binder or starting any RPC thread. The parent only
    // observes sysfs and reaps its own child; it never signals another service.
    const auto baseline = active(prefix);
    const pid_t child = fork();
    if (child < 0) throw std::runtime_error("fork failed");
    if (child == 0) {
        alarm(12);
        try {
            auto suspend = service();
            auto expected = baseline;
            const std::string name = "skyward-probe-client-exit-same-name";
            auto a = acquire(suspend, prefix, expected, name);
            auto b = acquire(suspend, prefix, expected, name);
            await_exact(prefix, expected, "client-holds-two-before-exit");
            std::printf("CLIENT_EXIT pid=%d tokens=%s,%s destructors=false\n", getpid(),
                        a.token.c_str(), b.token.c_str());
            std::fflush(stdout);
            _exit(0); // Deliberately test kernel Binder client-death cleanup.
        } catch (const std::exception& error) {
            std::fprintf(stderr, "FAIL client-exit-child: %s\n", error.what());
            _exit(1);
        }
    }
    int status = 0;
    pid_t result;
    do { result = waitpid(child, &status, 0); } while (result < 0 && errno == EINTR);
    if (result != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
        throw std::runtime_error("client-exit child did not finish successfully");
    await_exact(prefix, baseline, "client-process-exit-releases-both-tokens");
}

static sp<IWakeLock> acquire_for_hold(const sp<ISystemSuspend>& suspend,
                                    const std::string& name) {
    auto reply = suspend->acquireWakeLock(WakeLockType::PARTIAL, name);
    if (!reply.isOk())
        throw std::runtime_error("hold acquire transaction: " + reply.description());
    sp<IWakeLock> lock = reply;
    if (!lock) throw std::runtime_error("hold acquire returned a successful null lock");
    return lock;
}

static void hold(const std::string& prefix, long service_pid) {
    struct sigaction action{};
    action.sa_handler = finish_hold;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGTERM, &action, nullptr) != 0)
        throw std::runtime_error("install hold SIGTERM handler failed");
    // Keep SIGALRM's default action: the main 20-second deadline still ends
    // this client even if a Binder transaction never returns.
    auto suspend = service();
    const std::string name = "skyward-probe-hold-same-name-" + std::to_string(getpid());
    auto a = acquire_for_hold(suspend, name);
    auto b = acquire_for_hold(suspend, name);
    const auto observed = active(prefix);
    if (android::hardware::interfacesEqual(a, b) || observed.size() < 2)
        throw std::runtime_error("hold needs two distinct HIDL objects and at least two bridge tokens");
    if (hold_stopped) throw std::runtime_error("hold cancelled before READY");
    // Slot reuse plus concurrent netmgr activity prevents inferring which
    // names belong to these opaque objects. This is deliberately a weaker
    // observation; the outer owner test must verify this entire dead bridge
    // instance's prefix is empty after its supervisor completes cleanup.
    std::printf("VENUS_SUSPEND_PROBE_HOLD_READY pid=%ld service_pid=%ld held_objects=2 "
                "bridge_active_tokens=%zu token_attribution=not_asserted\n",
                static_cast<long>(getpid()), service_pid, observed.size());
    std::fflush(stdout);
    // The parent test may now terminate the exact owned bridge and inspect
    // its supervisor's orphan cleanup. Do not insist that tokens remain, and
    // do not reconnect, release by name, or signal any other process.
    while (!hold_stopped) std::this_thread::sleep_for(std::chrono::milliseconds(50));
    std::printf("VENUS_SUSPEND_PROBE_HOLD_STOP pid=%ld reason=SIGTERM\n",
                static_cast<long>(getpid()));
    std::fflush(stdout);
}

int main(int argc, char** argv) {
    if (argc != 3 || (std::strcmp(argv[2], "exercise") &&
                      std::strcmp(argv[2], "client-exit") && std::strcmp(argv[2], "hold"))) {
        std::fprintf(stderr, "usage: network-suspend-probe SERVICE_PID exercise|client-exit|hold\n");
        return 2;
    }
    char* end = nullptr;
    errno = 0;
    const long pid = std::strtol(argv[1], &end, 10);
    if (errno || !end || *end || pid <= 1 || pid > INT_MAX) {
        std::fprintf(stderr, "invalid service PID\n");
        return 2;
    }
    const std::string prefix = "skyward-suspend-" + std::to_string(pid) + "-";
    // Bound a broken Binder transaction too, not only sysfs observation.
    alarm(20);
    try {
        if (!std::strcmp(argv[2], "client-exit")) client_exit(prefix);
        else if (!std::strcmp(argv[2], "hold")) hold(prefix, pid);
        else exercise(prefix);
        std::printf("PASS mode=%s prefix=%s sysfs_writes=none suspend_requests=none\n",
                    argv[2], prefix.c_str());
        return 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "FAIL: %s\n", error.what());
        return 1;
    }
}
