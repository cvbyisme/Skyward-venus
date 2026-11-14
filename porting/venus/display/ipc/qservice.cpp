// Actual android.display.IQService wire protocol, with one implemented command.
// No foreign C++ QService vtable or private HWC object is fabricated.
#include "transport.hpp"
#include "binder_guard.hpp"
#include <binder/Binder.h>
#include <binder/IPCThreadState.h>
#include <binder/IServiceManager.h>
#include <binder/Parcel.h>
#include <binder/ProcessState.h>
#include <cstdio>
#include <cstdlib>
#include <signal.h>
using namespace android;
namespace {
volatile sig_atomic_t stop=0;
bool backend_failed=false;
void stopping(int){stop=1;}
class Service final:public BBinder {
 display_ipc::Owner& owner_;
 const String16 descriptor_{"android.display.IQService"};
public:
 explicit Service(display_ipc::Owner& owner):owner_(owner){}
 const String16& getInterfaceDescriptor()const override{return descriptor_;}
 status_t pingBinder()override{try{owner_.call(display_ipc::ping);return NO_ERROR;}catch(...){return DEAD_OBJECT;}}
protected:
 status_t onTransact(uint32_t code,const Parcel& data,Parcel* reply,uint32_t flags)override {
  if(code==INTERFACE_TRANSACTION||code==PING_TRANSACTION)return BBinder::onTransact(code,data,reply,flags);
  const auto pid=IPCThreadState::self()->getCallingPid();
  if(IPCThreadState::self()->getCallingUid()!=0)return PERMISSION_DENIED;
  if(code!=20){std::fprintf(stderr,"QSERVICE_UNSUPPORTED pid=%d code=%u\n",pid,code);return UNKNOWN_TRANSACTION;}
  if((flags&FLAG_ONEWAY)||!reply||!data.enforceInterface(descriptor_))return BAD_TYPE;
  int32_t enabled=-1;
  if(data.readInt32(&enabled)!=NO_ERROR||data.dataAvail()!=0||(enabled!=0&&enabled!=1))return BAD_VALUE;
  try{
   const auto ack=owner_.call(enabled?display_ipc::resume:display_ipc::pause);
   std::fprintf(stderr,"QSERVICE_ACK pid=%d code=20 enable=%d generation=%llu commit=%llu state=%u\n",pid,enabled,
     static_cast<unsigned long long>(ack.generation),static_cast<unsigned long long>(ack.commit),ack.state);
   return reply->writeInt32(NO_ERROR);
  }catch(const std::exception& e){
   std::fprintf(stderr,"QSERVICE_BACKEND_FAILED %s\n",e.what());backend_failed=true;stop=1;
   // Mi's proxy checks transact(), not the status in the output parcel.
   // Propagate failure as a real Binder transaction error; never empty success.
   return DEAD_OBJECT;
  }
 }
};
}
int main(int argc,char**argv){try{
 using display_ipc::check;
 check(argc==4,"usage: ipc-qservice SOCKET EXPECTED_OWNER_PID READY_FD");
 check(geteuid()==0,"qservice requires root");
 char* end=nullptr;long pid=strtol(argv[2],&end,10);check(end&&!*end&&pid>0&&pid<=INT32_MAX,"owner PID");
 long readyfd=strtol(argv[3],&end,10);check(end&&!*end&&readyfd>=3&&readyfd<=1024,"ready FD");
 signal(SIGTERM,stopping);signal(SIGINT,stopping);alarm(55);
 display_ipc::Owner owner(argv[1],static_cast<pid_t>(pid));
 auto process=ProcessState::initWithDriver("/dev/vndbinder");
 check(process!=nullptr&&process->getDriverName()==String8("/dev/vndbinder"),"private Binder initialization");
 process->setThreadPoolMaxThreadCount(0);
 auto sm=defaultServiceManager();check(sm!=nullptr,"real vendor service manager unavailable");
 check(sm->checkService(String16("display.qservice"))==nullptr,"refusing preexisting qservice");
 sp<Service> service=new Service(owner);
 check(sm->addService(String16("display.qservice"),service)==NO_ERROR,"qservice registration rejected");
 check(sm->checkService(String16("display.qservice"))==service,"qservice readback is not this instance");
 int binder=-1;check(IPCThreadState::self()->setupPolling(&binder)==NO_ERROR&&binder>=0,"Binder polling");
 display_ipc::restrict_to_binder(binder);IPCThreadState::self()->flushCommands();
 check(write(static_cast<int>(readyfd),"R",1)==1,"ready receipt");close(static_cast<int>(readyfd));
 std::fprintf(stderr,"DISPLAY_QSERVICE_READY pid=%d owner=%ld supported=20\n",getpid(),pid);
 while(!stop){pollfd fds[]={{binder,POLLIN,0},{owner.fd(),POLLIN,0}};int n=poll(fds,2,250);
  if(n<0&&errno==EINTR)continue;check(n>=0,"qservice polling failed");
  check(!(fds[1].revents&(POLLHUP|POLLERR|POLLNVAL))&&!(fds[1].revents&POLLIN),"unsolicited or dead owner");
  check(!(fds[0].revents&(POLLHUP|POLLERR|POLLNVAL)),"Binder failed");
  if(fds[0].revents&POLLIN)IPCThreadState::self()->handlePolledCommands();
 }
 std::fprintf(stderr,"DISPLAY_QSERVICE_STOPPED backend_failed=%d\n",backend_failed);return backend_failed?1:0;
}catch(const std::exception&e){std::fprintf(stderr,"ipc-qservice: %s\n",e.what());return 1;}}
