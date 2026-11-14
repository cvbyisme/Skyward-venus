#include "transport.hpp"
#include "binder_guard.hpp"
#include <cassert>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <fcntl.h>
using namespace display_ipc;
template<class F>void rejects(F f){bool rejected=false;try{f();}catch(const std::exception&){rejected=true;}assert(rejected);}
int main(){
 Message m{request,2,7,0,resume,0,0,0,0};auto bytes=encode(m);
 assert(bytes[0]=='V'&&bytes[6]==2&&bytes[8]==2&&bytes[16]==7&&bytes[32]==3);
 assert(decode(bytes.data(),bytes.size()).generation==7);
 for(size_t n=0;n<size;n++)rejects([&]{decode(bytes.data(),n);});
 bytes[63]=1;rejects([&]{decode(bytes.data(),bytes.size());});
 Message before{hello,0,7,1,0,0,1440,3200,ready};Message ack{reply,2,7,2,resume,0,1440,3200,ready};
 validate_reply(ack,m,before);ack.commit=1;rejects([&]{validate_reply(ack,m,before);});
 ack.commit=2;ack.generation=8;rejects([&]{validate_reply(ack,m,before);});
 ack.generation=7;ack.status=-5;rejects([&]{validate_reply(ack,m,before);});
 int sockets[2];assert(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0,sockets)==0);
 transmit(sockets[0],m);assert(receive(sockets[1]).seq==2);
 std::array<uint8_t,size+1> over{};assert(send(sockets[0],over.data(),over.size(),0)==static_cast<ssize_t>(over.size()));
 rejects([&]{receive(sockets[1],20);});rejects([&]{receive(sockets[1],20);});
 close(sockets[0]);rejects([&]{receive(sockets[1],20);});close(sockets[1]);
 pid_t child=fork();assert(child>=0);
 if(child==0){int allowed=open("/dev/null",O_RDWR),other=open("/dev/null",O_RDWR);assert(allowed>=0&&other>=0);
  restrict_to_binder(allowed);
  errno=0;assert(ioctl(allowed,BINDER_WRITE_READ,nullptr)<0&&errno==ENOTTY);
  errno=0;assert(ioctl(allowed,BINDER_THREAD_EXIT,nullptr)<0&&errno==ENOTTY);
  for(auto cmd:{BINDER_VERSION,BINDER_SET_CONTEXT_MGR}){errno=0;assert(ioctl(allowed,cmd,nullptr)<0&&errno==EPERM);}
  errno=0;assert(ioctl(other,BINDER_WRITE_READ,nullptr)<0&&errno==EPERM);
  errno=0;assert(open("/tmp/display-ipc-should-not-exist",O_WRONLY|O_CREAT,0600)<0&&errno==EPERM);
  // A subsequently created worker really inherits the filter.
  pid_t nested=fork();assert(nested>=0);if(!nested){errno=0;_exit(ioctl(allowed,BINDER_VERSION,nullptr)<0&&errno==EPERM?0:1);}
  int status;assert(waitpid(nested,&status,0)==nested&&WIFEXITED(status)&&WEXITSTATUS(status)==0);_exit(0);
 }
 int status;assert(waitpid(child,&status,0)==child&&WIFEXITED(status)&&WEXITSTATUS(status)==0);
}
