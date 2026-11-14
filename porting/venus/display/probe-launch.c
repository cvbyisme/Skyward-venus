// Standalone read-only display diagnostic root. No network launcher roles,
// production services, procfs, sysfs, DRM, or whole Data mount. The sole
// device is real vndbinder for the probe's bounded pre-guard initialization.
#define _GNU_SOURCE
#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <limits.h>
#include <mntent.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/vfs.h>
#include <unistd.h>

#ifndef DISPLAY_LINKER_CONFIG
#error Build with build-launch.sh so the reviewed linker configuration is embedded
#endif

static char root[PATH_MAX];
static void die(const char *what) { perror(what); exit(1); }
static void require(int yes, const char *what) {
 if (!yes) { errno=EINVAL; die(what); }
}
static void target(char out[PATH_MAX],const char *path) {
 require(snprintf(out,PATH_MAX,"%s%s",root,path)<PATH_MAX,"target path too long");
}
static void mkdir_in(const char *path) {
 char out[PATH_MAX];target(out,path);
 if(mkdir(out,0755)!=0)die(out);
}
static void file_in(const char *path,const char *text) {
 char out[PATH_MAX];target(out,path);
 int fd=open(out,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
 if(fd<0)die(out);
 size_t len=strlen(text),at=0;
 while(at<len) {
  ssize_t n=write(fd,text+at,len-at);
  if(n<0 && errno==EINTR)continue;
  if(n<=0)die("write private file");
  at+=(size_t)n;
 }
 if(close(fd)!=0)die("close private file");
}
static int readonly_source(const char *path) {
 struct statvfs s;
 if(statvfs(path,&s)!=0)die(path);
 return (s.f_flag&ST_RDONLY)!=0;
}
static void bind_ro(const char *source,const char *dest,int directory,int source_must_ro) {
 require(source[0]=='/',"source must be absolute");
 // Pin the exact inode before mounting. The last component may not be a
 // symlink; intermediate paths are supplied by the supervising root only.
 int fd=open(source,O_PATH|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)die(source);
 struct stat st;
 if(fstat(fd,&st)!=0)die("inspect source");
 require(directory?S_ISDIR(st.st_mode):S_ISREG(st.st_mode),"source type");
 if(source_must_ro)require(readonly_source(source),"original filesystem must already be read-only");
 char pinned[64],out[PATH_MAX];target(out,dest);
 snprintf(pinned,sizeof(pinned),"/proc/self/fd/%d",fd);
 // Deliberately not recursive: submounts cannot smuggle writable content.
 if(mount(pinned,out,NULL,MS_BIND,NULL)!=0)die("bind private source");
 if(mount(NULL,out,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL)!=0)die("remount source read-only");
 if(!readonly_source(out)){errno=EROFS;die("read-only bind verification");}
 if(close(fd)!=0)die("close pinned source");
}
static void bind_vndbinder(void) {
 const char *source="/dev/vndbinder";
 // Recovery exposes the real binderfs node through this fixed symlink.
 // Resolve only the two reviewed locations, then pin its character inode;
 // the private target is a direct bind, not a symlink into an exposed /dev.
 char actual[PATH_MAX];
 if(!realpath(source,actual))die("resolve vndbinder source");
 require(!strcmp(actual,source) || !strcmp(actual,"/dev/binderfs/vndbinder"),"unexpected vndbinder source location");
 int fd=open(actual,O_PATH|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)die(source);
 struct stat before,after;
 if(fstat(fd,&before)!=0)die("inspect vndbinder source");
 require(S_ISCHR(before.st_mode),"vndbinder source must be a real character device");
 char pinned[64],out[PATH_MAX];target(out,source);
 snprintf(pinned,sizeof(pinned),"/proc/self/fd/%d",fd);
 if(mount(pinned,out,NULL,MS_BIND,NULL)!=0)die("bind vndbinder");
 // A separate child mount must omit NODEV so ProcessState can open the real
 // driver. RDONLY protects the mount, not character-device operations;
 // the probe's two stacked seccomp filters bound those operations instead.
 if(mount(NULL,out,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NOEXEC,NULL)!=0)die("remount vndbinder");
 struct statvfs flags;
 if(statvfs(out,&flags)!=0 || stat(out,&after)!=0)die("inspect vndbinder bind");
 require((flags.f_flag&ST_RDONLY) && !(flags.f_flag&ST_NODEV) &&
         S_ISCHR(after.st_mode) && before.st_rdev==after.st_rdev,"vndbinder bind verification");
 if(close(fd)!=0)die("close pinned vndbinder");
}
static int under(const char *path,const char *mountpoint) {
 size_t n=strlen(mountpoint);
 return !strncmp(path,mountpoint,n) && (n==1 || path[n]==0 || path[n]=='/');
}
static void persist_checked(const char *path) {
 char actual[PATH_MAX];
 if(!realpath(path,actual))die("persist display realpath");
 require(readonly_source(actual),"persist source must be read-only before launching");
 FILE *f=setmntent("/proc/mounts","r");
 if(!f)die("read persist mount options");
 struct mntent *m;size_t best=0;int safe=0;
 while((m=getmntent(f))) {
  size_t n=strlen(m->mnt_dir);
  if(n>=best && under(actual,m->mnt_dir)) {
   best=n;
   safe=!strcmp(m->mnt_type,"ext4") && hasmntopt(m,"ro") &&
        (hasmntopt(m,"noload") || hasmntopt(m,"norecovery"));
  }
 }
 endmntent(f);
 require(best && safe,"persist must be on ext4 mounted ro,noload (or norecovery)");
}
static void close_extra_fds(void) {
 DIR *dir=opendir("/proc/self/fd");
 if(!dir)die("list inherited descriptors");
 int listing=dirfd(dir);struct dirent *entry;
 while((entry=readdir(dir))) {
  char *end;long fd=strtol(entry->d_name,&end,10);
  if(!*end && fd>2 && fd!=listing)close((int)fd);
 }
 closedir(dir);
}
static void usage(void) {
 fprintf(stderr,"usage: probe-launch PROBE_ELF DF_PARAM_XML --uncalibrated|--persist-display DIR --list|--render LIST_INDEX QDCM_ID\n");
 exit(2);
}
int main(int argc,char **argv) {
 if(argc<5)usage();
 require(geteuid()==0,"diagnostic launcher requires root");
 const char *persist=NULL;int first=4;
 if(!strcmp(argv[3],"--persist-display")) {
  if(argc<6)usage();
  persist=argv[4];first=5;
 } else if(strcmp(argv[3],"--uncalibrated"))usage();
 int list=argc==first+1 && !strcmp(argv[first],"--list");
 int render=argc==first+3 && !strcmp(argv[first],"--render");
 if(!list && !render)usage();
 if(render)for(int i=first+1;i<argc;i++) {
  char *end;errno=0;long n=strtol(argv[i],&end,10);
  require(*argv[i] && !*end && !errno && n>=0 && n<=INT_MAX,"invalid mode number");
 }
 close_extra_fds();
 if(persist)persist_checked(persist);
 struct statfs tmp;
 if(statfs("/tmp",&tmp)!=0)die("inspect /tmp");
 require((unsigned long)tmp.f_type==0x01021994UL,"launcher scratch must be RAM tmpfs");
 if(unshare(CLONE_NEWNS)!=0)die("private mount namespace");
 if(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL)!=0)die("private mount propagation");
 strcpy(root,"/tmp/venus-display-probe-XXXXXX");
 if(!mkdtemp(root))die("private root");
 if(mount("display-probe",root,"tmpfs",MS_NOSUID|MS_NODEV,"mode=0755,size=2m")!=0)die("private RAM root");
 const char *dirs[]={"/system","/system_ext","/vendor","/odm","/apex",
  "/apex/com.android.runtime","/apex/com.android.vndk.v30","/linkerconfig",
  "/dev","/dev/__properties__","/tmp","/skyward","/skyward/runtime",
  "/data","/data/vendor","/data/vendor/display","/mnt","/mnt/vendor",
  "/mnt/vendor/persist","/mnt/vendor/persist/display","/persist","/persist/display"};
 for(size_t i=0;i<sizeof(dirs)/sizeof(*dirs);i++)mkdir_in(dirs[i]);
 file_in("/skyward/runtime/display-probe","");
 file_in("/dev/vndbinder","");
 file_in("/data/vendor/display/DF_Param.xml","");
 file_in("/linkerconfig/ld.config.txt",DISPLAY_LINKER_CONFIG);
 bind_ro("/mnt/skyward-ro/system/system","/system",1,1);
 bind_ro("/mnt/skyward-ro/system_ext","/system_ext",1,1);
 bind_ro("/mnt/skyward-ro/vendor","/vendor",1,1);
 bind_ro("/tmp/venus-runtime","/apex/com.android.runtime",1,0);
 bind_ro("/tmp/venus-vndk30","/apex/com.android.vndk.v30",1,0);
 bind_ro("/dev/__properties__","/dev/__properties__",1,0);
 bind_vndbinder();
 bind_ro(argv[1],"/skyward/runtime/display-probe",0,0);
 bind_ro(argv[2],"/data/vendor/display/DF_Param.xml",0,0);
 if(persist) {
  bind_ro(persist,"/mnt/vendor/persist/display",1,1);
  bind_ro(persist,"/persist/display",1,1);
 }
 // No /proc: Android's linker explicitly falls back to absolute argv[0].
 // No /sys, DRM or other Binder nodes: this experiment owns no panel.
 if(mount(NULL,root,NULL,MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL)!=0)die("seal private root");
 fprintf(stderr,"DISPLAY_PROBE_LAUNCH root=%s proc=absent data=DF_Param-only vndbinder=real-preinit-only persist=%s; remove empty root directory after exit\n",
         root,persist?"read-only-no-replay":"ABSENT-UNCALIBRATED");
 if(chroot(root)!=0 || chdir("/")!=0)die("enter private root");
 if(clearenv()!=0)die("clear loader environment");
 if(setenv("PATH","/system/bin",1)!=0)die("private PATH");
 char *args[5]={"/skyward/runtime/display-probe",argv[first],NULL,NULL,NULL};
 if(render){args[2]=argv[first+1];args[3]=argv[first+2];}
 execv(args[0],args);
 die("exec guarded display probe");
 return 1;
}
