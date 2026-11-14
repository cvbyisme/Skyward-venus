// SPDX-License-Identifier: Apache-2.0
#include "backend.h"
#include <cassert>
#include <cstdio>

using namespace venus_netd;
struct Fake final : Backend {
    std::vector<Operation> objects;
    int add_calls = 0, remove_calls = 0, fail_add = 0;
    bool fail_remove = false, lose_ack = false, foreign_table = false, steal_on_add = false;
    unsigned current_index = 49;
    unsigned index(const std::string&) override { return current_index; }
    int unused_table(unsigned) override { return foreign_table ? -EEXIST : 0; }
    int exists(const Operation& op) override { return int(std::count(objects.begin(), objects.end(), op)); }
    int add(const Operation& op) override {
        ++add_calls;
        if (steal_on_add) { objects.push_back(op); return -EEXIST; }
        if (add_calls == fail_add && !lose_ack) return -EPERM;
        objects.push_back(op);
        return add_calls == fail_add ? -ETIMEDOUT : 0;
    }
    int remove(const Operation& op) override {
        ++remove_calls;
        if (fail_remove) return -EIO;
        auto at = std::find(objects.begin(), objects.end(), op);
        if (at == objects.end()) return -ENOENT;
        objects.erase(at); return 0;
    }
};
static uint64_t create(Manager& manager) {
    uint64_t h = 0; uint32_t fw = 0;
    assert(manager.create(h, fw) == 0); assert(netid(h) && fw == mark(netid(h))); return h;
}
static void allocation() {
    Fake backend; Manager manager(backend, "test");
    std::vector<uint64_t> handles;
    for (unsigned i = 1; i <= 50; ++i) { auto h = create(manager); assert(netid(h) == i); handles.push_back(h); }
    uint64_t h = 9; uint32_t fw = 9;
    assert(manager.create(h, fw) == -ENONET && h == 0 && fw == 0);
    assert(backend.add_calls == 0 && backend.remove_calls == 0 && backend.objects.empty());
    assert(manager.destroy(0) == -EINVAL);
    assert(manager.destroy(handles[0] ^ 1) == -EINVAL);
    assert(manager.destroy(handles[0]) == 0);
    assert(manager.destroy(handles[0]) == -ENONET);
    assert(create(manager) == handles[0]);
    assert(manager.shutdown() == 0);
    assert(manager.create(h, fw) == -ESHUTDOWN);
}
static void semantics() {
    auto ops = interface_operations(1, "rmnet_data0", 49, "owner");
    assert(ops.size() == 8);
    assert(ops[0].value == 0xf0001 && ops[0].mask == 0xffefffff);
    assert(ops[1].priority == 13000 && ops[1].value == 0xd0001 && ops[1].mask == 0xdffff);
    assert(ops[2].priority == 10500 && ops[2].root_only && ops[2].oif == "rmnet_data0");
    assert(ops[3].priority == 14000 && ops[3].value == 0xc0000 && ops[3].mask == 0xc0000);
    assert(ops[4].family == AF_INET6 && ops[4].table == 1049);
    Message encoded = request(ops[2], true);
    const auto* h = reinterpret_cast<const nlmsghdr*>(encoded.bytes.data());
    assert(h->nlmsg_type == RTM_NEWRULE && (h->nlmsg_flags & NLM_F_EXCL));
    const auto* rule = static_cast<const fib_rule_hdr*>(NLMSG_DATA(h));
    assert(rule->action == FR_ACT_TO_TBL && rule->family == AF_INET);
    int remaining = int(h->nlmsg_len - NLMSG_LENGTH(sizeof(*rule)));
    auto* attr = reinterpret_cast<const rtattr*>(reinterpret_cast<const uint8_t*>(rule) + NLMSG_ALIGN(sizeof(*rule)));
    bool uid = false, table = false, iif = false;
    for (; RTA_OK(attr, remaining); attr = RTA_NEXT(attr, remaining)) {
        if (attr->rta_type == FRA_UID_RANGE) { fib_rule_uid_range r; std::memcpy(&r, RTA_DATA(attr), sizeof(r)); assert(r.start == 0 && r.end == 0); uid = true; }
        if (attr->rta_type == FRA_TABLE) { unsigned v; std::memcpy(&v, RTA_DATA(attr), sizeof(v)); assert(v == 1049); table = true; }
        if (attr->rta_type == FRA_IIFNAME) { assert(RTA_PAYLOAD(attr) == 3 && !std::memcmp(RTA_DATA(attr), "lo", 3)); iif = true; }
    }
    assert(!remaining && uid && table && iif);
}
static void routes_and_ownership() {
    Fake backend; Manager manager(backend, "test"); auto h = create(manager), other = create(manager);
    assert(manager.add_interface(h, "rmnet_data0") == 0 && backend.objects.size() == 8);
    assert(manager.add_interface(h, "rmnet_data0") == 0 && backend.objects.size() == 8);
    assert(manager.add_interface(other, "rmnet_data0") == -EBUSY);
    assert(manager.route(h, "rmnet_data0", "192.0.2.0/24", "", true) == 0);
    assert(manager.route(h, "rmnet_data0", "0.0.0.0/0", "192.0.2.1", true) == 0);
    assert(manager.route(h, "rmnet_data0", "2001:db8::/64", "", true) == 0);
    assert(manager.route(h, "rmnet_data0", "::/0", "2001:db8::1", true) == 0);
    size_t count = backend.objects.size();
    assert(manager.route(h, "rmnet_data0", "0.0.0.0/0", "2001:db8::1", true) == -EINVAL);
    assert(manager.route(h, "rmnet_data0", "192.0.2.1/24", "", true) == -EINVAL);
    assert(manager.route(h, "rmnet_data0", "0.0.0.0/0", std::string("192.0.2.1\0bad", 13), true) == -EINVAL);
    assert(backend.objects.size() == count);
    assert(manager.route(h, "rmnet_data0", "198.51.100.0/24", "", false) == -ENOENT);
    assert(manager.destroy(other) == 0 && backend.objects.size() == count);
    assert(manager.destroy(h) == 0 && backend.objects.empty());
}
static void rollback_and_retry() {
    for (bool lost : {false, true}) for (int failed = 1; failed <= 8; ++failed) {
        Fake backend; backend.fail_add = failed; backend.lose_ack = lost;
        Manager manager(backend, "test"); auto h = create(manager);
        assert(manager.add_interface(h, "rmnet_data0") != 0);
        assert(backend.objects.empty()); assert(manager.destroy(h) == 0);
    }
    Fake backend; Manager manager(backend, "test"); auto h = create(manager);
    assert(manager.add_interface(h, "rmnet_data0") == 0);
    backend.fail_remove = true;
    assert(manager.shutdown() == -EIO && backend.objects.size() == 8);
    uint64_t rejected; uint32_t mark;
    assert(manager.create(rejected, mark) == -ESHUTDOWN);
    backend.fail_remove = false;
    assert(manager.shutdown() == 0 && backend.objects.empty());
}
static void refuse_foreign() {
    Fake backend; Manager manager(backend, "test"); auto h = create(manager);
    backend.foreign_table = true;
    assert(manager.add_interface(h, "rmnet_data0") == -EEXIST && backend.add_calls == 0);
    backend.foreign_table = false; backend.steal_on_add = true;
    assert(manager.add_interface(h, "rmnet_data0") == -EEXIST);
    assert(backend.objects.size() == 1 && backend.remove_calls == 0);
    assert(manager.shutdown() == 0 && backend.objects.size() == 1);
}
int main() {
    allocation(); semantics(); routes_and_ownership(); rollback_and_retry(); refuse_foreign();
    // Exercise real read-only netlink parsing without requiring NET_ADMIN.
    LinuxBackend live;
    int result = live.unused_table(0xffffffe0);
    assert(result == 0 || result == -EEXIST);
    std::puts("PASS netd allocation, AOSP wire rules, routes, rollback, lost ACK, cleanup retry, foreign ownership, read-only netlink");
}
