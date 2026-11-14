#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
static volatile sig_atomic_t stop;
static void finish(int sig) { (void)sig; stop=1; }
static int remove_receipt(const char *path,const struct stat *owned) {
    struct stat current;
    if(lstat(path,&current)!=0 || !S_ISREG(current.st_mode) ||
       current.st_dev!=owned->st_dev || current.st_ino!=owned->st_ino) {errno=ESTALE;return -1;}
    return unlink(path);
}
int main(void) {
    const char *ready="/tmp/venus-modem-held.ready";
    struct stat before,after,owned;
    if(geteuid()!=0 || access("/skyward/venus-recovery.txt",R_OK)!=0 ||
       access(ready,F_OK)==0 || lstat("/dev/subsys_modem",&before)!=0 || !S_ISCHR(before.st_mode)) return 2;
    struct sigaction action={0}; action.sa_handler=finish;
    if(sigaction(SIGTERM,&action,NULL)!=0 || sigaction(SIGINT,&action,NULL)!=0 ||
       sigaction(SIGALRM,&action,NULL)!=0) {perror("install modem stop handlers");return 1;}
    alarm(45);
    puts("MODEM_PHASE open kernel subsystem reference");fflush(stdout);
    int fd=open("/dev/subsys_modem",O_RDONLY|O_CLOEXEC|O_NOFOLLOW);
    if(fd<0) {perror("open modem subsystem");return 1;}
    alarm(0);
    if(stop || fstat(fd,&after)!=0 || !S_ISCHR(after.st_mode) || before.st_rdev!=after.st_rdev) {close(fd);return 1;}
    sigset_t blocked,previous,waiting;
    sigemptyset(&blocked);sigaddset(&blocked,SIGTERM);sigaddset(&blocked,SIGINT);sigaddset(&blocked,SIGALRM);
    if(sigprocmask(SIG_BLOCK,&blocked,&previous)!=0) {perror("block modem stop signals");close(fd);return 1;}
    if(stop) {close(fd);return 1;}
    waiting=previous;sigdelset(&waiting,SIGTERM);sigdelset(&waiting,SIGINT);sigdelset(&waiting,SIGALRM);
    int receipt=open(ready,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
    if(receipt<0) {close(fd);return 1;}
    if(fstat(receipt,&owned)!=0) {perror("inspect modem holder receipt");close(receipt);close(fd);return 1;}
    if(dprintf(receipt,"%d\n",(int)getpid())<0) {
        perror("write modem holder receipt");close(receipt);remove_receipt(ready,&owned);close(fd);return 1;
    }
    if(close(receipt)!=0) {perror("close modem holder receipt");remove_receipt(ready,&owned);close(fd);return 1;}
    puts("MODEM_PHASE subsystem reference held; query QMI separately for readiness");fflush(stdout);
    // Blocked between the flag check and sigsuspend's atomic unblock/wait:
    // TERM cannot arrive in the old check-then-pause lost-wakeup window.
    while(!stop) sigsuspend(&waiting);
    if(close(fd)!=0) {perror("release modem subsystem reference");return 1;}
    if(remove_receipt(ready,&owned)!=0) {
        perror("release owned modem receipt");return 1;
    }
    puts("MODEM_PHASE subsystem reference released");return 0;
}
