#include "stc_v2.hpp"
#include "blobs.hpp"
#include "vendor_pins.hpp"
#include "io_guard.hpp"
#include <binder/ProcessState.h>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/android/binder.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <limits>
#include <set>

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
uint32_t number(const char* s) {
  checked(*s && *s!='-',"invalid nonnegative integer"); errno=0; char* end=nullptr;
  unsigned long n=strtoul(s,&end,10);
  checked(errno==0 && !*end && n<=INT32_MAX,"invalid integer"); return static_cast<uint32_t>(n);
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

// Stack this narrower initialization filter before the existing all-ioctl
// guard. Even initialization cannot send BINDER_WRITE_READ or acquire a
// context manager; no Binder service or thread pool is involved.
void restrict_binder_initialization() {
  sock_filter instructions[]={
    BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,arch)),
    BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,native_audit_arch,1,0),
    BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
    BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,nr)),
    BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_ioctl,0,4),
    BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(seccomp_data,args[1])),
    BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,BINDER_VERSION,3,0),
    BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,BINDER_SET_MAX_THREADS,2,0),
    BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM),
    BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
    BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
  };
  sock_fprog program{static_cast<unsigned short>(sizeof(instructions)/sizeof(*instructions)),instructions};
  checked(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)==0,"Binder initialization no_new_privs failed");
  checked(prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&program)==0,"Binder initialization ioctl guard failed");
}
android::sp<android::ProcessState> initialize_binder() {
  struct stat st;
  checked(lstat("/dev/vndbinder",&st)==0 && S_ISCHR(st.st_mode),"real /dev/vndbinder character device is required");
  checked(access("/dev/vndbinder",R_OK|W_OK)==0,"/dev/vndbinder is not accessible; refusing driver fallback");
  checked(android::ProcessState::selfOrNull()==nullptr,"Binder was initialized before the guarded preparation");
  restrict_binder_initialization();
  std::fprintf(stderr,"BINDER_INIT_BEGIN driver=/dev/vndbinder allowed_ioctl=VERSION,SET_MAX_THREADS\n");
  auto state=android::ProcessState::initWithDriver("/dev/vndbinder");
  checked(state!=nullptr && !std::strcmp(state->getDriverName().c_str(),"/dev/vndbinder"),"Binder initialization used an unexpected driver");
  std::fprintf(stderr,"BINDER_INIT_DONE driver=/dev/vndbinder thread_pool=not_started\n");
  return state;
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
struct Interface {
  ScPostBlendInterface* ptr;
  bool initialized=false;
  ~Interface(){if(ptr){if(initialized)ptr->DeInit();delete ptr;}}
};
struct Record {std::string property;bool enabled;std::vector<uint8_t> bytes;};
template<class T>Record record(const char* property,const T& blob) {
  auto p=reinterpret_cast<const uint8_t*>(&blob);return {property,true,{p,p+sizeof(blob)}};
}
Record convert(const HwConfigPayload& p,Library& color) {
  checked(p.version==56 && p.hw_payload!=nullptr,"invalid STC hardware payload");
  if(p.hw_asset=="PostBlendIGC" || p.hw_asset=="PostBlendGC") {
    checked(p.hw_payload_len==88,"unexpected gamma ABI size");
    const auto& g=*static_cast<const GammaPostBlendConfig*>(p.hw_payload.get());
    checked(g.version==88,"unexpected gamma version");
    const char* name=p.hw_asset=="PostBlendIGC"?"SDE_DSPP_IGC_V4":"SDE_DSPP_GC_V1";
    if(!g.enabled)return {name,false,{}};
    if(p.hw_asset=="PostBlendIGC")return record(name,igc(g.r,g.g,g.b,g.dither_enabled,g.dither_strength));
    return record(name,gc(g.r,g.g,g.b));
  }
  if(p.hw_asset=="PostBlendGamut") {
    checked(p.hw_payload_len==117936,"unexpected gamut ABI size");
    const auto& g=*static_cast<const GamutConfig*>(p.hw_payload.get());
    checked(g.version==117936,"unexpected gamut version");
    if(!g.enabled)return {"SDE_DSPP_GAMUT_V4",false,{}};
    // Vendor algorithm expects the complete inline lut3d_info (16-byte header,
    // 4913 RGB input/output pairs), never a raw QDCM XML or userspace pointer blob.
    auto raw=g.gamut_info; uint32_t count;std::memcpy(&count,raw.data()+12,4);
    checked(count==4913,"only the full 17-cube gamut is currently supported");
    auto dst=std::make_unique<OwnedSdeGamut>();
    using Fn=int(*)(void*,SdeGamut*);
    auto fn=color.symbol<Fn>("_ZN3sdm11PPAlgorithm17ConvertGamutCoeffEP10lut3d_infoPNS_11SDEGamutCfgE");
    checked(fn(raw.data(),&dst->cfg)==0,"vendor gamut coefficient conversion failed");
    return record("SDE_DSPP_GAMUT_V4",gamut(*dst));
  }
  fail("unsupported STC asset: "+p.hw_asset);return {};
}
void frame(int fd,const std::vector<Record>& records) {
  write_all(fd,"VCL1",4);uint32_t n=records.size();write_all(fd,&n,4);
  for(const auto& r:records){uint32_t header[]={static_cast<uint32_t>(r.property.size()),r.enabled?1u:0u,static_cast<uint32_t>(r.bytes.size())};
    write_all(fd,header,sizeof(header));write_all(fd,r.property.data(),r.property.size());write_all(fd,r.bytes.data(),r.bytes.size());}
}
}
int main(int argc,char** argv) {
  try {
    const bool list=argc==2 && std::strcmp(argv[1],"--list")==0;
    const bool render=argc==4 && std::strcmp(argv[1],"--render")==0;
    checked(list||render,"usage: display-probe --list | --render LIST_INDEX QDCM_MODE_ID");
    const uint32_t index=render?number(argv[2]):0,id=render?number(argv[3]):0;
    alarm(30);verify_vendor();
    int output=dup(STDOUT_FILENO);checked(output>=0,"stdout dup failed");
    checked(dup2(STDERR_FILENO,STDOUT_FILENO)>=0,"vendor stdout redirect failed");
    // Keep the real VNDK ProcessState alive while plugin objects are alive.
    // The official constructor only opens, checks VERSION, sets MAX_THREADS,
    // and maps the driver. It never starts a thread pool or talks to services.
    const auto binder=initialize_binder();
    restrict_io();
    std::fprintf(stderr,"DISPLAY_IO_GUARD_ACTIVE all_ioctl=EPERM writable_open=EPERM\n");
    Library manager("/vendor/lib64/libsnapdragoncolor-manager.so");
    using Factory=ScPostBlendInterface*(*)(uint32_t,uint32_t);
    Interface api{manager.symbol<Factory>("GetScPostBlendInterface")(2,0)};
    checked(api.ptr!=nullptr,"STC V2 factory returned null");
    const auto panel_id=stc_panel_name(panel);
    std::fprintf(stderr,"STC_PANEL normalized=%s\n",panel_id.c_str());
    int rc=api.ptr->Init(panel_id);checked(rc==0,"STC Init failed under no-device-I/O guard");api.initialized=true;
    ColorModeList modes;auto request=wrap(kModeList,modes);
    checked(api.ptr->GetProperty(&request)==0,"STC mode enumeration failed");
    checked(modes.version==96 && !modes.list.empty() && modes.list.size()<=256,"invalid STC mode list");
    if(list) {
      std::string out="[\n";
      for(size_t i=0;i<modes.list.size();++i){const auto& m=modes.list[i];checked(m.hw_assets.size()<=64,"too many mode assets");
        if(i)out+=",\n";out+="{\"index\":"+std::to_string(i)+",\"gamut\":"+std::to_string(m.gamut)+",\"gamma\":"+std::to_string(m.gamma)+",\"intent\":"+std::to_string(m.intent)+",\"name\":"+quoted(m.name)+",\"assets\":[";
        for(size_t j=0;j<m.hw_assets.size();++j){if(j)out+=',';out+=quoted(m.hw_assets[j]);}out+="]}";
      }out+="\n]\n";write_all(output,out.data(),out.size());
    } else {
      checked(index<modes.list.size(),"mode index outside returned list");
      ModeRenderInputParams input;input.color_mode=modes.list[index];input.mode_id=static_cast<int32_t>(id);
      HwConfigOutputParams generated;auto in=wrap(kModeRenderInputParams,input),out=wrap(kHwConfigPayloadParam,generated);
      checked(api.ptr->ProcessOps(kScModeRenderIntent,in,&out)==0,"STC mode generation failed");
      checked(generated.version==32 && !generated.payload.empty() && generated.payload.size()<=32,"empty or invalid STC output");
      Library color("/vendor/lib64/libsdm-color.so");
      std::vector<Record> records;std::set<std::string> properties;
      for(const auto& p:generated.payload){auto r=convert(p,color);checked(properties.insert(r.property).second,"duplicate generated property");records.push_back(std::move(r));}
      // Emit only after every returned asset has been handled. Unknown assets
      // fail the whole request, so a partial color mode cannot masquerade as ready.
      frame(output,records);
      std::fprintf(stderr,"GENERATED_ONLY mode_index=%u qdcm_mode_id=%u assets=%zu; DRM_NOT_APPLIED\n",index,id,records.size());
    }
    int deinit=api.ptr->DeInit();api.initialized=false;
    checked(deinit==0,"STC DeInit failed");close(output);return 0;
  }catch(const std::exception& e){std::fprintf(stderr,"display-probe: %s\n",e.what());return 1;}
}
