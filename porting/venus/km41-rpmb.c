// Restore the exact installed-vendor DAC rule for the venus RPMB endpoint.
// No read/write/ioctl is ever issued to the device. The access test opens and
// closes it as uid/gid 1000 with no supplementary groups, not as Recovery root.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#ifndef KM41_RPMB_TEST
#include "runtime-cache.h"
#endif

#define NODE "/dev/0:0:0:49476"
#define SYSDEV "/sys/class/bsg/0:0:0:49476/dev"
#define VENDOR_RULE "/mnt/skyward-ro/vendor/ueventd.rc"

// 1: exact supported rule; 0: another node/comment; -1: changed/ambiguous rule.
static int rule_line(const char *input) {
    char line[1024], path[256], mode[16], owner[64], group[64], extra[2];
    if (strlen(input) >= sizeof(line)) return -1;
    strcpy(line, input);
    char *comment = strchr(line, '#');
    if (comment) *comment = 0;
    int n = sscanf(line, " %255s %15s %63s %63s %1s", path, mode, owner, group, extra);
    if (n < 1 || strcmp(path, NODE)) return 0;
    if (n != 4) return -1;
    return !strcmp(mode, "0600") && !strcmp(owner, "system") && !strcmp(group, "system") ? 1 : -1;
}
static bool valid_device(const struct stat *s, unsigned ma, unsigned mi) {
    return S_ISCHR(s->st_mode) && ma > 0 && major(s->st_rdev) == ma && minor(s->st_rdev) == mi;
}

#ifndef KM41_RPMB_TEST
static void die(const char *what) { perror(what); exit(1); }
static void vendor_rule(void) {
    const char *path=VENDOR_RULE;
    if(access("/skyward/venus-recovery.txt",R_OK)==0 &&
       access("/tmp/skyward-km41/cache/ready",F_OK)==0) {
        venus_cache_ready();
        path="/tmp/skyward-km41/cache/vendor/ueventd.rc";
    }
    FILE *f = fopen(path, "re");
    if (!f) die("RPMB installed vendor rule unavailable");
    char line[1024]; int count = 0;
    while (fgets(line, sizeof(line), f)) {
        int match = rule_line(line);
        if (match < 0) { fclose(f); fprintf(stderr, "RPMB vendor rule is unsupported; no permission changes\n"); exit(1); }
        count += match;
    }
    if (ferror(f) || count != 1) { fclose(f); fprintf(stderr, "RPMB vendor rule missing or ambiguous; no permission changes\n"); exit(1); }
    fclose(f);
}
static int system_access(void) {
    fflush(NULL);
    pid_t pid = fork();
    if (pid < 0) die("RPMB access probe fork");
    if (!pid) {
        if (prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) || setgroups(0, NULL) ||
            setresgid(1000,1000,1000) || setresuid(1000,1000,1000)) {
            dprintf(STDERR_FILENO,"VENUS_RPMB_ACCESS_FAILED privilege_drop_errno=%d\n",errno); _exit(1);
        }
        int fd = open(NODE, O_RDWR | O_CLOEXEC | O_NOFOLLOW);
        if (fd < 0) {
            dprintf(STDERR_FILENO,"VENUS_RPMB_ACCESS_FAILED uid=1000 gid=1000 errno=%d; no_device_io=true\n",errno); _exit(1);
        }
        if (close(fd)) _exit(1);
        dprintf(STDOUT_FILENO,"VENUS_RPMB_ACCESS_OK uid=1000 gid=1000 mode=0600 open_close_only=true ioctl=false\n");
        _exit(0);
    }
    int status;
    while (waitpid(pid, &status, 0) < 0) if (errno != EINTR) die("RPMB access probe wait");
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 1;
}
int main(int argc, char **argv) {
    bool apply = argc == 2 && !strcmp(argv[1], "--apply");
    if (argc != 2 || (!apply && strcmp(argv[1], "--check"))) {
        fprintf(stderr,"Use km41-rpmb --check or --apply in temporary venus Recovery\n");return 2;
    }
    FILE *marker = fopen("/skyward/probe-entryfix.txt", "re"); char first[64];
    if (geteuid() != 0 || !marker || !fgets(first,sizeof(first),marker) || strcmp(first,"venus-entryfix-v1\n")) {
        if (marker) fclose(marker);
        fprintf(stderr,"RPMB permissions require the temporary venus Recovery; unchanged\n");return 1;
    }
    fclose(marker);
    vendor_rule();
    unsigned ma, mi; char junk;
    FILE *sys = fopen(SYSDEV, "re");
    if (!sys || fscanf(sys,"%u:%u %c",&ma,&mi,&junk) != 2) {
        if (sys) fclose(sys); fprintf(stderr,"RPMB sysfs identity unavailable; unchanged\n");return 1;
    }
    fclose(sys);
    struct stat before, opened;
    if (lstat(NODE,&before)) die("RPMB node missing");
    if (!valid_device(&before,ma,mi)) {fprintf(stderr,"RPMB node is not the expected character device; unchanged\n");return 1;}
    printf("VENUS_RPMB_NODE_BEFORE uid=%u gid=%u mode=%04o identity=verified\n",before.st_uid,before.st_gid,before.st_mode&07777);
    if (apply && (before.st_uid != 1000 || before.st_gid != 1000 || (before.st_mode&07777) != 0600)) {
        int fd = open(NODE,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
        if (fd < 0) die("RPMB root permission handle");
        if (fstat(fd,&opened) || !valid_device(&opened,ma,mi) || opened.st_ino != before.st_ino || opened.st_dev != before.st_dev) {
            close(fd);fprintf(stderr,"RPMB node changed during validation; unchanged\n");return 1;
        }
        if (fchmod(fd,0600) || fchown(fd,1000,1000)) die("RPMB exact vendor permissions");
        close(fd);
        puts("VENUS_RPMB_PERMISSIONS_APPLIED source=installed_vendor owner=system:system mode=0600");
    }
    if (lstat(NODE,&opened) || !valid_device(&opened,ma,mi)) return 1;
    int result = system_access();
    if (opened.st_uid != 1000 || opened.st_gid != 1000 || (opened.st_mode&07777) != 0600) {
        fprintf(stderr,"VENUS_RPMB_DAC_MISMATCH expected=system:system:0600\n");return 1;
    }
    return result;
}
#else
#include <assert.h>
int main(void) {
    assert(rule_line(NODE " 0600 system system\n") == 1);
    assert(rule_line("  " NODE "\t0600\tsystem\tsystem # installed vendor\n") == 1);
    assert(rule_line("# " NODE " 0600 system system\n") == 0);
    assert(rule_line("/dev/another-node 0600 system system\n") == 0);
    assert(rule_line(NODE " 0666 system system\n") == -1);
    assert(rule_line(NODE " 0600 root root\n") == -1);
    assert(rule_line(NODE " 0600 system\n") == -1);
    assert(rule_line(NODE " 0600 system system extra\n") == -1);
    struct stat s={0};s.st_mode=S_IFCHR|0600;s.st_rdev=makedev(240,1);
    assert(valid_device(&s,240,1));
    assert(!valid_device(&s,240,2));
    s.st_mode=S_IFLNK|0777;assert(!valid_device(&s,240,1));
    s.st_mode=S_IFBLK|0600;assert(!valid_device(&s,240,1));
    puts("KM41_RPMB_TESTS_PASS count=12; no device or credentials used");return 0;
}
#endif
