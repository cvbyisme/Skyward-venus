// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Skyward Venus contributors
#pragma once
#include <vector>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unistd.h>

namespace venus_suspend {
[[noreturn]] inline void fatal(const char* phase, const char* message) noexcept {
    std::fprintf(stderr, "VENUS_SUSPEND_FATAL pid=%ld phase=%s error=%s\n", static_cast<long>(getpid()), phase, message);
    std::fflush(stderr);
    // The legacy libpower caller handles DEAD_OBJECT, but treats an ordinary
    // HIDL exception or successful null handle as success/fatal. A failed
    // backend must therefore kill this endpoint; its supervisor then stops
    // owned clients and clears only this dead instance's kernel tokens.
    _exit(74);
}

class Operations {
  public:
    virtual ~Operations() = default;
    virtual void acquire(const std::string& token) = 0;
    virtual void release(const std::string& token) = 0;
};

class Sysfs final : public Operations {
    int acquire_fd_ = -1;
    int release_fd_ = -1;
    std::mutex io_;
    static int open_file(const char* path) {
        const int fd = open(path, O_WRONLY | O_CLOEXEC | O_NOFOLLOW);
        if (fd < 0) throw std::runtime_error(std::string("open ") + path + ": " + std::strerror(errno));
        return fd;
    }
    void write_token(int fd, const std::string& token) {
        std::lock_guard<std::mutex> guard(io_);
        // sysfs stores one complete request per write. Never retry a partial
        // write as another request containing only a suffix of the token.
        ssize_t count;
        do { count = write(fd, token.data(), token.size()); } while (count < 0 && errno == EINTR);
        if (count < 0) throw std::runtime_error(std::string("write kernel wake lock: ") + std::strerror(errno));
        if (static_cast<size_t>(count) != token.size()) throw std::runtime_error("short kernel wake lock write");
    }
  public:
    Sysfs() {
        acquire_fd_ = open_file("/sys/power/wake_lock");
        try { release_fd_ = open_file("/sys/power/wake_unlock"); }
        catch (...) { close(acquire_fd_); throw; }
    }
    ~Sysfs() override { close(acquire_fd_); close(release_fd_); }
    void acquire(const std::string& token) override { write_token(acquire_fd_, token); }
    void release(const std::string& token) override { write_token(release_fd_, token); }
};

class Tokens {
  public:
    struct Slot { unsigned long long id; std::string name; };
  private:
    const std::string prefix_;
    std::mutex mutex_;
    unsigned long long next_ = 1;
    std::vector<unsigned long long> free_;
  public:
    explicit Tokens(long pid) : prefix_("skyward-suspend-" + std::to_string(pid) + "-") {
        if (pid <= 0) throw std::runtime_error("invalid suspend owner PID");
    }
    const std::string& prefix() const { return prefix_; }
    Slot take() {
        std::lock_guard<std::mutex> guard(mutex_);
        unsigned long long number;
        if (free_.empty()) {
            if (next_ == 0) fatal("token", "suspend token counter exhausted");
            number = next_++;
        } else { number = free_.back(); free_.pop_back(); }
        return {number, prefix_ + std::to_string(number)};
    }
    void give(const Slot& slot) {
        std::lock_guard<std::mutex> guard(mutex_);
        free_.push_back(slot.id);
    }
};

// Each returned Binder object owns one lease, independent of debugName.
// Release marks the lease inactive only after the actual kernel write.
class Lease {
    const std::shared_ptr<Operations> backend_;
    const std::shared_ptr<Tokens> tokens_;
    const Tokens::Slot slot_;
    std::mutex mutex_;
    bool active_ = false;
  public:
    Lease(std::shared_ptr<Operations> backend, std::shared_ptr<Tokens> tokens)
        : backend_(std::move(backend)), tokens_(std::move(tokens)), slot_(tokens_->take()) {
        backend_->acquire(slot_.name);
        active_ = true;
    }
    Lease(const Lease&) = delete;
    Lease& operator=(const Lease&) = delete;
    void release() {
        std::lock_guard<std::mutex> guard(mutex_);
        if (!active_) return;
        backend_->release(slot_.name);
        active_ = false;
        // wake_unlock only relaxes the kernel source: it does not delete
        // its name. Reuse released slots to bound names by peak concurrency.
        // An old released Binder object remains a no-op even after reuse.
        tokens_->give(slot_);
    }
    ~Lease() noexcept {
        try { release(); }
        catch (const std::exception& error) { fatal("destructor-release", error.what()); }
        catch (...) { fatal("destructor-release", "unknown backend failure"); }
    }
};
} // namespace venus_suspend
