// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Skyward Venus contributors
#pragma once

#include <arpa/inet.h>
#include <net/if.h>
#include <algorithm>
#include <array>
#include <cerrno>
#include <cstdint>
#include <map>
#include <mutex>
#include <string>
#include <vector>

namespace venus_netd {
constexpr uint32_t kHandleMagic = 0xcafed00d;
constexpr uint32_t kPermission = 0xc0000; // AOSP PERMISSION_SYSTEM=3, bits 18..19.
inline uint64_t handle(unsigned id) { return (uint64_t(id) << 32) | kHandleMagic; }
inline uint32_t mark(unsigned id) { return id | 0xf0000; }
inline unsigned netid(uint64_t h) {
    unsigned id = unsigned(h >> 32);
    return uint32_t(h) == kHandleMagic && id >= 1 && id <= 50 ? id : 0;
}
inline bool valid_name(const std::string& s) {
    if (s.empty() || s.size() >= IFNAMSIZ || s == "lo" || s == "." || s == "..") return false;
    return std::all_of(s.begin(), s.end(), [](unsigned char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
               (c >= '0' && c <= '9') || c == '_' || c == '-' || c == '.';
    });
}

struct Operation {
    enum Kind { Rule, Route, Mark } kind = Rule;
    int family = AF_INET;
    unsigned table = 0, index = 0, id = 0;
    uint32_t priority = 0, value = 0, mask = 0;
    bool root_only = false, gateway = false;
    uint8_t prefix = 0;
    std::string iface, oif, tag;
    std::array<uint8_t, 16> destination{}, next{};
    bool operator==(const Operation& b) const {
        return kind == b.kind && family == b.family && table == b.table && index == b.index &&
            id == b.id && priority == b.priority && value == b.value && mask == b.mask &&
            root_only == b.root_only && gateway == b.gateway && prefix == b.prefix &&
            iface == b.iface && oif == b.oif && tag == b.tag &&
            destination == b.destination && next == b.next;
    }
};

inline int parse_route(const std::string& destination, const std::string& next, Operation& op) {
    if (destination.size() > 64 || next.size() > 46 || destination.find('\0') != std::string::npos ||
        next.find('\0') != std::string::npos) return -EINVAL;
    auto slash = destination.find('/');
    if (slash == std::string::npos || destination.find('/', slash + 1) != std::string::npos) return -EINVAL;
    std::string address = destination.substr(0, slash), bits = destination.substr(slash + 1);
    if (bits.empty() || bits.size() > 3 || !std::all_of(bits.begin(), bits.end(), [](char c) { return c >= '0' && c <= '9'; })) return -EINVAL;
    unsigned prefix = 0;
    for (char c : bits) prefix = prefix * 10 + unsigned(c - '0');
    op.family = address.find(':') == std::string::npos ? AF_INET : AF_INET6;
    const unsigned size = op.family == AF_INET ? 4 : 16;
    if (prefix > size * 8 || inet_pton(op.family, address.c_str(), op.destination.data()) != 1) return -EINVAL;
    // Do not silently change the caller's route into a different prefix.
    for (unsigned bit = prefix; bit < size * 8; ++bit)
        if (op.destination[bit / 8] & (1u << (7 - bit % 8))) return -EINVAL;
    op.prefix = uint8_t(prefix);
    op.gateway = !next.empty();
    if (op.gateway && inet_pton(op.family, next.c_str(), op.next.data()) != 1) return -EINVAL;
    return 0;
}

inline std::vector<Operation> interface_operations(unsigned id, const std::string& iface,
                                                   unsigned index, const std::string& tag) {
    std::vector<Operation> result;
    for (int family : {AF_INET, AF_INET6}) {
        Operation op;
        op.kind = Operation::Mark; op.family = family; op.id = id;
        op.table = 1000 + index; op.index = index; op.iface = iface; op.tag = tag;
        op.value = mark(id); op.mask = 0xffefffff; // Preserve AOSP uidBillingDone.
        result.push_back(op);
        op.kind = Operation::Rule; op.value = id | 0xd0000; op.mask = 0xdffff;
        op.priority = 13000; result.push_back(op);
        op.value = 0; op.mask = 0; op.priority = 10500;
        op.oif = iface; op.root_only = true; result.push_back(op);
        op.value = kPermission; op.mask = kPermission; op.priority = 14000;
        op.root_only = false; result.push_back(op);
    }
    return result;
}

struct Backend {
    virtual ~Backend() = default;
    virtual unsigned index(const std::string&) = 0;
    virtual int unused_table(unsigned) = 0; // 0 only after a complete IPv4+IPv6 dump.
    virtual int exists(const Operation&) = 0; // 0 absent, 1 exact unique object, negative error.
    virtual int add(const Operation&) = 0;
    virtual int remove(const Operation&) = 0;
};

class Manager {
    struct Interface { unsigned index = 0; bool ready = false; std::vector<Operation> owned; };
    struct Network { std::map<std::string, Interface> interfaces; };
    Backend& backend_;
    const std::string tag_;
    std::map<unsigned, Network> networks_;
    std::mutex mutex_;
    bool closing_ = false;

    int lookup(uint64_t h, Network*& network) {
        unsigned id = netid(h);
        if (!id) return -EINVAL;
        auto at = networks_.find(id);
        if (at == networks_.end()) return -ENONET;
        network = &at->second;
        return 0;
    }
    // Keep failed receipts in place. Never delete an object not acquired by this owner.
    int clear(Interface& iface) {
        int first = 0;
        for (size_t i = iface.owned.size(); i != 0; --i) {
            const Operation& op = iface.owned[i - 1];
            int state = backend_.exists(op);
            int ret = state < 0 ? state : state == 0 ? 0 : backend_.remove(op);
            if (!ret) {
                state = backend_.exists(op);
                ret = state < 0 ? state : state == 0 ? 0 : -EBUSY;
            }
            if (ret) { if (!first) first = ret; }
            else iface.owned.erase(iface.owned.begin() + (i - 1));
        }
        iface.ready = false;
        return first;
    }
    int acquire(Interface& iface, const Operation& op) {
        int state = backend_.exists(op);
        if (state != 0) return state < 0 ? state : -EEXIST;
        // Record before the syscall: a lost ACK can still mean a successful mutation.
        iface.owned.push_back(op);
        int ret = backend_.add(op);
        // A definite EEXIST belongs to a concurrent/foreign writer, not to this request.
        if (ret == -EEXIST) { iface.owned.pop_back(); return ret; }
        int after = backend_.exists(op);
        if (!ret && after != 1) ret = after < 0 ? after : -EIO;
        if (ret && after == 0) iface.owned.pop_back();
        return ret;
    }
public:
    Manager(Backend& backend, std::string tag) : backend_(backend), tag_(std::move(tag)) {}
    int create(uint64_t& h, uint32_t& fwmark) {
        std::lock_guard<std::mutex> lock(mutex_);
        h = 0; fwmark = 0;
        if (closing_) return -ESHUTDOWN;
        for (unsigned id = 1; id <= 50; ++id) if (!networks_.count(id)) {
            networks_.emplace(id, Network{});
            h = handle(id); fwmark = mark(id);
            return 0; // AOSP creates an empty in-memory PhysicalNetwork here as well.
        }
        return -ENONET;
    }
    int add_interface(uint64_t h, const std::string& name) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (closing_) return -ESHUTDOWN;
        Network* network;
        if (int ret = lookup(h, network)) return ret;
        if (!valid_name(name)) return -EINVAL;
        auto old = network->interfaces.find(name);
        if (old != network->interfaces.end()) {
            if (!old->second.ready) return -EBUSY;
            if (backend_.index(name) != old->second.index) return -ESTALE;
            for (const auto& op : old->second.owned) if (backend_.exists(op) != 1) return -EIO;
            return 0;
        }
        for (const auto& n : networks_) if (n.second.interfaces.count(name)) return -EBUSY;
        unsigned index = backend_.index(name);
        if (!index || index > UINT32_MAX - 1000) return -ENODEV;
        if (int ret = backend_.unused_table(index + 1000)) return ret;
        auto& iface = network->interfaces[name]; iface.index = index;
        for (const auto& op : interface_operations(netid(h), name, index, tag_ + "-" + std::to_string(netid(h)))) {
            if (int ret = acquire(iface, op)) {
                int cleanup = clear(iface);
                if (iface.owned.empty()) network->interfaces.erase(name);
                return cleanup ? cleanup : ret;
            }
        }
        if (backend_.index(name) != index) {
            int cleanup = clear(iface);
            if (iface.owned.empty()) network->interfaces.erase(name);
            return cleanup ? cleanup : -ESTALE;
        }
        iface.ready = true;
        return 0;
    }
    int remove_interface(uint64_t h, const std::string& name) {
        std::lock_guard<std::mutex> lock(mutex_);
        Network* network;
        if (int ret = lookup(h, network)) return ret;
        if (!valid_name(name)) return -EINVAL;
        auto at = network->interfaces.find(name);
        if (at == network->interfaces.end()) return 0;
        int ret = clear(at->second);
        if (at->second.owned.empty()) network->interfaces.erase(at);
        return ret;
    }
    int route(uint64_t h, const std::string& name, const std::string& destination,
              const std::string& gateway, bool add) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (closing_ && add) return -ESHUTDOWN;
        Network* network;
        if (int ret = lookup(h, network)) return ret;
        if (!valid_name(name)) return -EINVAL;
        auto at = network->interfaces.find(name);
        if (at == network->interfaces.end() || !at->second.ready) return -ENODEV;
        Interface& iface = at->second;
        if (backend_.index(name) != iface.index) return -ESTALE;
        Operation op; op.kind = Operation::Route; op.id = netid(h); op.iface = name;
        op.index = iface.index; op.table = iface.index + 1000;
        if (int ret = parse_route(destination, gateway, op)) return ret;
        auto found = std::find(iface.owned.begin(), iface.owned.end(), op);
        if (add) {
            if (found != iface.owned.end()) return backend_.exists(op) == 1 ? 0 : -EIO;
            return acquire(iface, op);
        }
        if (found == iface.owned.end()) return -ENOENT;
        int state = backend_.exists(op);
        int ret = state < 0 ? state : state ? backend_.remove(op) : 0;
        if (!ret && (state = backend_.exists(op)) != 0) ret = state < 0 ? state : -EBUSY;
        if (!ret) iface.owned.erase(found);
        return ret;
    }
    int destroy(uint64_t h) {
        std::lock_guard<std::mutex> lock(mutex_);
        Network* network;
        if (int ret = lookup(h, network)) return ret;
        int first = 0;
        for (auto at = network->interfaces.begin(); at != network->interfaces.end();) {
            int ret = clear(at->second); if (ret && !first) first = ret;
            if (at->second.owned.empty()) at = network->interfaces.erase(at); else ++at;
        }
        if (network->interfaces.empty()) networks_.erase(netid(h));
        return first;
    }
    int shutdown() {
        std::lock_guard<std::mutex> lock(mutex_);
        closing_ = true;
        int first = 0;
        for (auto n = networks_.begin(); n != networks_.end();) {
            for (auto at = n->second.interfaces.begin(); at != n->second.interfaces.end();) {
                int ret = clear(at->second); if (ret && !first) first = ret;
                if (at->second.owned.empty()) at = n->second.interfaces.erase(at); else ++at;
            }
            if (n->second.interfaces.empty()) n = networks_.erase(n); else ++n;
        }
        return first;
    }
};
} // namespace venus_netd
