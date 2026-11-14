// Host regression test for the exact launcher's directory setup.
// Never invokes the launcher entrypoint, mounts, HALs, or device operations.
#define main km41_launcher_entrypoint
#include "km41-launch.c"
#undef main
#include <sys/wait.h>

static void expect_mode(const char *p, mode_t expected) {
 struct stat st;
 if(lstat(p,&st)!=0 || !S_ISDIR(st.st_mode) || (st.st_mode & 0777)!=expected) {
  fprintf(stderr,"directory mode test failed\n");exit(1);
 }
}
static void expect_refusal(const char *p) {
 pid_t child=fork();
 if(child<0)fail("fork test");
 if(child==0) {
  int nullfd=open("/dev/null",O_WRONLY);
  if(nullfd>=0){dup2(nullfd,2);close(nullfd);}
  directory(p,0755);_exit(0);
 }
 int status;
 if(waitpid(child,&status,0)!=child || !WIFEXITED(status) || WEXITSTATUS(status)==0) {
  fprintf(stderr,"non-directory path was not refused\n");exit(1);
 }
}
int main(void) {
 char base[]="/tmp/km41-dirtest.XXXXXX";
 if(!mkdtemp(base))fail("mkdtemp test");
 char stage[PATH_MAX],inside[PATH_MAX],link[PATH_MAX],file[PATH_MAX];
 snprintf(stage,sizeof(stage),"%s/private",base);
 snprintf(inside,sizeof(inside),"%s/private/root",base);
 snprintf(link,sizeof(link),"%s/link",base);
 snprintf(file,sizeof(file),"%s/file",base);
 mode_t old=umask(077);
 directory(stage,0700);expect_mode(stage,0700);
 directory(inside,0755);expect_mode(inside,0755);
 if(chmod(inside,0700)!=0)fail("chmod test");
 directory(inside,0755);expect_mode(inside,0755);
 if(symlink(inside,link)!=0)fail("symlink test");
 expect_refusal(link);
 int fd=open(file,O_WRONLY|O_CREAT|O_EXCL,0600);
 if(fd<0)fail("create file test");
 close(fd);expect_refusal(file);
 umask(old);
 if(unlink(link)||unlink(file)||rmdir(inside)||rmdir(stage)||rmdir(base))fail("cleanup test");
 puts("KM41_DIRECTORY_TESTS_PASS count=5; umask077 private-parent searchable-chroot non-directory-refusal");
 return 0;
}
