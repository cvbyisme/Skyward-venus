// Per-process mount namespace for original vendor security services.
// Uses installed system runtime + VNDK30 without replacing Recovery libraries.
// Diagnostic daemons run as root in the already-permissive temporary Recovery.
// No /data or /metadata is exposed to this chroot. Do not use as a daily HAL setup.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <linux/capability.h>
#include <limits.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "runtime-cache.h"
static void fail(const char *s) { perror(s); exit(1); }
static char root[PATH_MAX];
static void wifi_helper_user(int cnss) {
 const gid_t groups[]={1000,3003,3005,1010};
 if(prctl(PR_SET_KEEPCAPS,1,0,0,0)!=0 ||
    setgroups(cnss?4:1,groups)!=0 || setresgid(1000,1000,1000)!=0 ||
    setresuid(1000,1000,1000)!=0)fail("WLAN helper uid/groups");
 struct __user_cap_header_struct header={.version=_LINUX_CAPABILITY_VERSION_3,.pid=0};
 struct __user_cap_data_struct data[2]={{0},{0}};
 if(cnss) data[0].effective=data[0].permitted=data[0].inheritable=1U<<CAP_NET_ADMIN;
 if(syscall(__NR_capset,&header,data)!=0)fail("WLAN helper capabilities");
 if(cnss && prctl(PR_CAP_AMBIENT,PR_CAP_AMBIENT_RAISE,CAP_NET_ADMIN,0,0)!=0)fail("CNSS exec capability");
}
static void target(char *out,const char *path) {
 if (snprintf(out,PATH_MAX,"%s%s",root,path)>=PATH_MAX) {errno=ENAMETOOLONG;fail("target");}
}
static void directory(const char *p,mode_t mode) {
 struct stat s;
 if(lstat(p,&s)==0) {
  if(!S_ISDIR(s.st_mode)){errno=ENOTDIR;fail(p);}
 } else if(errno!=ENOENT || mkdir(p,mode)!=0)fail(p);
 // The launcher inherits umask 077 from the diagnostic shell. QSEECom
 // drops to uid system: its chroot root and intermediate directories must
 // remain searchable. Set exact modes through a no-follow directory FD;
 // the enclosing host directory and diagnostic logs remain private.
 int fd=open(p,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)fail(p);
 if(fchmod(fd,mode)!=0)fail("directory mode");
 if(close(fd)!=0)fail("close directory");
}
static void netmgr_ram_directory(int fd,struct stat *st) {
 struct statfs fs;
 if(fstat(fd,st)!=0 || !S_ISDIR(st->st_mode) || fstatfs(fd,&fs)!=0 ||
    (unsigned long)fs.f_type!=0x01021994UL) {
  errno=EINVAL;fail("NETMGR requires a RAM directory");
 }
}
static int netmgr_directory_at(int parent,const char *name,uid_t uid,gid_t gid,mode_t mode,int create) {
 struct stat before,st;netmgr_ram_directory(parent,&before);
 int made=0;
 if(create) {
  if(mkdirat(parent,name,0700)==0)made=1;
  else if(errno!=EEXIST)fail("create NETMGR RAM directory");
 }
 int fd=openat(parent,name,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)fail("open NETMGR RAM directory");
 netmgr_ram_directory(fd,&st);
 if(st.st_dev!=before.st_dev) {errno=EINVAL;fail("foreign NETMGR directory mount");}
 if(made && (fchown(fd,uid,gid)!=0 || fchmod(fd,mode)!=0))fail("own new NETMGR RAM directory");
 if(fstat(fd,&st)!=0 || st.st_uid!=uid || st.st_gid!=gid || (st.st_mode&07777)!=mode) {
  errno=EINVAL;fail("foreign NETMGR RAM directory ownership or mode");
 }
 return fd;
}
static void netmgr_recovery_file(int parent,uid_t uid,gid_t gid) {
 struct stat before,st;netmgr_ram_directory(parent,&before);
 int made=1;
 int fd=openat(parent,"recovery_info",O_RDWR|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
 if(fd<0 && errno==EEXIST) {
  made=0;
  // A previous netmgr instance owns this state. Validate it without
  // truncating it; NONBLOCK also makes a foreign FIFO fail without waiting.
  fd=openat(parent,"recovery_info",O_RDONLY|O_NONBLOCK|O_NOFOLLOW|O_CLOEXEC);
 }
 if(fd<0)fail("open NETMGR recovery_info");
 if(fstat(fd,&st)!=0 || !S_ISREG(st.st_mode) || st.st_nlink!=1 || st.st_dev!=before.st_dev) {
  errno=EINVAL;fail("foreign NETMGR recovery_info type or mount");
 }
 if(made && (fchown(fd,uid,gid)!=0 || fchmod(fd,0600)!=0))fail("own new NETMGR recovery_info");
 if(fstat(fd,&st)!=0 || st.st_uid!=uid || st.st_gid!=gid ||
    (st.st_mode&0600)!=0600 || (st.st_mode&07133)!=0) {
  errno=EINVAL;fail("foreign NETMGR recovery_info ownership or mode");
 }
 if(close(fd)!=0)fail("close NETMGR recovery_info");
}
static int netmgr_root_directory(const char *path) {
 int fd=open(path,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)fail("open NETMGR root directory");
 struct stat st;netmgr_ram_directory(fd,&st);
 if(st.st_uid!=0 || st.st_gid!=0 || (st.st_mode&07777)!=0755) {
  errno=EINVAL;fail("foreign NETMGR root directory");
 }
 return fd;
}
static void netmgr_init_paths(void) {
 // Installed netmgr.rc: /dev/socket/netmgr 0750 radio:radio, and private
 // /data/vendor/netmgr/{recovery,recovery/recovery_info}. Evidence:
 // venus_dump/network-20261110/logs/normal-netmgr-ipacm-profile.txt:58,74-81.
 // Only /dev (RAM) is shared. All /data state stays under this RAM chroot.
 int dev=netmgr_root_directory("/dev");
 int sockets=netmgr_directory_at(dev,"socket",0,0,0755,0);
 int socket_dir=netmgr_directory_at(sockets,"netmgr",1001,1001,0750,1);
 if(close(socket_dir)!=0 || close(sockets)!=0 || close(dev)!=0)fail("close NETMGR socket directories");
 int home=netmgr_root_directory(root);
 int data=netmgr_directory_at(home,"data",0,0,0755,0);
 int vendor=netmgr_directory_at(data,"vendor",0,0,0755,1);
 int state=netmgr_directory_at(vendor,"netmgr",1001,1001,0770,1);
 int recovery=netmgr_directory_at(state,"recovery",1001,1001,0770,1);
 netmgr_recovery_file(recovery,1001,1001);
 if(close(recovery)!=0 || close(state)!=0 || close(vendor)!=0 || close(data)!=0 || close(home)!=0)
  fail("close NETMGR state directories");
}
static void bind(const char *src,const char *to,int rec,int ro) {
 char dst[PATH_MAX];target(dst,to);
 if(mount(src,dst,NULL,MS_BIND|(rec?MS_REC:0),NULL)!=0)fail(src);
 if(ro && mount(NULL,dst,NULL,MS_REMOUNT|MS_BIND|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL)!=0)fail("readonly bind");
}
static void detach_private_sources(void) {
 // unshare + MS_PRIVATE above makes these only this child's inherited
 // copies. chroot alone would leave them pinning their block devices even
 // though they are outside the process's visible root. Never use a lazy or
 // forced unmount, and never remove the parent's mappings here.
 const char *parts[]={"system","system_ext","vendor","odm","product","mi_ext","system_dlkm","vendor_dlkm","odm_dlkm"};
 for(size_t i=0;i<sizeof(parts)/sizeof(parts[0]);i++) {
  char path[PATH_MAX];
  snprintf(path,sizeof(path),"/mnt/skyward-ro/%s",parts[i]);
  if(umount(path)!=0 && errno!=EINVAL && errno!=ENOENT)fail("detach private logical source");
 }
 // WLAN may publish a factory-address child mount on the RAM firmware
 // aliases. Remove the inherited children before their parents, only in
 // this launcher's private namespace. No lazy/forced unmount is allowed.
 const char *firmware_children[]={"/system/etc/firmware/wlan/qca_cld","/vendor/firmware/wlan/qca_cld"};
 for(size_t i=0;i<sizeof(firmware_children)/sizeof(firmware_children[0]);i++) {
  if(umount(firmware_children[i])!=0 && errno!=EINVAL && errno!=ENOENT)fail("detach private WLAN firmware child");
 }
 if(umount("/vendor/firmware_mnt")!=0 && errno!=EINVAL && errno!=ENOENT)fail("detach private firmware source");
 if(umount("/vendor/firmware")!=0 && errno!=EINVAL && errno!=ENOENT)fail("detach private stock firmware alias");
 // /sdcard is a separate bind of decrypted Data after CE unlock. Its
 // inherited private copy pins userdata even after /data is detached and
 // chroot hides it. Release that alias before the underlying Data mount.
 const char *private_storage[]={"/sdcard","/data","/metadata","/persist","/mnt/vendor/persist","/cache"};
 for(size_t i=0;i<sizeof(private_storage)/sizeof(private_storage[0]);i++) {
  if(umount(private_storage[i])!=0 && errno!=EINVAL && errno!=ENOENT)fail("detach private inherited storage");
 }
}
int main(int argc,char **argv) {
 if(argc<2 || argc>3 || geteuid()!=0 || access("/skyward/probe-entryfix.txt",R_OK)!=0) {
  fprintf(stderr,"usage: km41-launch hwmanager|servicemanager|boot|keymaster|qseecomd|gatekeeper|rmt_storage|tftp|ipacm|netmgr|netmgr_ready|network_suspend|network_netd|vndmanager|peripheral|cnss|wpa [--list], in venus Recovery\n");return 1;
 }
 const char *program=NULL;int manager=0,namespaced=0,direct=0,tftp=0,ipacm=0,netmgr=0,ram_helper=0,cnss=0,wpa=0,vndmanager=0,peripheral=0;
 if(!strcmp(argv[1],"hwmanager")){program="/system_ext/bin/hwservicemanager";manager=1;}
 else if(!strcmp(argv[1],"servicemanager")){program="/system/bin/servicemanager";manager=1;direct=1;}
 else if(!strcmp(argv[1],"boot")){program="/vendor/bin/hw/android.hardware.boot-service.venus";namespaced=1;direct=1;}
 else if(!strcmp(argv[1],"rmt_storage")){program="/vendor/bin/rmt_storage";namespaced=1;direct=1;}
 else if(!strcmp(argv[1],"tftp")){program="/vendor/bin/tftp_server";namespaced=1;direct=1;tftp=1;}
 else if(!strcmp(argv[1],"ipacm")){program="/vendor/bin/ipacm";namespaced=1;direct=1;ipacm=1;}
 else if(!strcmp(argv[1],"netmgr")){program="/vendor/bin/netmgrd";namespaced=1;direct=1;netmgr=1;}
 else if(!strcmp(argv[1],"network_suspend")){program="/skyward/runtime/network-suspend";namespaced=1;direct=1;ram_helper=1;}
 else if(!strcmp(argv[1],"network_netd")){program="/skyward/runtime/network-netd";namespaced=1;direct=1;ram_helper=1;}
 else if(!strcmp(argv[1],"netmgr_ready")){program="/skyward/runtime/netmgr-ready";namespaced=1;direct=1;ram_helper=1;}
 else if(!strcmp(argv[1],"cnss")){program="/vendor/bin/cnss-daemon";namespaced=1;direct=1;cnss=1;}
 else if(!strcmp(argv[1],"wpa")){program="/vendor/bin/hw/wpa_supplicant";namespaced=1;direct=1;wpa=1;}
 else if(!strcmp(argv[1],"vndmanager")){program="/vendor/bin/vndservicemanager";namespaced=1;direct=1;vndmanager=1;}
 else if(!strcmp(argv[1],"peripheral")){program="/vendor/bin/pm-service";namespaced=1;direct=1;peripheral=1;}
 else if(!strcmp(argv[1],"keymaster"))program="/vendor/bin/hw/android.hardware.keymaster@4.1-service-qti";
 else if(!strcmp(argv[1],"qseecomd"))program="/vendor/bin/qseecomd";
 else if(!strcmp(argv[1],"gatekeeper"))program="/vendor/bin/hw/android.hardware.gatekeeper@1.0-service-qti";
 else {fprintf(stderr,"unsupported role\n");return 1;}
 if(argc==3 && strcmp(argv[2],"--list")){fprintf(stderr,"unsupported argument\n");return 1;}
 int cached=access("/skyward/venus-recovery.txt",R_OK)==0;
 if(cached)venus_cache_ready();
 directory("/tmp/skyward-km41",0700);
 snprintf(root,sizeof(root),"/tmp/skyward-km41/root-%s",argv[1]);directory(root,0755);
 if(unshare(CLONE_NEWNS)!=0)fail("unshare mount namespace");
 if(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL)!=0)fail("private mounts");
 const char *dirs[]={"/dev","/proc","/sys","/system","/system_ext","/vendor","/odm","/product","/apex","/linkerconfig","/tmp","/mnt","/data","/metadata","/firmware","/apex/com.android.runtime","/apex/com.android.vndk.v30"};
 for(size_t i=0;i<sizeof(dirs)/sizeof(dirs[0]);i++){char p[PATH_MAX];target(p,dirs[i]);directory(p,0755);}
 bind("/dev","/dev",1,0);bind("/proc","/proc",0,0);bind("/sys","/sys",1,0);
 if(netmgr) {
  if(!cached) {errno=EINVAL;fail("NETMGR requires verified RAM runtime");}
  // The installed binary opens /system/vendor/etc/data/netmgr_config.xml.
  // Preserve Android's /system/vendor -> /vendor alias inside this private
  // wrapper without changing the verified cache shared by the six HALs.
  const char *components[]={"bin","etc","lib64"};
  for(size_t i=0;i<sizeof(components)/sizeof(components[0]);i++) {
   char src[PATH_MAX],dst[PATH_MAX],p[PATH_MAX];
   snprintf(src,sizeof(src),"/tmp/skyward-km41/cache/system/system/%s",components[i]);
   snprintf(dst,sizeof(dst),"/system/%s",components[i]);
   target(p,dst);directory(p,0755);bind(src,dst,0,1);
  }
  char p[PATH_MAX];target(p,"/system/vendor");
  struct stat st;
  if(lstat(p,&st)==0) {
   char link[32];ssize_t n=readlink(p,link,sizeof(link));
   if(!S_ISLNK(st.st_mode) || n!=7 || memcmp(link,"/vendor",7)) {errno=EINVAL;fail("private system vendor alias");}
  } else if(errno!=ENOENT || symlink("/vendor",p)!=0)fail("create private system vendor alias");
 } else bind(cached?"/tmp/skyward-km41/cache/system/system":"/mnt/skyward-ro/system/system","/system",0,1);
 bind(cached?"/tmp/skyward-km41/cache/system_ext":"/mnt/skyward-ro/system_ext","/system_ext",0,1);
 if(tftp) {
  if(!cached) {errno=EINVAL;fail("TFTP requires verified RAM runtime");}
  // Keep the RFS service's writable calibration workspace private and
  // volatile. Never expose the phone's mounted persist or Data here.
  const char *components[]={"bin","etc","lib64","firmware","rfs"};
  for(size_t i=0;i<sizeof(components)/sizeof(components[0]);i++) {
   char src[PATH_MAX],dst[PATH_MAX],p[PATH_MAX];
   snprintf(src,sizeof(src),"/tmp/skyward-km41/cache/vendor/%s",components[i]);
   snprintf(dst,sizeof(dst),"/vendor/%s",components[i]);
   target(p,dst);directory(p,0755);bind(src,dst,0,1);
  }
  char p[PATH_MAX];target(p,"/vendor/firmware_mnt");directory(p,0755);
  const char *extra[]={"/mnt/vendor","/mnt/vendor/persist","/data/vendor","/data/vendor/tombstones","/data/vendor/tombstones/rfs"};
  for(size_t i=0;i<sizeof(extra)/sizeof(extra[0]);i++) {target(p,extra[i]);directory(p,0755);}
  // The installed daemon drops to vendor_rfs (2903), exactly as named in
  // this ROM's passwd/init. Only its volatile dump directory is writable.
  target(p,"/data/vendor/tombstones/rfs");
  if(chown(p,2903,2903)!=0 || chmod(p,0770)!=0)fail("private RFS dump ownership");
  struct stat st;struct statfs fs;
  const char *scratch="/tmp/venus-network-persist";
  if(lstat(scratch,&st)!=0 || !S_ISDIR(st.st_mode) || st.st_uid!=0 ||
     st.st_gid!=2903 || (st.st_mode&0027)!=0 || statfs(scratch,&fs)!=0 || (unsigned long)fs.f_type!=0x01021994UL) {
   errno=EINVAL;fail("private RAM RFS workspace");
  }
  bind(scratch,"/mnt/vendor/persist",0,0);
 } else bind(cached?"/tmp/skyward-km41/cache/vendor":"/mnt/skyward-ro/vendor","/vendor",0,1);
 bind(cached?"/tmp/skyward-km41/cache/odm":"/mnt/skyward-ro/odm","/odm",0,1);
 bind(cached?"/tmp/skyward-km41/cache/product":"/mnt/skyward-ro/product","/product",0,1);
 bind("/tmp/venus-runtime","/apex/com.android.runtime",0,1);
 bind("/tmp/venus-vndk30","/apex/com.android.vndk.v30",0,1);
 if(netmgr) {
  // Android 17's LLNDK libandroid_net depends on its installed tethering
  // APEX. Keep that platform ABI separate from the VNDK30 vendor backend.
  char p[PATH_MAX];target(p,"/apex/com.android.tethering");directory(p,0755);
  bind("/tmp/venus-tethering","/apex/com.android.tethering",0,1);
 }
 bind(cached?"/tmp/skyward-km41/cache/firmware":"/vendor/firmware_mnt","/vendor/firmware_mnt",0,1);
 bind(cached?"/tmp/skyward-km41/cache/firmware":"/vendor/firmware_mnt","/firmware",0,1);
 bind("/tmp","/tmp",0,0);
 if(ipacm) {
  // Match the installed init's private IPA state directory. It remains
  // RAM-only in this namespace; the phone's personal Data is not exposed.
  char p[PATH_MAX];target(p,"/data/vendor");directory(p,0755);
  target(p,"/data/vendor/ipa");directory(p,0770);
  if(chown(p,1001,1001)!=0)fail("private IPA state ownership");
 }
 if(netmgr)netmgr_init_paths();
 if(cnss || wpa) {
  // Only volatile Wi-Fi state is visible. The installed daemon obtains
  // the platform MAC/calibration through CNSS; do not invent a MAC or
  // substitute another device's persist files when Venus has none.
  char p[PATH_MAX];target(p,"/data/vendor");directory(p,0755);
  const char *wifi_dirs[]={"/data/vendor/wifi","/data/vendor/wifi/sockets","/data/vendor/wifi/wpa","/data/vendor/wifi/wpa/sockets"};
  for(size_t i=0;i<sizeof(wifi_dirs)/sizeof(wifi_dirs[0]);i++) {
   target(p,wifi_dirs[i]);directory(p,0770);
   if(chown(p,1010,1010)!=0)fail("private Wi-Fi state ownership");
  }
 }
 if(wpa) {
  char p[PATH_MAX];
  const char *dirs[]={"/skyward","/skyward/run","/skyward/run/supplicant"};
  for(size_t i=0;i<sizeof(dirs)/sizeof(dirs[0]);i++) {target(p,dirs[i]);directory(p,0755);}
  struct stat st;struct statfs fs;
  const char *home="/skyward/run/supplicant";
  if(lstat(home,&st)!=0 || !S_ISDIR(st.st_mode) || st.st_uid!=0 ||
     (st.st_mode&0022)!=0 || statfs(home,&fs)!=0 || (unsigned long)fs.f_type!=0x01021994UL) {
   errno=EINVAL;fail("private RAM supplicant workspace");
  }
  bind(home,"/skyward/run/supplicant",0,0);
 }
 if(ram_helper) {
  if(!cached) {errno=EINVAL;fail("network helper requires verified RAM runtime");}
  // Expose only the selected helper in its own private root. Both suspend
  // HIDL and the netmgr client must use the original isolated vendor ABI.
  struct stat st;char p[PATH_MAX];
  if(lstat(program,&st)!=0 || !S_ISREG(st.st_mode) || st.st_uid!=0 ||
     (st.st_mode&0022)!=0 || (st.st_mode&0100)==0) {errno=EINVAL;fail("network helper asset");}
  target(p,"/skyward");directory(p,0755);
  target(p,"/skyward/runtime");directory(p,0755);
  target(p,program);
  int fd=open(p,O_RDONLY|O_CREAT|O_NOFOLLOW|O_CLOEXEC,0700);
  if(fd<0)fail("create network helper target");
  if(fstat(fd,&st)!=0 || !S_ISREG(st.st_mode)) {errno=EINVAL;fail("network helper target");}
  if(close(fd)!=0)fail("close network helper target");
  bind(program,program,0,1);
 }
 // Bind only a minimal framework declaration, leaving installed manifests unchanged.
 bind("/tmp/skyward-km41/framework-manifest.xml","/system/etc/vintf/manifest.xml",0,1);
 if(namespaced) {
  char config[PATH_MAX];target(config,"/linkerconfig/ld.config.txt");
  int fd=open(config,O_RDONLY|O_CREAT|O_NOFOLLOW|O_CLOEXEC,0600);
  if(fd<0)fail("create linker config target");
  struct stat st;
  if(fstat(fd,&st)!=0 || !S_ISREG(st.st_mode)) {errno=EINVAL;fail("linker config target");}
  if(close(fd)!=0)fail("close linker config target");
  bind(netmgr?"/skyward/runtime/network-ld.config.txt":"/skyward/runtime/boot-ld.config.txt","/linkerconfig/ld.config.txt",0,1);
 }
 if(cached)detach_private_sources();
 if(chroot(root)!=0 || chdir("/")!=0)fail("chroot");
 setenv("PATH","/system/bin:/vendor/bin:/system_ext/bin",1);
 if(namespaced) {
  // LLNDK Binder must retain the installed system ABI while the Qualcomm
  // backend uses VNDK30 libbase/libcutils. A flat LD_LIBRARY_PATH mixes them.
  unsetenv("LD_LIBRARY_PATH");unsetenv("LD_PRELOAD");
 } else {
 setenv("LD_LIBRARY_PATH", manager ?
  "/apex/com.android.runtime/lib64/bionic:/system/lib64:/system_ext/lib64:/vendor/lib64:/apex/com.android.vndk.v30/lib64" :
  "/apex/com.android.runtime/lib64/bionic:/vendor/lib64:/apex/com.android.vndk.v30/lib64:/system/lib64",1);
 // The installed LLNDK liblog requires the installed libc++ ABI.
 if(!manager)setenv("LD_PRELOAD","/system/lib64/libc++.so",1);
 }
 const char *loader="/apex/com.android.runtime/bin/linker64";
 fprintf(stderr,"KM41_LAUNCH role=%s root-only-diagnostic private-mount-namespace data-not-mounted\n",argv[1]);
 if(argc!=3 && (cnss || vndmanager || peripheral))wifi_helper_user(cnss);
 if(argc==3)execl(loader,loader,"--list",program,(char*)NULL);
 else if(vndmanager)execl(program,program,"/dev/vndbinder",(char*)NULL);
 else if(cnss)execl(program,program,"-n","-l",(char*)NULL);
 else if(wpa)execl(program,program,"-i","wlan0","-D","nl80211","-c","/skyward/run/supplicant/wpa_supplicant.conf",(char*)NULL);
 else if(direct)execl(program,program,(char*)NULL);
 else execl(loader,loader,program,(char*)NULL);
 fail("exec security service");return 1;
}
