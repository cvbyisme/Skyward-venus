#pragma once
#include "protocol.hpp"
#include <cerrno>
#include <chrono>
#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
namespace display_ipc {
inline void wait_fd(int fd,short events,int milliseconds=5000) {
 const auto end=std::chrono::steady_clock::now()+std::chrono::milliseconds(milliseconds);
 for(;;){auto left=std::chrono::duration_cast<std::chrono::milliseconds>(end-std::chrono::steady_clock::now()).count();
  check(left>0,"owner IPC deadline");pollfd p{fd,events,0};int n=poll(&p,1,static_cast<int>(left));
  if(n<0&&errno==EINTR)continue;
  check(n>0,"owner IPC wait failed or timed out");
  check(!(p.revents&(POLLERR|POLLNVAL)),"owner IPC failed");
  if(p.revents&events)return;
  check(!(p.revents&POLLHUP),"owner IPC exited");}
}
inline Message receive(int fd,int milliseconds=5000){wait_fd(fd,POLLIN,milliseconds);std::array<uint8_t,size+1>b{};
 ssize_t n=recv(fd,b.data(),b.size(),MSG_TRUNC|MSG_DONTWAIT);check(n==static_cast<ssize_t>(size),"owner packet truncated or peer gone");return decode(b.data(),n);}
inline void transmit(int fd,const Message& m){const auto b=encode(m);wait_fd(fd,POLLOUT);
 check(send(fd,b.data(),b.size(),MSG_DONTWAIT|MSG_NOSIGNAL)==static_cast<ssize_t>(b.size()),"owner packet send failed");}
class Owner {
 int fd_=-1; uint64_t seq_=0;
public:
 Message last;
 Owner(const char* path,pid_t pid) {
  check(path[0]=='/'&&std::strlen(path)<sizeof(sockaddr_un::sun_path),"owner socket path");
  fd_=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);check(fd_>=0,"owner socket");
  try{sockaddr_un a{};a.sun_family=AF_UNIX;std::strcpy(a.sun_path,path);
   check(connect(fd_,reinterpret_cast<sockaddr*>(&a),sizeof(a))==0,"connect owner");
   ucred c{};socklen_t n=sizeof(c);check(getsockopt(fd_,SOL_SOCKET,SO_PEERCRED,&c,&n)==0&&n==sizeof(c)&&c.uid==0&&c.pid==pid,"owner credentials changed");
   last=receive(fd_);check(last.kind==hello&&last.seq==0&&last.generation!=0&&last.commit>=1&&last.op==0&&last.status==0&&last.width>0&&last.height>0&&last.state==ready,"owner is not physically initialized");
   call(ping);
  }catch(...){close(fd_);fd_=-1;throw;}
 }
 ~Owner(){if(fd_>=0)close(fd_);}
 Owner(const Owner&)=delete;
 int fd()const{return fd_;}
 Message call(uint32_t op){check(fd_>=0,"owner link poisoned");
  try{Message m{};m.kind=request;m.seq=++seq_;m.generation=last.generation;m.op=op;
   transmit(fd_,m);auto r=receive(fd_);validate_reply(r,m,last);last=r;return r;
  }catch(...){close(fd_);fd_=-1;throw;}
 }
};
}
