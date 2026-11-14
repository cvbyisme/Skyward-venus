// Read-only, bounded /dev/kmsg stream for the host-side diagnostic runner.
// No mounts, keys, firmware, properties, signals to other processes or reboots.
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int marker_valid(void) {
    char line[64];
    FILE *f = fopen("/skyward/probe-entryfix.txt", "re");
    if (!f) return 0;
    int ok = fgets(line, sizeof(line), f) != NULL &&
             strcmp(line, "venus-entryfix-v1\n") == 0;
    fclose(f);
    return ok;
}
static int emit(const void *data, size_t size) {
    const char *p = data;
    while (size) {
        ssize_t n = write(STDOUT_FILENO, p, size);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return -1;
        p += n; size -= (size_t)n;
    }
    return 0;
}
static int64_t monotonic_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts)) return -1;
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
int main(void) {
    if (geteuid() != 0 || !marker_valid()) {
        fputs("Refusing kmsg capture outside venus temporary Recovery\n", stderr);
        return 2;
    }
    int fd = open("/dev/kmsg", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) { perror("open kmsg"); return 1; }
    int64_t start = monotonic_ms();
    if (start < 0) { close(fd); return 1; }
    const char ready[] = "KM41_KMSG_CAPTURE_READY max_bytes=16777216 max_seconds=300\n";
    if (emit(ready, sizeof(ready)-1)) { close(fd); return 1; }
    char buffer[65536];
    size_t total = 0;
    struct pollfd p = {.fd=fd, .events=POLLIN};
    while (total < 16u*1024u*1024u) {
        int64_t now = monotonic_ms();
        if (now < 0 || now - start >= 300000) break;
        int events = poll(&p, 1, 250);
        if (events < 0 && errno == EINTR) continue;
        if (events < 0) { perror("poll kmsg"); close(fd); return 1; }
        if (!events) continue;
        ssize_t n = read(fd, buffer, sizeof(buffer));
        if (n < 0 && (errno == EAGAIN || errno == EINTR)) continue;
        if (n < 0 && errno == EPIPE) {
            const char lost[] = "KM41_KMSG_OVERRUN: old records lost\n";
            if (emit(lost, sizeof(lost)-1)) break;
            continue;
        }
        if (n <= 0) { if (n < 0) perror("read kmsg"); close(fd); return 1; }
        if (emit(buffer, (size_t)n)) { close(fd); return 1; }
        total += (size_t)n;
    }
    close(fd);
    return 0;
}
