// Separate IPC experiment guard. The original all-ioctl guard is unchanged.
#pragma once
#include "../io_guard.hpp"
#include <linux/android/binder.h>
namespace display_ipc {
inline void restrict_to_binder(int binder_fd) {
 using namespace venus_display;
 std::vector<sock_filter> f;
 auto stmt=[&](uint16_t code,uint32_t k){f.push_back(BPF_STMT(code,k));};
 auto jump=[&](uint16_t code,uint32_t k,uint8_t t,uint8_t e){f.push_back(BPF_JUMP(code,k,t,e));};
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,arch));
 jump(BPF_JMP|BPF_JEQ|BPF_K,native_audit_arch,1,0);stmt(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS);
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,nr));
 // Only this already opened real Binder descriptor can transact or leave a
 // looper. No DRM, DSI, input, context-manager or binderfs-control ioctl.
 jump(BPF_JMP|BPF_JEQ|BPF_K,__NR_ioctl,0,9);
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,args[0]));
 jump(BPF_JMP|BPF_JEQ|BPF_K,static_cast<uint32_t>(binder_fd),1,0);
 stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,args[1]));
 jump(BPF_JMP|BPF_JEQ|BPF_K,BINDER_WRITE_READ,2,0);
 jump(BPF_JMP|BPF_JEQ|BPF_K,BINDER_THREAD_EXIT,1,0);
 stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
 stmt(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,nr));
 for(int n:{__NR_execve,__NR_execveat,__NR_mount,__NR_umount2,
    __NR_socket,__NR_socketpair,__NR_connect,__NR_bind,__NR_listen,__NR_accept,__NR_accept4,
    __NR_unlinkat,__NR_renameat,__NR_renameat2,__NR_linkat,__NR_symlinkat,
    __NR_mkdirat,__NR_mknodat,__NR_truncate,__NR_ftruncate,
    __NR_fchmod,__NR_fchmodat,__NR_fchown,__NR_fchownat,
    __NR_setxattr,__NR_lsetxattr,__NR_fsetxattr,__NR_removexattr,__NR_lremovexattr,__NR_fremovexattr}) {
    jump(BPF_JMP|BPF_JEQ|BPF_K,n,0,1);stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
 }
#ifdef __NR_openat2
 jump(BPF_JMP|BPF_JEQ|BPF_K,__NR_openat2,0,1);stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
#endif
 jump(BPF_JMP|BPF_JEQ|BPF_K,__NR_openat,0,4);
 stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,args[2]));
 jump(BPF_JMP|BPF_JSET|BPF_K,O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND,0,1);
 stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);stmt(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
 stmt(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
 sock_fprog prog{static_cast<unsigned short>(f.size()),f.data()};
 guard_check(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)==0,"no_new_privs failed");
 // Install before any Binder pool is started: every later thread inherits it.
 guard_check(prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&prog)==0,"Binder-only seccomp failed");
}
}
