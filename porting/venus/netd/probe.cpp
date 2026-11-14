// SPDX-License-Identifier: Apache-2.0
// Direct HIDL lifecycle checks. Only creates/destroys this probe's empty OEM
// networks; route observation is RTM_GETROUTE, never a configuration request.
#ifndef NETD_PROBE_ROUTES_ONLY
#include <android/system/net/netd/1.1/INetd.h>
#include <hidl/HidlTransportSupport.h>
#endif

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <sys/socket.h>
#include <unistd.h>
#include <vector>

using Routes = std::set<std::string>;

struct Socket {
    int fd;
    explicit Socket(int value) : fd(value) {
        if (fd < 0) throw std::runtime_error(std::string("netlink socket: ") + strerror(errno));
    }
    ~Socket() { close(fd); }
    Socket(const Socket&) = delete;
    Socket& operator=(const Socket&) = delete;
};

static void require(bool yes, const std::string& why) {
    if (!yes) throw std::runtime_error(why);
}

static std::string route_key(const rtmsg& route, const rtattr* attr, int bytes) {
    std::vector<std::string> attrs;
    for (; RTA_OK(attr, bytes); attr = RTA_NEXT(attr, bytes)) {
        // Counters, use timestamps and expiry countdowns are not configuration.
        if (attr->rta_type == RTA_CACHEINFO || attr->rta_type == RTA_EXPIRES) continue;
        std::string value = std::to_string(attr->rta_type) + "=";
        const auto* payload = static_cast<const unsigned char*>(RTA_DATA(attr));
        for (unsigned n = 0; n < RTA_PAYLOAD(attr); ++n) {
            char hex[3];
            snprintf(hex, sizeof(hex), "%02x", payload[n]);
            value += hex;
        }
        attrs.push_back(std::move(value));
    }
    require(bytes == 0, "truncated route attributes");
    std::sort(attrs.begin(), attrs.end());
    std::ostringstream key;
    key << unsigned(route.rtm_family) << '/' << unsigned(route.rtm_dst_len) << '/'
        << unsigned(route.rtm_src_len) << '/' << unsigned(route.rtm_tos) << '/'
        << unsigned(route.rtm_table) << '/' << unsigned(route.rtm_protocol) << '/'
        << unsigned(route.rtm_scope) << '/' << unsigned(route.rtm_type) << '/'
        << route.rtm_flags;
    for (const auto& item : attrs) key << ' ' << item;
    return key.str();
}

static Routes defaults(unsigned char family) {
    Socket socket_fd(socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE));
    sockaddr_nl local{};
    local.nl_family = AF_NETLINK;
    require(bind(socket_fd.fd, reinterpret_cast<sockaddr*>(&local), sizeof(local)) == 0,
            "bind route observation socket");
    timeval timeout{3, 0};
    require(setsockopt(socket_fd.fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) == 0,
            "set route observation timeout");
    struct Request { nlmsghdr header; rtmsg route; } request{};
    request.header.nlmsg_len = NLMSG_LENGTH(sizeof(rtmsg));
    request.header.nlmsg_type = RTM_GETROUTE;
    request.header.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    request.header.nlmsg_seq = 1;
    request.route.rtm_family = family;
    sockaddr_nl kernel{};
    kernel.nl_family = AF_NETLINK;
    require(sendto(socket_fd.fd, &request, request.header.nlmsg_len, 0,
                   reinterpret_cast<sockaddr*>(&kernel), sizeof(kernel)) == request.header.nlmsg_len,
            "request route dump");
    Routes routes;
    for (;;) {
        alignas(nlmsghdr) unsigned char data[65536];
        sockaddr_nl sender{};
        iovec iov{data, sizeof(data)};
        msghdr message{};
        message.msg_name = &sender;
        message.msg_namelen = sizeof(sender);
        message.msg_iov = &iov;
        message.msg_iovlen = 1;
        ssize_t count;
        do { count = recvmsg(socket_fd.fd, &message, 0); } while (count < 0 && errno == EINTR);
        require(count > 0, std::string("receive route dump: ") + strerror(errno));
        require(!(message.msg_flags & MSG_TRUNC) && sender.nl_pid == 0,
                "truncated or non-kernel route dump");
        unsigned left = static_cast<unsigned>(count);
        for (const auto* header = reinterpret_cast<const nlmsghdr*>(data);
             NLMSG_OK(header, left); header = NLMSG_NEXT(header, left)) {
            require(header->nlmsg_seq == 1, "unexpected route dump sequence");
            require(!(header->nlmsg_flags & NLM_F_DUMP_INTR), "route dump interrupted by configuration change");
            if (header->nlmsg_type == NLMSG_DONE) {
                if (NLMSG_PAYLOAD(header, 0) >= sizeof(int)) {
                    int error;
                    memcpy(&error, NLMSG_DATA(header), sizeof(error));
                    require(error == 0, "route dump completion error " + std::to_string(error));
                }
                return routes;
            }
            if (header->nlmsg_type == NLMSG_ERROR) {
                require(NLMSG_PAYLOAD(header, 0) >= sizeof(nlmsgerr), "short route dump error");
                const auto* error = static_cast<const nlmsgerr*>(NLMSG_DATA(header));
                require(error->error == 0, "route dump error " + std::to_string(error->error));
                continue;
            }
            if (header->nlmsg_type != RTM_NEWROUTE) continue;
            require(NLMSG_PAYLOAD(header, 0) >= sizeof(rtmsg), "short route entry");
            const auto* route = static_cast<const rtmsg*>(NLMSG_DATA(header));
            if (route->rtm_family != family || route->rtm_dst_len != 0) continue;
            routes.insert(route_key(*route, RTM_RTA(route), RTM_PAYLOAD(header)));
        }
        require(left == 0, "malformed route dump message");
    }
}

static Routes defaults() {
    auto all = defaults(AF_INET);
    const auto v6 = defaults(AF_INET6);
    all.insert(v6.begin(), v6.end());
    return all;
}

static void unchanged(const Routes& before, const char* phase) {
    const auto now = defaults();
    if (now != before) {
        for (const auto& row : before) if (!now.count(row)) fprintf(stderr, "REMOVED %s\n", row.c_str());
        for (const auto& row : now) if (!before.count(row)) fprintf(stderr, "ADDED %s\n", row.c_str());
        throw std::runtime_error(std::string(phase) + ": default route set changed; concurrent changes also fail this check");
    }
    printf("PASS phase=%s default_routes=%zu unchanged=true\n", phase, now.size());
    fflush(stdout);
}

#ifndef NETD_PROBE_ROUTES_ONLY
using android::sp;
using android::system::net::netd::V1_1::INetd;
using Status = android::system::net::netd::V1_0::INetd::StatusCode;

static Status response(android::hardware::Return<Status>&& reply, const char* phase) {
    require(reply.isOk(), std::string(phase) + " transaction: " + reply.description());
    return reply;
}

struct Network {
    sp<INetd> netd;
    uint64_t handle = 0;
    bool held = false;
    explicit Network(sp<INetd> service) : netd(std::move(service)) {}
    ~Network() {
        if (!held) return;
        auto reply = netd->destroyOemNetwork(handle);
        if (!reply.isOk()) fprintf(stderr, "CLEANUP_FAILED handle=0x%llx transaction=%s\n",
                                  static_cast<unsigned long long>(handle), reply.description().c_str());
        else if (static_cast<Status>(reply) != Status::OK)
            fprintf(stderr, "CLEANUP_FAILED handle=0x%llx status=%d\n",
                    static_cast<unsigned long long>(handle), static_cast<int>(static_cast<Status>(reply)));
    }
    void create() {
        unsigned callbacks = 0;
        uint32_t mark = 0;
        Status status = Status::UNKNOWN_ERROR;
        auto reply = netd->createOemNetwork([&](uint64_t h, uint32_t m, Status s) {
            ++callbacks;
            handle = h;
            mark = m;
            status = s;
            held = s == Status::OK;
        });
        require(reply.isOk(), "create transaction: " + reply.description());
        require(callbacks == 1 && status == Status::OK, "create did not return one successful result");
        const uint64_t id = handle >> 32;
        require(id >= 1 && id <= 50 && static_cast<uint32_t>(handle) == 0xcafed00d,
                "create returned invalid AOSP OEM handle");
        require(mark == (0xf0000U | id), "create returned incorrect AOSP SYSTEM network mark");
        printf("CREATED handle=0x%llx mark=0x%x pid=%d\n", static_cast<unsigned long long>(handle), mark, getpid());
        fflush(stdout);
    }
    void destroy() {
        require(response(netd->destroyOemNetwork(handle), "destroy") == Status::OK, "destroy failed");
        held = false;
    }
};

static void lifecycle(const sp<INetd>& netd) {
    const auto before = defaults();
    Network first(netd), second(netd);
    first.create();
    second.create();
    require(first.handle != second.handle, "independent creates returned the same live network");
    unchanged(before, "two-empty-networks-created");
    require(response(netd->destroyOemNetwork(first.handle ^ 1U), "malformed handle") == Status::INVALID_ARGUMENTS,
            "malformed handle was not rejected");
    require(response(netd->destroyOemNetwork(0), "zero handle") == Status::INVALID_ARGUMENTS,
            "zero handle was not rejected");
    unchanged(before, "invalid-handles-rejected");
    first.destroy();
    require(response(netd->destroyOemNetwork(first.handle), "stale handle") == Status::NO_NETWORK,
            "destroyed network was still accepted");
    second.destroy();
    unchanged(before, "both-networks-destroyed");
}
#endif

int main(int argc, char** argv) {
    alarm(20); // Also bound HIDL transactions; the caller should impose its own deadline.
    try {
#ifdef NETD_PROBE_ROUTES_ONLY
        (void) argc;
        (void) argv;
        const auto before = defaults();
        unchanged(before, "host-read-only-route-dump");
#else
        require(argc == 1 || (argc == 2 && std::string(argv[1]) == "--lifecycle"),
                "usage: network-netd-probe [--lifecycle]");
        android::hardware::configureRpcThreadpool(1, false);
        auto netd = INetd::tryGetService("default");
        require(netd != nullptr, "HIDL Netd 1.1/default is not registered");
        auto ping = netd->ping();
        require(ping.isOk(), "Netd ping transaction: " + ping.description());
        puts("PASS HIDL android.system.net.netd@1.1::INetd/default ping");
        fflush(stdout);
        if (argc == 2) lifecycle(netd);
#endif
        alarm(0);
        return 0;
    } catch (const std::exception& error) {
        fprintf(stderr, "FAIL %s\n", error.what());
        return 1;
    }
}
