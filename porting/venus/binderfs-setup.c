// Temporary Recovery IPC setup only. No storage or credential operations.
// Linux binderfs UAPI: BINDER_CTL_ADD dynamically allocates device numbers.
#include <errno.h>
#include <fcntl.h>
#include <linux/android/binderfs.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>
static void die(const char *what) { perror(what); exit(1); }
int main(void) {
    if (geteuid()!=0 || access("/skyward/probe-entryfix.txt",R_OK)!=0) {
        fprintf(stderr,"Requires rooted venus temporary Recovery\n"); return 1;
    }
    struct stat st;
    if (lstat("/dev/binderfs", &st)==0) {
        if (!S_ISDIR(st.st_mode)) { fprintf(stderr,"binderfs path is not a real directory\n"); return 1; }
    } else if (errno!=ENOENT || mkdir("/dev/binderfs",0755)!=0) die("mkdir binderfs");
    struct statfs fs;
    if (statfs("/dev/binderfs", &fs)!=0) die("statfs");
    if ((unsigned long)fs.f_type != 0x6c6f6f70UL) {
        if (mount("binder","/dev/binderfs","binder",MS_NOSUID|MS_NOEXEC,"max=3")!=0) die("mount binderfs");
    }
    int fd=open("/dev/binderfs/binder-control",O_RDWR|O_CLOEXEC);
    if (fd<0) die("open binder-control");
    const char *names[]={"binder","hwbinder","vndbinder"};
    for (size_t i=0; i<3; ++i) {
        char path[128], link[128];
        snprintf(path,sizeof(path),"/dev/binderfs/%s",names[i]);
        snprintf(link,sizeof(link),"/dev/%s",names[i]);
        if (lstat(path,&st)!=0) {
            if (errno!=ENOENT) die("lstat node");
            struct binderfs_device dev={0};
            snprintf(dev.name,sizeof(dev.name),"%s",names[i]);
            if (ioctl(fd,BINDER_CTL_ADD,&dev)!=0) die("BINDER_CTL_ADD");
            if (chown(path,0,0)!=0 || chmod(path,0666)!=0) die("node ownership");
            printf("created %s (%u:%u)\n",names[i],dev.major,dev.minor);
        }
        if (stat(path,&st)!=0 || !S_ISCHR(st.st_mode)) die("check binder node");
        // Match the installed system's ueventd.rc Binder endpoints. Vendor
        // supplicant drops to wifi before opening hwbinder; root:system
        // 0660 prevented the actual HIDL transport from opening at all.
        // Binder service policy remains separate from Unix node access.
        if (chown(path,0,0)!=0 || chmod(path,0666)!=0) die("binder endpoint permissions");
        struct stat old;
        if (lstat(link,&old)==0) {
            if (stat(link,&old)!=0 || !S_ISCHR(old.st_mode) || old.st_rdev!=st.st_rdev) {
                fprintf(stderr,"Refusing to replace an existing IPC endpoint %s\n",link); return 1;
            }
        } else {
            if (errno!=ENOENT || symlink(path,link)!=0) die("symlink binder");
        }
    }
    close(fd);
    puts("BINDERFS_READY; existing endpoints retained"); return 0;
}
