// SPDX-License-Identifier: Apache-2.0
// Pass the parent's net namespace inode before unshare -Urn; see README.md.
#include "backend.h"
#include <cassert>
#include <sys/stat.h>
using namespace venus_netd;
int main(int argc, char** argv) {
    assert(argc == 2);
    struct stat self{};
    assert(!stat("/proc/self/ns/net", &self));
    const auto initial = std::stoull(argv[1]);
    assert(initial && self.st_ino != initial); // Never alter the parent's network namespace.
    LinuxBackend kernel;
    unsigned index = kernel.index("netdtest0"); assert(index);
    assert(kernel.unused_table(index + 1000) == 0);
    auto operations = interface_operations(1, "netdtest0", index, "test");
    std::vector<Operation> owned;
    for (const auto& op : operations) if (op.kind != Operation::Mark) {
        assert(kernel.exists(op) == 0);
        assert(kernel.add(op) == 0);
        int found = kernel.exists(op);
        if (found != 1) std::fprintf(stderr, "rule readback priority=%u family=%d value=%d\n", op.priority, op.family, found);
        assert(found == 1); owned.push_back(op);
    }
    assert(kernel.unused_table(index + 1000) == -EEXIST);
    const char* destinations[] = {"192.0.2.0/24", "0.0.0.0/0", "2001:db8::/64", "::/0"};
    const char* gateways[] = {"", "192.0.2.1", "", "2001:db8::1"};
    for (unsigned i = 0; i < 4; ++i) {
        Operation op; op.kind = Operation::Route; op.iface = "netdtest0"; op.index = index; op.table = index + 1000;
        assert(parse_route(destinations[i], gateways[i], op) == 0);
        assert(kernel.exists(op) == 0);
        int added = kernel.add(op); if (added) std::fprintf(stderr, "route add error=%d\n", added);
        assert(added == 0);
        int found = kernel.exists(op);
        if (found != 1) std::fprintf(stderr, "route readback i=%u value=%d\n", i, found);
        assert(found == 1);
        assert(kernel.add(op) == -EEXIST);
        owned.push_back(op);
    }
    for (auto at = owned.rbegin(); at != owned.rend(); ++at) {
        assert(kernel.remove(*at) == 0); assert(kernel.exists(*at) == 0);
    }
    assert(kernel.unused_table(index + 1000) == 0);
    Manager manager(kernel, "skyward-oem-test-" + std::to_string(getpid()));
    uint64_t h = 0; uint32_t fwmark = 0;
    assert(manager.create(h, fwmark) == 0 && fwmark == 0xf0001);
    assert(manager.add_interface(h, "netdtest0") == 0);
    assert(manager.route(h, "netdtest0", "192.0.2.0/24", "", true) == 0);
    assert(manager.route(h, "netdtest0", "0.0.0.0/0", "192.0.2.1", true) == 0);
    assert(manager.route(h, "netdtest0", "2001:db8::/64", "", true) == 0);
    assert(manager.route(h, "netdtest0", "::/0", "2001:db8::1", true) == 0);
    assert(manager.shutdown() == 0);
    assert(kernel.unused_table(index + 1000) == 0);
    std::puts("PASS isolated real-kernel IPv4/IPv6 rules, routes, duplicate rejection, exact cleanup");
}
