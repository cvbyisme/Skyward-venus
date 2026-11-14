// Independent IPC supervisor. Never uses the global vendor Binder registry.
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
#include <linux/android/binderfs.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <poll.h>
#include <signal.h>
#include <time.h>

#define MANAGER_HASH_OUTPUT \
 "a6ad7a43eb91a5cd87e511f609a3671537eff7e39cfb5bd148f716b0779f9b4d  /vendor/bin/vndservicemanager\n" \
 "e0c155ca1f71d33ce8ff34d719de074d54d241611e2fd588bf4dac99378ea9e4  /vendor/etc/selinux/vndservice_contexts\n"

#ifndef DISPLAY_LINKER_CONFIG
#error Build with build-launch.sh so the reviewed linker configuration is embedded
#endif

static char root[PATH_MAX];
static pid_t owner_pid,sm_pid,qs_pid,client_pid;
static int cleanup_done;
static int owner_clean=1;
static int services_clean=1;
static void cleanup(void);
static void die(const char *what) { perror(what); cleanup(); exit(1); }
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

static volatile sig_atomic_t interrupted;
static void stopping(int sig) { (void)sig; interrupted=1; }
static long long now_ms(void) {
 struct timespec t;if(clock_gettime(CLOCK_MONOTONIC,&t))die("monotonic clock");
 return (long long)t.tv_sec*1000+t.tv_nsec/1000000;
}
static int reaped(pid_t *pid,int *status) {
 if(!*pid)return 1;
 pid_t got=waitpid(*pid,status,WNOHANG);
 if(got==*pid){*pid=0;return 1;}
 if(got<0&&errno==ECHILD){*pid=0;return 1;}
 return 0;
}
static void stop_child(pid_t *pid,const char *name) {
 int status=0;
 if(!*pid)return;
 int is_owner=!strcmp(name,"kms-owner");
 if(reaped(pid,&status)){
  if(is_owner&&(!WIFEXITED(status)||WEXITSTATUS(status)!=0))owner_clean=0;
  if(!strcmp(name,"qservice")||!strcmp(name,"vendor-manager"))services_clean=0;
  return;
 }
 fprintf(stderr,"DISPLAY_IPC_STOP role=%s pid=%d\n",name,*pid);
 kill(*pid,SIGTERM);long long end=now_ms()+(is_owner?5500:2500);
 while(!reaped(pid,&status)&&now_ms()<end)poll(NULL,0,20);
 if(*pid){fprintf(stderr,"DISPLAY_IPC_KILL role=%s pid=%d graceful_cleanup=false\n",name,*pid);services_clean=0;kill(*pid,SIGKILL);
  end=now_ms()+1000;while(!reaped(pid,&status)&&now_ms()<end)poll(NULL,0,20);}
 if(*pid)fprintf(stderr,"DISPLAY_IPC_UNREAPED role=%s pid=%d\n",name,*pid);
 if(is_owner&&(*pid||!WIFEXITED(status)||WEXITSTATUS(status)!=0))owner_clean=0;
 if(!strcmp(name,"qservice")&&(!WIFEXITED(status)||WEXITSTATUS(status)!=0))services_clean=0;
}
static void cleanup(void) {
 if(cleanup_done)return;
 cleanup_done=1;
 stop_child(&client_pid,"client");stop_child(&qs_pid,"qservice");
 stop_child(&sm_pid,"vendor-manager");stop_child(&owner_pid,"kms-owner");
}
static void child_parent(pid_t parent) {
 // These are exact owned children. If the supervisor is killed, leave no
 // service behind. The owner gets TERM so it can settle its baseline.
 if(prctl(PR_SET_PDEATHSIG,SIGTERM)!=0||getppid()!=parent)_exit(125);
 signal(SIGTERM,SIG_DFL);signal(SIGINT,SIG_DFL);signal(SIGALRM,SIG_DFL);
 owner_pid=sm_pid=qs_pid=client_pid=0;cleanup_done=1;
}
static void child_root(void) {
 if(chroot(root)||chdir("/")||clearenv()||setenv("PATH","/system/bin",1))_exit(125);
}
static void private_binder(void) {
 char base[PATH_MAX],control[PATH_MAX],node[PATH_MAX],dest[PATH_MAX];
 target(base,"/.binder");target(control,"/.binder/binder-control");target(node,"/.binder/skyward-display");target(dest,"/dev/vndbinder");
 // Some Android kernels populate their configured default devices on each
 // fresh mount. Reserve room, then allocate our own distinct context name.
 if(mount("binder",base,"binder",MS_NOSUID|MS_NOEXEC,"max=8"))die("create independent binderfs");
 int fd=open(control,O_RDONLY|O_CLOEXEC);if(fd<0)die("open private binder-control");
 struct binderfs_device device={0};strcpy(device.name,"skyward-display");
 if(ioctl(fd,BINDER_CTL_ADD,&device))die("allocate private Binder context");
 close(fd);
 struct stat created,global;if(stat(node,&created)||stat("/dev/vndbinder",&global))die("inspect private Binder identity");
 require(S_ISCHR(created.st_mode)&&created.st_rdev!=global.st_rdev,"private Binder must differ from global device");
 if(mount(node,dest,NULL,MS_BIND,NULL)||mount(NULL,dest,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NOEXEC,NULL))die("bind private vendor Binder");
 // Keep only the one driver inode; no client can allocate another context.
 if(umount(base))die("hide binderfs control mount");
 fprintf(stderr,"DISPLAY_IPC_BINDER private_major=%u private_minor=%u global_registry=untouched\n",device.major,device.minor);
}
static void socket_bind(const char *source) {
 struct stat s;if(lstat(source,&s))die("owner socket stat");
 require(S_ISSOCK(s.st_mode)&&s.st_uid==0&&(s.st_mode&077)==0,"owner socket type/mode");
 char out[PATH_MAX];target(out,"/run/owner.sock");
 if(mount(source,out,NULL,MS_BIND,NULL)||mount(NULL,out,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC,NULL))die("bind owner socket");
}
static void original_manager_hash(void) {
 // This original executable/context table runs before the STC client's own
 // mandatory 35-file check, so validate it independently before execution.
 int fds[2];if(pipe2(fds,O_CLOEXEC))die("manager hash pipe");pid_t parent=getpid(),child=fork();if(child<0)die("manager hash fork");
 if(!child){child_parent(parent);alarm(8);close(fds[0]);if(dup2(fds[1],1)<0)_exit(125);close(fds[1]);child_root();
  char *args[]={"/system/bin/toybox","sha256sum","/vendor/bin/vndservicemanager","/vendor/etc/selinux/vndservice_contexts",NULL};execv(args[0],args);_exit(126);}
 close(fds[1]);char output[1024];size_t used=0;long long end=now_ms()+9000;int status=0;
 while(now_ms()<end&&!interrupted){struct pollfd p={fds[0],POLLIN,0};int n=poll(&p,1,100);
  if(n<0&&errno==EINTR)continue;
  if(n<0)break;
  if(p.revents&(POLLIN|POLLHUP)){ssize_t got=read(fds[0],output+used,sizeof(output)-1-used);if(got<=0)break;used+=(size_t)got;if(used==sizeof(output)-1)break;}}
 close(fds[0]);output[used]=0;
 long long reap_end=now_ms()+500;while(!reaped(&child,&status)&&now_ms()<reap_end)poll(NULL,0,10);
 int hash_timed_out=child!=0;
 if(child)stop_child(&child,"hash-verifier");
 require(!hash_timed_out&&WIFEXITED(status)&&WEXITSTATUS(status)==0,"manager hash child failed or timed out");
 require(!strcmp(output,MANAGER_HASH_OUTPUT),"original vendor manager or service contexts hash mismatch");
}
int main(int argc,char **argv) {
 if(argc!=8||strcmp(argv[5],"--persist-display")||strcmp(argv[7],"--list")){
  fprintf(stderr,"usage: display-ipc-launch OWNER QSERVICE CLIENT DF_PARAM --persist-display ORIGINAL_RO_DISPLAY_DIR --list\n");return 2;}
 require(geteuid()==0,"IPC launcher requires root");
 for(int i=1;i<=4;i++)require(argv[i][0]=='/',"artifact paths must be absolute");
 close_extra_fds();persist_checked(argv[6]);
 signal(SIGTERM,stopping);signal(SIGINT,stopping);
 struct statfs tmp;if(statfs("/tmp",&tmp))die("tmpfs check");require((unsigned long)tmp.f_type==0x01021994UL,"scratch must be RAM");
 if(unshare(CLONE_NEWNS)||mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))die("private mount namespace");
 strcpy(root,"/tmp/venus-display-ipc-XXXXXX");if(!mkdtemp(root))die("private root");
 if(mount("display-ipc",root,"tmpfs",MS_NOSUID|MS_NODEV,"mode=0755,size=2m"))die("root tmpfs");
 const char *dirs[]={"/system","/system_ext","/vendor","/odm","/apex","/apex/com.android.runtime","/apex/com.android.vndk.v30",
  "/linkerconfig","/dev","/dev/__properties__","/run","/tmp","/skyward","/skyward/runtime","/data","/data/vendor","/data/vendor/display",
  "/mnt","/mnt/vendor","/mnt/vendor/persist","/mnt/vendor/persist/display","/persist","/persist/display","/.binder","/proc","/sys","/sys/fs","/sys/fs/selinux"};
 for(size_t i=0;i<sizeof(dirs)/sizeof(*dirs);i++)mkdir_in(dirs[i]);
 file_in("/skyward/runtime/display-ipc-qservice","");file_in("/skyward/runtime/display-ipc-client","");
 file_in("/run/owner.sock","");file_in("/dev/vndbinder","");file_in("/data/vendor/display/DF_Param.xml","");
 file_in("/linkerconfig/ld.config.txt",DISPLAY_LINKER_CONFIG);
 bind_ro("/mnt/skyward-ro/system/system","/system",1,1);bind_ro("/mnt/skyward-ro/system_ext","/system_ext",1,1);
 bind_ro("/mnt/skyward-ro/vendor","/vendor",1,1);bind_ro("/tmp/venus-runtime","/apex/com.android.runtime",1,0);
 bind_ro("/tmp/venus-vndk30","/apex/com.android.vndk.v30",1,0);bind_ro("/dev/__properties__","/dev/__properties__",1,0);
 bind_ro(argv[2],"/skyward/runtime/display-ipc-qservice",0,0);bind_ro(argv[3],"/skyward/runtime/display-ipc-client",0,0);
 bind_ro(argv[4],"/data/vendor/display/DF_Param.xml",0,0);bind_ro(argv[6],"/mnt/vendor/persist/display",1,1);bind_ro(argv[6],"/persist/display",1,1);
 private_binder();original_manager_hash();
 char run[PATH_MAX]="/tmp/venus-display-owner-XXXXXX";if(!mkdtemp(run))die("owner private RAM directory");
 char sock[PATH_MAX];require(snprintf(sock,sizeof(sock),"%s/owner.sock",run)<(int)sizeof(sock),"socket path");
 pid_t parent=getpid();owner_pid=fork();if(owner_pid<0)die("fork owner");
 if(!owner_pid){child_parent(parent);if(clearenv()||setenv("PATH","/system/bin",1))_exit(125);
  char *args[]={argv[1],sock,"--baseline",NULL};execv(args[0],args);_exit(126);}
 long long end=now_ms()+10000;struct stat s;int status=0;
 while(lstat(sock,&s)!=0){require(!interrupted&&now_ms()<end&&!reaped(&owner_pid,&status),"owner did not initialize its socket");poll(NULL,0,50);}
 socket_bind(sock);
 if(mount(NULL,root,NULL,MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL))die("seal IPC root");
 sm_pid=fork();if(sm_pid<0)die("fork vendor manager");
 if(!sm_pid){child_parent(parent);
  // Only the trusted original manager sees real SELinux and /proc identity.
  // Its private mount namespace is not shared with QService or vendor plugins.
  if(unshare(CLONE_NEWNS)||mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))_exit(125);
  char proc[PATH_MAX];target(proc,"/proc");if(mount("proc",proc,"proc",MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC,NULL))_exit(125);
  bind_ro("/sys/fs/selinux","/sys/fs/selinux",1,0);child_root();
  char *args[]={"/vendor/bin/vndservicemanager","/dev/vndbinder",NULL};execv(args[0],args);_exit(126);}
 int ready[2];if(pipe2(ready,O_CLOEXEC))die("qservice ready pipe");
 pid_t expected_owner=owner_pid;
 qs_pid=fork();if(qs_pid<0)die("fork qservice");
 if(!qs_pid){child_parent(parent);close(ready[0]);int fd=fcntl(ready[1],F_DUPFD,3);if(fd<0)_exit(125);close(ready[1]);
  char pidbuf[24],fdbuf[24];snprintf(pidbuf,sizeof(pidbuf),"%d",expected_owner);snprintf(fdbuf,sizeof(fdbuf),"%d",fd);
  child_root();char *args[]={"/skyward/runtime/display-ipc-qservice","/run/owner.sock",pidbuf,fdbuf,NULL};execv(args[0],args);_exit(126);}
 close(ready[1]);end=now_ms()+10000;int acknowledged=0;
 while(!acknowledged&&!interrupted&&now_ms()<end){struct pollfd p={ready[0],POLLIN,0};int n=poll(&p,1,50);
  if(n>0&&(p.revents&POLLIN)){char c;acknowledged=read(ready[0],&c,1)==1&&c=='R';break;}
  require(!reaped(&sm_pid,&status)&&!reaped(&owner_pid,&status)&&!reaped(&qs_pid,&status),"service exited before readiness");}
 close(ready[0]);require(acknowledged,"qservice ready timeout");
 client_pid=fork();if(client_pid<0)die("fork IPC client");
 if(!client_pid){child_parent(parent);child_root();char *args[]={"/skyward/runtime/display-ipc-client","--list",NULL};execv(args[0],args);_exit(126);}
 fprintf(stderr,"DISPLAY_IPC_STARTED root=%s owner_run=%s owner=%d manager=%d qservice=%d client=%d SELinux=unchanged\n",root,run,owner_pid,sm_pid,qs_pid,client_pid);
 end=now_ms()+35000;int client_status=-1;
 while(!interrupted&&now_ms()<end){
  if(reaped(&client_pid,&status)){client_status=status;break;}
  if(reaped(&owner_pid,&status)||reaped(&sm_pid,&status)||reaped(&qs_pid,&status))break;
  poll(NULL,0,30);
 }
 int success=client_status>=0&&WIFEXITED(client_status)&&WEXITSTATUS(client_status)==0;
 cleanup();success=success&&owner_clean&&services_clean;
 if(rmdir(run))fprintf(stderr,"DISPLAY_IPC_OWNER_DIR_REMAINS %s\n",run);
 fprintf(stderr,"DISPLAY_IPC_FINISHED success=%d root=%s remove_empty_root_after_exit=true\n",success,root);
 return success?0:1;
}
