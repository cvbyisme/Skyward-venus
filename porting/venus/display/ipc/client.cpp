// Separate IPC client; the original guarded probe is unchanged.
// Hash/panel/mode serialization helpers copied from frozen probe f5ca9088 build.
#include "../stc_v2.hpp"
#include "../vendor_pins.hpp"
#include "binder_guard.hpp"
#include <binder/ProcessState.h>
#include <binder/IPCThreadState.h>
#include <binder/IServiceManager.h>
#include <binder/Parcel.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
using namespace snapdragoncolor;
using namespace venus_display;
namespace {
constexpr const char* panel="xiaomi 38 08 0a cmd mode dsc dsi panel";
std::string stc_panel_name(std::string name) {
  // SDM PPHWAttributes::Set passes this normalized identifier to STC,
  // rather than the connector's raw panel string. Keep the stock XML intact.
  for(char& c:name)if(c==' ')c='_';
  const auto newline=name.find('\n');
  if(newline!=std::string::npos)name.resize(newline);
  return name;
}
void fail(const std::string& why) { throw std::runtime_error(why); }
void checked(bool ok,const char* why) { if(!ok) fail(why); }
void write_all(int fd,const void* data,size_t len) {
  auto p=static_cast<const uint8_t*>(data);
  while(len) { ssize_t n=write(fd,p,len); if(n<0 && errno==EINTR)continue;
    checked(n>0,"write failed"); p+=n; len-=static_cast<size_t>(n); }
}
std::string quoted(const std::string& s) {
  checked(s.size()<4096,"overlong vendor string");
  const char* hex="0123456789abcdef"; std::string out="\"";
  for(unsigned char c:s) { if(c=='"'||c=='\\') {out+='\\';out+=c;}
    else if(c<32 || c>=127) {out+="\\u00";out+=hex[c>>4];out+=hex[c&15];} else out+=c; }
  return out+'"';
}
void verify_vendor() {
  // Only fixed, compile-time paths. Use the recovery's existing SHA256 tool;
  // no shell parsing, downloaded verifier, or caller-provided hash bypass.
  int pipefd[2]; checked(pipe2(pipefd,O_CLOEXEC)==0,"hash pipe failed");
  pid_t child=fork(); checked(child>=0,"hash fork failed");
  if(child==0) {
    alarm(10); if(dup2(pipefd[1],STDOUT_FILENO)<0)_exit(125);
    close(pipefd[0]);close(pipefd[1]);
    std::vector<char*> args={const_cast<char*>("/system/bin/toybox"),const_cast<char*>("sha256sum")};
    for(const auto& p:vendor_pins)args.push_back(const_cast<char*>(p.path));
    args.push_back(nullptr);execv(args[0],args.data());_exit(126);
  }
  close(pipefd[1]); std::string result; char buf[1024];
  for(;;) {ssize_t n=read(pipefd[0],buf,sizeof(buf));if(n<0&&errno==EINTR)continue;
    if(n<=0)break;result.append(buf,n);if(result.size()>16384){kill(child,SIGKILL);break;} }
  close(pipefd[0]); int status=0; pid_t got;
  do {got=waitpid(child,&status,0);} while(got<0&&errno==EINTR);
  checked(got==child && WIFEXITED(status) && WEXITSTATUS(status)==0,"vendor SHA256 verification tool failed");
  std::string expected;
  for(const auto& p:vendor_pins)expected+=std::string(p.sha256)+"  "+p.path+"\n";
  checked(result==expected,"vendor SHA256 mismatch; private ABI refused");
}

struct Library {
  void* handle;
  explicit Library(const char* p):handle(dlopen(p,RTLD_NOW|RTLD_LOCAL)) {
    if(!handle){const char* e=dlerror();fail(std::string("dlopen: ")+(e?e:p));}
  }
  ~Library(){if(handle)dlclose(handle);}
  Library(const Library&)=delete;
  template<class T>T symbol(const char* name){dlerror();void* p=dlsym(handle,name);const char* e=dlerror();
    if(e||!p)fail(std::string("dlsym: ")+name);T fn;static_assert(sizeof(fn)==sizeof(p));std::memcpy(&fn,&p,sizeof(fn));return fn;}
};

void exercise(const android::sp<android::IBinder>& service) {
  for(int enabled:{0,1}) {
    android::Parcel input,reply;
    checked(input.writeInterfaceToken(android::String16("android.display.IQService"))==0,"write qservice token");
    checked(input.writeInt32(enabled)==0,"write toggle");
    int result=service->transact(20,input,&reply);
    checked(result==0,"QService real pause/resume failed");
    int32_t status=-1;
    checked(reply.readInt32(&status)==0 && status==0 && reply.dataAvail()==0,"invalid QService completion");
  }
  android::Parcel unsupported,reply;
  unsupported.writeInterfaceToken(android::String16("android.display.IQService"));
  checked(service->transact(399,unsupported,&reply)==android::UNKNOWN_TRANSACTION,"unsupported command was not rejected");
  android::Parcel invalid,badreply;
  invalid.writeInterfaceToken(android::String16("android.display.IQService"));invalid.writeInt32(2);
  checked(service->transact(20,invalid,&badreply)==android::BAD_VALUE,"invalid toggle was not rejected");
  std::fprintf(stderr,"DISPLAY_QSERVICE_EXERCISE_PASS pause/resume=real-ACK invalid/unknown=rejected\n");
}
}
int main(int argc,char**argv) {
  try {
    checked(argc==2 && !std::strcmp(argv[1],"--list"),"usage: ipc-client --list");
    alarm(30);verify_vendor();
    int output=dup(STDOUT_FILENO);checked(output>=0,"stdout dup");
    checked(dup2(STDERR_FILENO,STDOUT_FILENO)>=0,"vendor stdout redirect");
    struct stat st{};checked(lstat("/dev/vndbinder",&st)==0 && S_ISCHR(st.st_mode),"private Binder node missing");
    checked(android::ProcessState::selfOrNull()==nullptr,"unexpected earlier Binder initialization");
    auto binder=android::ProcessState::initWithDriver("/dev/vndbinder");
    checked(binder!=nullptr && !std::strcmp(binder->getDriverName().c_str(),"/dev/vndbinder"),"wrong Binder driver");
    auto sm=android::defaultServiceManager();checked(sm!=nullptr,"original vendor manager unavailable");
    auto qs=sm->checkService(android::String16("display.qservice"));checked(qs!=nullptr,"real qservice absent");
    checked(sm->checkService(android::String16("display.mistcservice"))==nullptr,"preexisting MiStc service");
    int fd=-1;checked(android::IPCThreadState::self()->setupPolling(&fd)==0 && fd>=0,"Binder polling setup");
    display_ipc::restrict_to_binder(fd);
    binder->startThreadPool(); // Threads inherit the filter before vendor loading.
    std::fprintf(stderr,"DISPLAY_IPC_GUARD_ACTIVE binder_fd=%d all_other_ioctl=EPERM writable_open=EPERM\n",fd);
    exercise(qs);
    // Binder-published plugin objects have process lifetime. Keep their code
    // mapped until _exit tears down all threads/handles together; do not dlclose
    // or delete a vendor object while the registry may own its real callbacks.
    auto* manager=new Library("/vendor/lib64/libsnapdragoncolor-manager.so");
    using Factory=ScPostBlendInterface*(*)(uint32_t,uint32_t);
    auto* api=manager->symbol<Factory>("GetScPostBlendInterface")(2,0);
    checked(api!=nullptr,"STC V2 factory returned null");
    const auto panel_id=stc_panel_name(panel);
    std::fprintf(stderr,"STC_PANEL normalized=%s\n",panel_id.c_str());
    int rc=api->Init(panel_id);
    std::fprintf(stderr,"STC_INIT_RESULT status=%d\n",rc);checked(rc==0,"original STC Init failed with real IPC");
    checked(sm->checkService(android::String16("display.mistcservice"))!=nullptr,"MI did not publish its actual service");
    ColorModeList modes;auto request=wrap(kModeList,modes);
    checked(api->GetProperty(&request)==0,"STC mode enumeration failed");
    checked(modes.version==96 && !modes.list.empty() && modes.list.size()<=256,"invalid STC mode list");
    std::string out="[\n";
    for(size_t i=0;i<modes.list.size();++i){const auto& m=modes.list[i];checked(m.hw_assets.size()<=64,"too many mode assets");
      if(i)out+=",\n";out+="{\"index\":"+std::to_string(i)+",\"gamut\":"+std::to_string(m.gamut)+",\"gamma\":"+std::to_string(m.gamma)+",\"intent\":"+std::to_string(m.intent)+",\"name\":"+quoted(m.name)+",\"assets\":[";
      for(size_t j=0;j<m.hw_assets.size();++j){if(j)out+=',';out+=quoted(m.hw_assets[j]);}out+="]}";
    }
    out+="\n]\n";write_all(output,out.data(),out.size());close(output);
    std::fprintf(stderr,"DISPLAY_IPC_LIST_DONE modes=%zu color_applied=false calibration_equivalence=unproven\n",modes.list.size());
    std::fflush(nullptr);_exit(0);
  }catch(const std::exception&e){std::fprintf(stderr,"ipc-client: %s\n",e.what());std::fflush(nullptr);_exit(1);}
}
