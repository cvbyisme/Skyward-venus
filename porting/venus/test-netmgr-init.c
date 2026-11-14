// Tests the production directory/file helpers against real host tmpfs.
// Does not invoke launcher main, mount anything, or contact the phone.
#define main km41_launcher_entrypoint
#include "km41-launch.c"
#undef main
#include <assert.h>
#include <sys/wait.h>

static void refuse(int parent,const char *name,uid_t uid,gid_t gid,mode_t mode,int file) {
 pid_t pid=fork();if(pid<0)fail("test fork");
 if(pid==0) {
  int nullfd=open("/dev/null",O_WRONLY);if(nullfd>=0){dup2(nullfd,2);close(nullfd);}
  if(file)netmgr_recovery_file(parent,uid,gid);
  else close(netmgr_directory_at(parent,name,uid,gid,mode,1));
  _exit(0);
 }
 int status;
 assert(waitpid(pid,&status,0)==pid && WIFEXITED(status) && WEXITSTATUS(status)!=0);
}
static void bytes_preserved(int parent) {
 int fd=openat(parent,"recovery_info",O_RDONLY|O_NOFOLLOW|O_CLOEXEC);assert(fd>=0);
 char bytes[16]={0};assert(read(fd,bytes,sizeof(bytes))==9 && !memcmp(bytes,"previous\n",9));close(fd);
}
int main(void) {
 char path[]="/dev/shm/venus-netmgr-test.XXXXXX";
 assert(mkdtemp(path));
 int base=open(path,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);assert(base>=0);
 uid_t uid=geteuid();gid_t gid=getegid();
 mode_t old=umask(077);
 int socket_dir=netmgr_directory_at(base,"netmgr",uid,gid,0750,1);
 struct stat st;assert(fstat(socket_dir,&st)==0 && (st.st_mode&0777)==0750);
 close(netmgr_directory_at(base,"netmgr",uid,gid,0750,1));
 refuse(base,"netmgr",uid+1,gid,0750,0); // no takeover of another owner
 assert(fstat(socket_dir,&st)==0 && st.st_uid==uid && (st.st_mode&0777)==0750);
 assert(fchmod(socket_dir,0777)==0);refuse(base,"netmgr",uid,gid,0750,0);
 assert(fstat(socket_dir,&st)==0 && (st.st_mode&0777)==0777);assert(fchmod(socket_dir,0750)==0);
 assert(symlinkat("netmgr",base,"alias")==0);refuse(base,"alias",uid,gid,0750,0);
 int plain=openat(base,"plain",O_WRONLY|O_CREAT|O_EXCL,0600);assert(plain>=0);close(plain);
 refuse(base,"plain",uid,gid,0750,0);

 int recovery=netmgr_directory_at(socket_dir,"recovery",uid,gid,0770,1);
 netmgr_recovery_file(recovery,uid,gid);
 int fd=openat(recovery,"recovery_info",O_WRONLY|O_NOFOLLOW|O_CLOEXEC);assert(fd>=0);
 assert(fstat(fd,&st)==0 && st.st_size==0 && st.st_nlink==1 && (st.st_mode&0777)==0600);
 assert(write(fd,"previous\n",9)==9);close(fd);
 netmgr_recovery_file(recovery,uid,gid);bytes_preserved(recovery);
 refuse(recovery,NULL,uid+1,gid,0,1);bytes_preserved(recovery);
 assert(fchmodat(recovery,"recovery_info",0666,0)==0);refuse(recovery,NULL,uid,gid,0,1);
 assert(fstatat(recovery,"recovery_info",&st,AT_SYMLINK_NOFOLLOW)==0 && (st.st_mode&0777)==0666);
 assert(fchmodat(recovery,"recovery_info",0600,0)==0);bytes_preserved(recovery);
 assert(linkat(recovery,"recovery_info",recovery,"hardlink",0)==0);
 refuse(recovery,NULL,uid,gid,0,1);bytes_preserved(recovery);assert(unlinkat(recovery,"hardlink",0)==0);
 assert(renameat(recovery,"recovery_info",recovery,"saved")==0);
 assert(symlinkat("saved",recovery,"recovery_info")==0);refuse(recovery,NULL,uid,gid,0,1);
 assert(unlinkat(recovery,"recovery_info",0)==0);
 assert(mkfifoat(recovery,"recovery_info",0600)==0);refuse(recovery,NULL,uid,gid,0,1);
 assert(unlinkat(recovery,"recovery_info",0)==0);
 assert(mkdirat(recovery,"recovery_info",0700)==0);refuse(recovery,NULL,uid,gid,0,1);
 assert(unlinkat(recovery,"recovery_info",AT_REMOVEDIR)==0);
 assert(renameat(recovery,"saved",recovery,"recovery_info")==0);bytes_preserved(recovery);
 // A regular non-RAM filesystem must be rejected before creating a path.
 int disk=open(".",O_RDONLY|O_DIRECTORY|O_CLOEXEC);assert(disk>=0);
 struct statfs fs;assert(fstatfs(disk,&fs)==0);
 if((unsigned long)fs.f_type!=0x01021994UL) {
  assert(faccessat(disk,"must-not-create-netmgr-test",F_OK,AT_SYMLINK_NOFOLLOW)!=0 && errno==ENOENT);
  refuse(disk,"must-not-create-netmgr-test",uid,gid,0750,0);
  assert(faccessat(disk,"must-not-create-netmgr-test",F_OK,AT_SYMLINK_NOFOLLOW)!=0 && errno==ENOENT);
 }
 close(disk);
 assert(unlinkat(recovery,"recovery_info",0)==0);close(recovery);
 assert(unlinkat(socket_dir,"recovery",AT_REMOVEDIR)==0);close(socket_dir);
 assert(unlinkat(base,"netmgr",AT_REMOVEDIR)==0);
 assert(unlinkat(base,"plain",0)==0 && unlinkat(base,"alias",0)==0);close(base);
 assert(rmdir(path)==0);umask(old);
 puts("PASS: RAM netmgr paths, exact modes, no takeover/truncation, symlink/hardlink/FIFO/type/filesystem refusal");
}
