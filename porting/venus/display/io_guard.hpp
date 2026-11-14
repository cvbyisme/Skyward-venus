// No display/driver I/O during parameter-generation experiments.
#pragma once
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <stdexcept>
#include <vector>
namespace venus_display {
#if defined(__aarch64__)
inline constexpr uint32_t native_audit_arch=AUDIT_ARCH_AARCH64;
#elif defined(__x86_64__)
inline constexpr uint32_t native_audit_arch=AUDIT_ARCH_X86_64;
#else
#error Unsupported experiment architecture
#endif
inline void guard_check(bool ok,const char* why){if(!ok)throw std::runtime_error(why);}
inline void restrict_io() {
  // Before dlopen/constructors: no ioctl (DRM/DSI/Binder included), no writable
  // opens, namespace changes, process execution, or normal file mutation.
  // This is an experiment guard, not an untrusted-code security boundary.
  std::vector<sock_filter> f;
  auto stmt=[&](uint16_t code,uint32_t k){f.push_back(BPF_STMT(code,k));};
  auto jump=[&](uint16_t code,uint32_t k,uint8_t t,uint8_t e){f.push_back(BPF_JUMP(code,k,t,e));};
  stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,arch));
  jump(BPF_JMP|BPF_JEQ|BPF_K,native_audit_arch,1,0);
  stmt(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS);
  stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,nr));
  for(int n:{__NR_ioctl,__NR_execve,__NR_execveat,__NR_mount,__NR_umount2,
             __NR_unlinkat,__NR_renameat,__NR_renameat2,__NR_linkat,__NR_symlinkat,
             __NR_mkdirat,__NR_mknodat,__NR_truncate,__NR_ftruncate,
             __NR_fchmod,__NR_fchmodat,__NR_fchown,__NR_fchownat,
             __NR_setxattr,__NR_lsetxattr,__NR_fsetxattr,__NR_removexattr,
             __NR_lremovexattr,__NR_fremovexattr}) {
    jump(BPF_JMP|BPF_JEQ|BPF_K,n,0,1);stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
  }
#ifdef __NR_openat2
  jump(BPF_JMP|BPF_JEQ|BPF_K,__NR_openat2,0,1);stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
#endif
  jump(BPF_JMP|BPF_JEQ|BPF_K,__NR_openat,0,4);
  stmt(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,args[2]));
  jump(BPF_JMP|BPF_JSET|BPF_K,O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND,0,1);
  stmt(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM);
  stmt(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
  stmt(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
  sock_fprog prog{static_cast<unsigned short>(f.size()),f.data()};
  guard_check(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)==0,"no_new_privs failed");
  guard_check(prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&prog)==0,"seccomp guard failed");
}
}
