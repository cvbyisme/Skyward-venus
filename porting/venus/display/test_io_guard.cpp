#include "io_guard.hpp"
#include <cstdio>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>

int main(){
  int fds[2];if(pipe(fds)!=0)return 1;
  pid_t child=fork();if(child<0)return 2;
  if(child==0){
    close(fds[0]);
    try{venus_display::restrict_io();}catch(...){_exit(3);}
    int fd=open("/dev/null",O_RDONLY);if(fd<0)_exit(4);
    errno=0;if(ioctl(fd,0)!=-1 || errno!=EPERM)_exit(5);close(fd);
    errno=0;if(open("/dev/null",O_WRONLY)!=-1 || errno!=EPERM)_exit(6);
    errno=0;if(open("/tmp/venus-color-guard-must-not-create",O_CREAT|O_RDONLY,0600)!=-1 || errno!=EPERM)_exit(7);
    errno=0;if(unlinkat(AT_FDCWD,"/tmp/venus-color-guard-must-not-remove",0)!=-1 || errno!=EPERM)_exit(8);
    char* args[]={const_cast<char*>("/bin/true"),nullptr};errno=0;
    if(execv(args[0],args)!=-1 || errno!=EPERM)_exit(9);
    if(write(fds[1],"ok",2)!=2)_exit(10);
    _exit(0);
  }
  close(fds[1]);char data[2];ssize_t n=read(fds[0],data,2);close(fds[0]);
  int status=0;if(waitpid(child,&status,0)!=child || !WIFEXITED(status) || WEXITSTATUS(status)!=0 || n!=2)return 11;
  std::puts("PASS: no ioctl/write-open/create/unlink/exec; reads and inherited output work");
}
