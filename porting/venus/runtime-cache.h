// Shared validation for the private, current-boot RAM runtime cache.
#ifndef VENUS_RUNTIME_CACHE_H
#define VENUS_RUNTIME_CACHE_H
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>
static void venus_cache_fail(const char *what) { perror(what); exit(1); }
static void venus_cache_ready(void) {
 const char *cache="/tmp/skyward-km41/cache";
 struct stat st;struct statfs fs;
 if(lstat(cache,&st)!=0 || !S_ISDIR(st.st_mode) || st.st_uid!=0 ||
    (st.st_mode&0077)!=0 || statfs(cache,&fs)!=0 || (unsigned long)fs.f_type!=0x01021994UL) {
  errno=EINVAL;venus_cache_fail("private RAM runtime cache");
 }
 char boot[64]={0},got[128]={0},want[128];
 int fd=open("/proc/sys/kernel/random/boot_id",O_RDONLY|O_CLOEXEC);
 if(fd<0)venus_cache_fail("cache boot identity");
 ssize_t n=read(fd,boot,sizeof(boot)-1);
 if(n!=37 || boot[36]!='\n') {errno=EINVAL;venus_cache_fail("cache boot identity format");}
 if(close(fd)!=0)venus_cache_fail("close boot identity");
 boot[36]=0;
 fd=open("/tmp/skyward-km41/cache/ready",O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)venus_cache_fail("runtime cache receipt");
 if(fstat(fd,&st)!=0 || !S_ISREG(st.st_mode) || st.st_uid!=0 || (st.st_mode&0022)!=0) {
  errno=EINVAL;venus_cache_fail("cache receipt identity");
 }
 n=read(fd,got,sizeof(got)-1);
 if(n<0)venus_cache_fail("read cache receipt");
 if(close(fd)!=0)venus_cache_fail("close cache receipt");
 int expected=snprintf(want,sizeof(want),"venus-runtime-cache-v1\nboot=%s\n",boot);
 if(n!=expected || memcmp(got,want,(size_t)expected)!=0) {errno=EINVAL;venus_cache_fail("stale runtime cache");}
}
#endif
