// Synchronous readiness ping through the installed, verified Venus client ABI.
// The original library sends event 13 and returns 0 only for response event 14.
// Its poll timeout is not an overall deadline: the owner must kill/reap this
// subprocess when its own deadline expires. Never register a persistent client.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(int argc,char **argv) {
 (void)argv;
 if(argc!=1) {fputs("netmgr-ready takes no arguments\n",stderr);return 64;}
 // Keep the result channel exact even when vendor initialization emits its
 // diagnostics on stdout. All vendor output goes to the parent's error log.
 int result_fd=fcntl(STDOUT_FILENO,F_DUPFD_CLOEXEC,3);
 if(result_fd<0 || dup2(STDERR_FILENO,STDOUT_FILENO)<0) {
  perror("netmgr-ready result channel");return 74;
 }
 void *library=dlopen("/vendor/lib64/libnetmgr.so",RTLD_NOW|RTLD_LOCAL);
 if(!library) {fprintf(stderr,"netmgr-ready dlopen: %s\n",dlerror());return 70;}
 dlerror();
 typedef int (*ping_fn)(int timeout_ms);
 ping_fn ping=(ping_fn)dlsym(library,"netmgr_client_send_ping_msg");
 const char *error=dlerror();
 if(error || !ping) {fprintf(stderr,"netmgr-ready dlsym: %s\n",error?error:"null ping symbol");return 70;}
 const int ready=ping(500)==0;
 const char *answer=ready?"present\n":"absent\n";
 const size_t size=ready?8:7;
 if(write(result_fd,answer,size)!=(ssize_t)size) {perror("netmgr-ready write result");return 74;}
 if(close(result_fd)!=0) {perror("netmgr-ready close result");return 74;}
 return 0;
}
