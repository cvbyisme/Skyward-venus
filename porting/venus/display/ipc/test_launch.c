// Host-only test: includes the real helpers, never substitutes a Binder service.
#define main display_launcher_main
#include "launch.c"
#undef main
#include <assert.h>
#include <sys/un.h>
int main(void) {
 // Entered by unshare -Urmpf; all mounts and all children belong to this test.
 assert(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL)==0);
 assert(mount("ipc-test","/tmp","tmpfs",MS_NOSUID|MS_NODEV,"mode=1777")==0);
 strcpy(root,"/tmp/test-root");assert(mkdir(root,0700)==0);
 mkdir_in("/run");file_in("/run/owner.sock","");mkdir_in("/proc");
 char source[]="/tmp/owner.sock";int server=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);assert(server>=0);
 struct sockaddr_un a={.sun_family=AF_UNIX};strcpy(a.sun_path,source);
 assert(bind(server,(struct sockaddr*)&a,sizeof(a))==0&&chmod(source,0600)==0&&listen(server,1)==0);
 socket_bind(source);
 int client=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);strcpy(a.sun_path,"/tmp/test-root/run/owner.sock");
 assert(connect(client,(struct sockaddr*)&a,sizeof(a))==0);
 int accepted=accept(server,NULL,NULL);assert(accepted>=0);
 assert(send(client,"actual seqpacket",16,0)==16);char packet[20];assert(recv(accepted,packet,sizeof(packet),0)==16&&!strcmp(packet,"actual seqpacket"));
 close(accepted);close(client);close(server);
 struct statvfs v;assert(statvfs(a.sun_path,&v)==0&&(v.f_flag&ST_RDONLY));
 // The original manager's identity procfs must not leak into its siblings.
 pid_t only_manager=fork();assert(only_manager>=0);
 if(!only_manager){assert(unshare(CLONE_NEWNS)==0);assert(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL)==0);
  char proc[PATH_MAX];target(proc,"/proc");assert(mount("proc",proc,"proc",MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC,NULL)==0);
  char info[PATH_MAX];target(info,"/proc/self/status");assert(access(info,R_OK)==0);_exit(0);}
 int status;assert(waitpid(only_manager,&status,0)==only_manager&&WIFEXITED(status)&&WEXITSTATUS(status)==0);
 assert(access("/tmp/test-root/proc/self/status",F_OK)<0&&errno==ENOENT);
 // Cleanup targets only recorded, unreaped owned children. An unrelated
 // sibling remains live; a TERM-ignoring owned process gets bounded KILL.
 pid_t sibling=fork();assert(sibling>=0);if(!sibling){signal(SIGTERM,SIG_DFL);for(;;)pause();}
 int started[2];assert(pipe(started)==0);
 pid_t stubborn=fork();assert(stubborn>=0);if(!stubborn){close(started[0]);signal(SIGTERM,SIG_IGN);assert(write(started[1],"R",1)==1);for(;;)pause();}
 close(started[1]);char c;assert(read(started[0],&c,1)==1);close(started[0]);
 long long begin=now_ms();stop_child(&stubborn,"test-stubborn");assert(stubborn==0&&now_ms()-begin<4000);
 assert(kill(sibling,0)==0);kill(sibling,SIGKILL);assert(waitpid(sibling,&status,0)==sibling);
 puts("PASS readonly socket bind, real seqpacket, manager-only proc, bounded exact-child cleanup");
 return 0;
}
