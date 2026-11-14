// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Skyward Venus contributors
#pragma once
#include "manager.h"
#include <linux/fib_rules.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <poll.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <functional>

namespace venus_netd {
#ifndef VENUS_NETD_TEST_IPTABLES_DIR
#define VENUS_NETD_TEST_IPTABLES_DIR "/system/bin"
#endif
using Bytes = std::vector<uint8_t>;
struct Message {
    Bytes bytes;
    template<class T> Message(uint16_t type, uint16_t flags, const T& payload) {
        bytes.resize(NLMSG_SPACE(sizeof(T)), 0);
        nlmsghdr h{}; h.nlmsg_len = NLMSG_LENGTH(sizeof(T)); h.nlmsg_type = type;
        h.nlmsg_flags = flags; h.nlmsg_seq = 1;
        std::memcpy(bytes.data(), &h, sizeof(h));
        std::memcpy(bytes.data() + NLMSG_HDRLEN, &payload, sizeof(payload));
    }
    void attr(uint16_t type, const void* value, size_t size) {
        auto* header = reinterpret_cast<nlmsghdr*>(bytes.data());
        size_t old = NLMSG_ALIGN(header->nlmsg_len), full = old + RTA_SPACE(size);
        bytes.resize(full, 0); header = reinterpret_cast<nlmsghdr*>(bytes.data());
        rtattr a{}; a.rta_type = type; a.rta_len = uint16_t(RTA_LENGTH(size));
        std::memcpy(bytes.data() + old, &a, sizeof(a));
        std::memcpy(bytes.data() + old + RTA_LENGTH(0), value, size);
        header->nlmsg_len = uint32_t(full);
    }
    template<class T> void attr(uint16_t type, const T& value) { attr(type, &value, sizeof(value)); }
    void string(uint16_t type, const std::string& value) { attr(type, value.c_str(), value.size() + 1); }
};

inline Message request(const Operation& op, bool add) {
    uint16_t flags = NLM_F_REQUEST | NLM_F_ACK;
    if (add) flags |= NLM_F_CREATE | NLM_F_EXCL;
    if (op.kind == Operation::Rule) {
        fib_rule_hdr rule{}; rule.family = uint8_t(op.family); rule.action = FR_ACT_TO_TBL;
        Message msg(add ? RTM_NEWRULE : RTM_DELRULE, flags, rule);
        msg.attr(FRA_TABLE, op.table); msg.attr(FRA_PRIORITY, op.priority);
        msg.string(FRA_IIFNAME, "lo");
        if (!op.oif.empty()) msg.string(FRA_OIFNAME, op.oif);
        if (op.mask) { msg.attr(FRA_FWMARK, op.value); msg.attr(FRA_FWMASK, op.mask); }
        if (op.root_only) { fib_rule_uid_range range{}; msg.attr(FRA_UID_RANGE, range); }
        return msg;
    }
    rtmsg route{}; route.rtm_family = uint8_t(op.family); route.rtm_dst_len = op.prefix;
    route.rtm_protocol = RTPROT_STATIC; route.rtm_type = RTN_UNICAST;
    route.rtm_scope = op.gateway ? RT_SCOPE_UNIVERSE : RT_SCOPE_LINK;
    Message msg(add ? RTM_NEWROUTE : RTM_DELROUTE, flags, route);
    const size_t size = op.family == AF_INET ? 4 : 16;
    msg.attr(RTA_TABLE, op.table); msg.attr(RTA_DST, op.destination.data(), size);
    msg.attr(RTA_OIF, op.index);
    if (op.gateway) msg.attr(RTA_GATEWAY, op.next.data(), size);
    return msg;
}

class LinuxBackend final : public Backend {
    static int exchange(Message msg, bool dump, const std::function<int(const nlmsghdr*)>& visit) {
        int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK, NETLINK_ROUTE);
        if (fd < 0) return -errno;
        struct Fd { int value; ~Fd() { close(value); } } guard{fd};
        sockaddr_nl local{}; local.nl_family = AF_NETLINK;
        if (bind(fd, reinterpret_cast<sockaddr*>(&local), sizeof(local))) return -errno;
        sockaddr_nl kernel{}; kernel.nl_family = AF_NETLINK;
        ssize_t sent = sendto(fd, msg.bytes.data(), msg.bytes.size(), 0, reinterpret_cast<sockaddr*>(&kernel), sizeof(kernel));
        if (sent < 0) return -errno;
        if (sent != ssize_t(msg.bytes.size())) return -EIO;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
        for (;;) {
            auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now()).count();
            if (remaining <= 0) return -ETIMEDOUT;
            pollfd p{fd, POLLIN, 0}; int ready = poll(&p, 1, int(remaining));
            if (ready < 0) { if (errno == EINTR) continue; return -errno; }
            if (!ready) return -ETIMEDOUT;
            if (p.revents & (POLLERR | POLLHUP | POLLNVAL)) return -EIO;
            alignas(nlmsghdr) uint8_t buffer[65536];
            sockaddr_nl sender{}; iovec io{buffer, sizeof(buffer)}; msghdr received{};
            received.msg_name = &sender; received.msg_namelen = sizeof(sender);
            received.msg_iov = &io; received.msg_iovlen = 1;
            ssize_t bytes = recvmsg(fd, &received, 0);
            if (bytes < 0) { if (errno == EAGAIN || errno == EINTR) continue; return -errno; }
            if (!bytes || (received.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) || sender.nl_pid != 0) return -EBADMSG;
            unsigned size = unsigned(bytes);
            for (auto* h = reinterpret_cast<nlmsghdr*>(buffer); NLMSG_OK(h, size); h = NLMSG_NEXT(h, size)) {
                if (h->nlmsg_seq != 1) return -EBADMSG;
                if (h->nlmsg_flags & NLM_F_DUMP_INTR) return -EINTR;
                if (h->nlmsg_type == NLMSG_ERROR) {
                    if (h->nlmsg_len < NLMSG_LENGTH(sizeof(nlmsgerr))) return -EBADMSG;
                    nlmsgerr e{}; std::memcpy(&e, NLMSG_DATA(h), sizeof(e));
                    if (e.error || !dump) return e.error;
                } else if (h->nlmsg_type == NLMSG_DONE) {
                    if (!dump) return -EBADMSG;
                    if (h->nlmsg_len >= NLMSG_LENGTH(sizeof(int))) {
                        int error = 0; std::memcpy(&error, NLMSG_DATA(h), sizeof(error));
                        if (error) return error;
                    }
                    return 0;
                } else if (int ret = visit(h)) return ret;
            }
            if (size) return -EBADMSG;
        }
    }
    using Attributes = std::map<unsigned, Bytes>;
    static int attributes(const nlmsghdr* h, size_t payload, Attributes& attributes) {
        if (h->nlmsg_len < NLMSG_LENGTH(payload)) return -EBADMSG;
        unsigned size = h->nlmsg_len - NLMSG_LENGTH(payload);
        auto* at = reinterpret_cast<const rtattr*>(reinterpret_cast<const uint8_t*>(NLMSG_DATA(h)) + NLMSG_ALIGN(payload));
        for (; RTA_OK(at, size); at = RTA_NEXT(at, size)) {
            if (attributes.count(at->rta_type)) return -EBADMSG;
            const auto* p = static_cast<const uint8_t*>(RTA_DATA(at));
            attributes[at->rta_type] = Bytes(p, p + RTA_PAYLOAD(at));
        }
        return size ? -EBADMSG : 0;
    }
    template<class T> static bool equal(const Attributes& a, unsigned key, const T& value) {
        auto at = a.find(key);
        return at != a.end() && at->second.size() == sizeof(T) && !std::memcmp(at->second.data(), &value, sizeof(T));
    }
    static bool equal_string(const Attributes& a, unsigned key, const std::string& value) {
        auto at = a.find(key);
        return at != a.end() && at->second.size() == value.size() + 1 && !std::memcmp(at->second.data(), value.c_str(), value.size() + 1);
    }
    static unsigned table(const nlmsghdr* h, const Attributes& a, bool rule) {
        auto at = a.find(rule ? unsigned(FRA_TABLE) : unsigned(RTA_TABLE));
        if (at != a.end() && at->second.size() == 4) { unsigned v; std::memcpy(&v, at->second.data(), 4); return v; }
        return rule ? static_cast<const fib_rule_hdr*>(NLMSG_DATA(h))->table : static_cast<const rtmsg*>(NLMSG_DATA(h))->rtm_table;
    }
    static bool matches(const nlmsghdr* h, const Attributes& a, const Operation& op) {
        const bool rule = op.kind == Operation::Rule;
        if (table(h, a, rule) != op.table) return false;
        if (rule) {
            auto* r = static_cast<const fib_rule_hdr*>(NLMSG_DATA(h));
            if (r->family != op.family || r->action != FR_ACT_TO_TBL || r->src_len || r->dst_len || r->tos || (r->flags & FIB_RULE_INVERT)) return false;
            if (!equal(a, FRA_PRIORITY, op.priority) || !equal_string(a, FRA_IIFNAME, "lo")) return false;
            if (op.mask ? (!equal(a, FRA_FWMARK, op.value) || !equal(a, FRA_FWMASK, op.mask)) : (a.count(FRA_FWMARK) || a.count(FRA_FWMASK))) return false;
            if (op.oif.empty() ? bool(a.count(FRA_OIFNAME)) : !equal_string(a, FRA_OIFNAME, op.oif)) return false;
            fib_rule_uid_range range{};
            if (op.root_only ? !equal(a, FRA_UID_RANGE, range) : bool(a.count(FRA_UID_RANGE))) return false;
            // Do not delete a rule carrying selectors that this bridge did not create.
            for (const auto& item : a) switch (item.first) {
                case FRA_TABLE: case FRA_PRIORITY: case FRA_IIFNAME: case FRA_OIFNAME:
                case FRA_FWMARK: case FRA_FWMASK: case FRA_UID_RANGE: case FRA_PROTOCOL: break;
                case FRA_SUPPRESS_PREFIXLEN: case FRA_SUPPRESS_IFGROUP:
                    if (!equal(a, item.first, UINT32_MAX)) return false;
                    break;
                default: return false;
            }
            return true;
        }
        auto* r = static_cast<const rtmsg*>(NLMSG_DATA(h));
        if (r->rtm_family != op.family || r->rtm_dst_len != op.prefix || r->rtm_src_len || r->rtm_tos ||
            r->rtm_protocol != RTPROT_STATIC || r->rtm_type != RTN_UNICAST ||
            // Linux normalizes IPv6 direct routes to UNIVERSE (unlike IPv4).
            r->rtm_scope != (op.gateway || op.family == AF_INET6 ? RT_SCOPE_UNIVERSE : RT_SCOPE_LINK) || !equal(a, RTA_OIF, op.index)) return false;
        size_t size = op.family == AF_INET ? 4 : 16;
        auto dst = a.find(RTA_DST);
        if (dst == a.end()) { if (op.prefix != 0) return false; }
        else if (dst->second.size() != size || std::memcmp(dst->second.data(), op.destination.data(), size)) return false;
        auto gw = a.find(RTA_GATEWAY);
        if (op.gateway ? (gw == a.end() || gw->second.size() != size || std::memcmp(gw->second.data(), op.next.data(), size)) : gw != a.end()) return false;
        for (const auto& item : a) switch (item.first) {
            case RTA_TABLE: case RTA_DST: case RTA_GATEWAY: case RTA_OIF: case RTA_CACHEINFO: break;
            // IPv6 assigns its default priority 1024 even if the request omitted it.
            case RTA_PRIORITY: if (op.family != AF_INET6 || !equal(a, RTA_PRIORITY, uint32_t(1024))) return false; break;
            case RTA_PREF: if (!equal(a, RTA_PREF, uint8_t(0))) return false; break;
            default: return false;
        }
        return true;
    }
    static int dump(int family, bool rules, const std::function<int(const nlmsghdr*, const Attributes&)>& visitor) {
        fib_rule_hdr rule{}; rule.family = uint8_t(family);
        rtmsg route{}; route.rtm_family = uint8_t(family);
        Message message = rules ? Message(RTM_GETRULE, NLM_F_REQUEST | NLM_F_DUMP, rule) :
            Message(RTM_GETROUTE, NLM_F_REQUEST | NLM_F_DUMP, route);
        return exchange(std::move(message), true, [&](const nlmsghdr* h) {
            if (h->nlmsg_type != (rules ? RTM_NEWRULE : RTM_NEWROUTE)) return -EBADMSG;
            Attributes a;
            if (int ret = attributes(h, rules ? sizeof(fib_rule_hdr) : sizeof(rtmsg), a)) return ret;
            return visitor(h, a);
        });
    }
    static int iptables(const Operation& op, const char* action) {
        const char* tool = op.family == AF_INET ? VENUS_NETD_TEST_IPTABLES_DIR "/iptables" : VENUS_NETD_TEST_IPTABLES_DIR "/ip6tables";
        char value[40]; std::snprintf(value, sizeof(value), "0x%x/0x%x", op.value, op.mask);
        // Exact argv, never a shell. The comment is an ownership token, not an interface wildcard.
        std::vector<std::string> args{tool, "-w", "1", "-t", "mangle", action, "INPUT", "-i", op.iface,
            "-m", "comment", "--comment", op.tag, "-j", "MARK", "--set-mark", value};
        std::vector<char*> argv;
        for (auto& arg : args) argv.push_back(const_cast<char*>(arg.c_str()));
        argv.push_back(nullptr);
        pid_t child = fork(); if (child < 0) return -errno;
        if (!child) { execv(tool, argv.data()); _exit(127); }
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
        int status = 0;
        for (;;) {
            pid_t done = waitpid(child, &status, WNOHANG);
            if (done == child) return WIFEXITED(status) ? WEXITSTATUS(status) : -EIO;
            if (done < 0 && errno != EINTR) return -errno;
            if (std::chrono::steady_clock::now() >= deadline) {
                kill(child, SIGKILL);
                while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
                return -ETIMEDOUT;
            }
            poll(nullptr, 0, 10);
        }
    }
public:
    unsigned index(const std::string& name) override { return if_nametoindex(name.c_str()); }
    int unused_table(unsigned target) override {
        bool used = false;
        for (int family : {AF_INET, AF_INET6}) for (bool rules : {false, true}) {
            int ret = dump(family, rules, [&](const nlmsghdr* h, const Attributes& a) {
                if (table(h, a, rules) == target) used = true;
                return 0;
            });
            if (ret) return ret;
        }
        return used ? -EEXIST : 0;
    }
    int exists(const Operation& op) override {
        if (op.kind == Operation::Mark) {
            int ret = iptables(op, "-C");
            return ret == 0 ? 1 : ret == 1 ? 0 : ret < 0 ? ret : -EIO;
        }
        unsigned count = 0;
        int ret = dump(op.family, op.kind == Operation::Rule, [&](const nlmsghdr* h, const Attributes& a) {
            if (matches(h, a, op)) ++count;
            return 0;
        });
        return ret ? ret : count > 1 ? -EUCLEAN : int(count);
    }
    int add(const Operation& op) override {
        int ret = op.kind == Operation::Mark ? iptables(op, "-A") : exchange(request(op, true), false, [](const nlmsghdr*) { return -EBADMSG; });
        return ret <= 0 ? ret : -EIO;
    }
    int remove(const Operation& op) override {
        int ret = op.kind == Operation::Mark ? iptables(op, "-D") : exchange(request(op, false), false, [](const nlmsghdr*) { return -EBADMSG; });
        return ret <= 0 ? ret : -EIO;
    }
};
} // namespace venus_netd
