//! Isolated IPC experiment. Root must stop the production UI before starting
//! this sole DRM owner. No color property, AOD, raw DSI or backlight writes.
#[path = "../../../porting/venus/display/ipc/protocol.rs"]
mod protocol;

#[cfg(target_os="android")]
mod device {
    use super::protocol::*;
    use anyhow::{bail,ensure,Context,Result};
    use skyward_ui::{Canvas,Rgba,Surface};
    use skyward_ui::surface::kms::KmsSurface;
    use std::ffi::CString;
    use std::io;
    use std::mem;
    use std::os::fd::{AsRawFd,FromRawFd,OwnedFd};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool,Ordering};
    use std::time::{Duration,Instant};
    static STOP:AtomicBool=AtomicBool::new(false);
    extern "C" fn stop(_:libc::c_int){STOP.store(true,Ordering::Relaxed);}
    struct Panel {surface:KmsSurface,frame:Canvas,restored:bool}
    impl Display for Panel {
        fn settle(&mut self)->Result<(),String>{self.surface.settle().map_err(|e|format!("settle: {e:#}"))}
        fn refresh(&mut self)->Result<(),String>{
            self.surface.present(&self.frame).and_then(|_|self.surface.settle()).map_err(|e|format!("refresh: {e:#}"))
        }
    }
    impl Panel {fn restore(&mut self)->Result<(),String>{
        self.refresh()?;self.restored=true;
        eprintln!("DISPLAY_OWNER_RESTORED generated-normal-baseline");Ok(())
    }}
    impl Drop for Panel {fn drop(&mut self){if !self.restored {
        if let Err(e)=self.restore(){eprintln!("DISPLAY_OWNER_RESTORE_FAILED {e}");}
    }}}
    struct SocketPath(String);
    impl Drop for SocketPath {fn drop(&mut self){let _=std::fs::remove_file(&self.0);}}
    fn poll(fd:i32,timeout:i32)->Result<i16>{
        let mut p=libc::pollfd{fd,events:libc::POLLIN,revents:0};
        let n=unsafe{libc::poll(&mut p,1,timeout)};
        if n<0 {let e=io::Error::last_os_error();if e.kind()==io::ErrorKind::Interrupted{return Ok(0)}return Err(e.into());}
        Ok(p.revents)
    }
    fn send(fd:i32,m:Message)->Result<()> {
        let b=m.encode();
        let n=unsafe{libc::send(fd,b.as_ptr().cast(),b.len(),libc::MSG_NOSIGNAL)};
        ensure!(n==LEN as isize,"send owner reply: {}",io::Error::last_os_error());Ok(())
    }
    fn recv(fd:i32)->Result<Message>{
        let mut b=[0u8;LEN+1];
        let n=unsafe{libc::recv(fd,b.as_mut_ptr().cast(),b.len(),libc::MSG_TRUNC)};
        ensure!(n==LEN as isize,"peer exited or wrong packet size: {n}");
        Message::decode(&b[..LEN]).map_err(anyhow::Error::msg)
    }
    fn listener(path:&str)->Result<(OwnedFd,SocketPath)> {
        ensure!(path.starts_with('/') && path.len()+8<108,"socket path must be a short absolute path");
        ensure!(!Path::new(path).exists(),"refusing to replace existing socket path");
        let parent=Path::new(path).parent().context("socket parent")?;
        use std::os::unix::fs::MetadataExt;
        let meta=std::fs::symlink_metadata(parent)?;
        ensure!(meta.is_dir() && meta.uid()==0 && meta.mode()&0o077==0,"socket parent must be root-owned mode 0700");
        let raw=unsafe{libc::socket(libc::AF_UNIX,libc::SOCK_SEQPACKET|libc::SOCK_CLOEXEC,0)};
        ensure!(raw>=0,"socket: {}",io::Error::last_os_error());
        let fd=unsafe{OwnedFd::from_raw_fd(raw)};
        let pending=format!("{path}.pending");
        let mut addr:libc::sockaddr_un=unsafe{mem::zeroed()};addr.sun_family=libc::AF_UNIX as _;
        for (out,b) in addr.sun_path.iter_mut().zip(pending.bytes()){*out=b as _;}
        let n=unsafe{libc::bind(raw,(&addr as *const libc::sockaddr_un).cast(),mem::size_of_val(&addr) as _)};
        ensure!(n==0,"bind: {}",io::Error::last_os_error());
        let mut cleanup=SocketPath(pending.clone());
        let c=CString::new(pending.clone())?;
        ensure!(unsafe{libc::chmod(c.as_ptr(),0o600)}==0,"chmod socket");
        ensure!(unsafe{libc::listen(raw,1)}==0,"listen socket");
        // The supervisor sees the final inode only after listen is ready.
        std::fs::rename(&pending,path)?;cleanup.0=path.into();Ok((fd,cleanup))
    }
    pub fn run()->Result<()> {
        let args:Vec<_>=std::env::args().collect();
        ensure!(args.len()==3 && args[2]=="--baseline","usage: venus_display_owner ABSOLUTE_SOCKET --baseline; stop UI first");
        ensure!(unsafe{libc::geteuid()}==0,"owner requires root");
        unsafe{libc::umask(0o077);libc::signal(libc::SIGTERM,stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT,stop as *const () as libc::sighandler_t);}
        ensure!(std::fs::read_to_string("/sys/class/mi_display/disp-DSI-0/doze_brightness")?.trim()=="0","start in normal display mode");
        ensure!(std::fs::read_to_string("/sys/class/backlight/panel0-backlight/brightness")?.trim().parse::<u32>()?>0,"start with normal nonzero brightness");
        let mut generation=0u64;
        ensure!(unsafe{libc::getrandom((&mut generation as *mut u64).cast(),8,0)}==8 && generation!=0,"getrandom generation failed");
        let surface=KmsSurface::open("/dev/dri/card0").context("exclusive KMS owner; production UI must be stopped")?;
        let (w,h)=surface.size();
        eprintln!("DISPLAY_OWNER_KMS {}",surface.describe());
        let mut frame=Canvas::filled(w,h,Rgba::rgb(24,35,48));
        frame.fill_rect(w as f32*0.3,h as f32*0.4,w as f32*0.4,h as f32*0.05,Rgba::rgb(180,190,200));
        let mut panel=Panel{surface,frame,restored:false};panel.refresh().map_err(anyhow::Error::msg)?;
        let mut owner=Owner::initialized(panel,generation,w,h);
        let (listener,_socket)=listener(&args[1])?;
        eprintln!("DISPLAY_OWNER_READY pid={} generation={generation} size={w}x{h} frame=generated-normal-baseline color_applied=false",std::process::id());
        let deadline=Instant::now()+Duration::from_secs(60);
        let connect_deadline=Instant::now()+Duration::from_secs(12);
        loop {ensure!(!STOP.load(Ordering::Relaxed),"owner stopped before peer connected");
            ensure!(Instant::now()<connect_deadline,"QService did not connect within 12 seconds");
            if poll(listener.as_raw_fd(),200)?&libc::POLLIN!=0{break;}}
        let raw=unsafe{libc::accept4(listener.as_raw_fd(),std::ptr::null_mut(),std::ptr::null_mut(),libc::SOCK_CLOEXEC)};
        ensure!(raw>=0,"accept: {}",io::Error::last_os_error());let peer=unsafe{OwnedFd::from_raw_fd(raw)};
        drop(listener); // Exactly one service is allowed for this owner generation.
        let mut cred:libc::ucred=unsafe{mem::zeroed()};let mut len=mem::size_of_val(&cred) as libc::socklen_t;
        ensure!(unsafe{libc::getsockopt(raw,libc::SOL_SOCKET,libc::SO_PEERCRED,(&mut cred as *mut libc::ucred).cast(),&mut len)}==0 && len as usize==mem::size_of_val(&cred) && cred.uid==0 && cred.pid>0,"untrusted owner peer");
        eprintln!("DISPLAY_OWNER_PEER pid={} uid={}",cred.pid,cred.uid);send(raw,owner.current)?;
        let mut next=Instant::now()+Duration::from_secs(1);
        while !STOP.load(Ordering::Relaxed) {
            ensure!(Instant::now()<deadline,"owner reached its 60-second deadline");
            let events=poll(peer.as_raw_fd(),100)?;
            if events&(libc::POLLHUP|libc::POLLERR|libc::POLLNVAL)!=0 {break;}
            if events&libc::POLLIN!=0 {
                let m=recv(raw)?;
                match owner.request(m) {
                    Ok(reply)=>{send(raw,reply)?;eprintln!("DISPLAY_OWNER_ACK request={} op={} state={} commit={}",reply.request,reply.op,reply.state,reply.commit);}
                    Err(error)=>{let failed=Message{kind:REPLY,request:m.request,op:m.op,status:-5,state:FAILED,..owner.current};let _=send(raw,failed);bail!("owner request failed: {error}");}
                }
            }
            if Instant::now()>=next {owner.tick().map_err(anyhow::Error::msg)?;next=Instant::now()+Duration::from_secs(1);}
        }
        // Drop restores and settles the known baseline even after pause or error.
        owner.display.restore().map_err(anyhow::Error::msg)?;
        drop(owner);Ok(())
    }
}
fn main(){
    #[cfg(target_os="android")]
    if let Err(e)=device::run(){eprintln!("display-owner: {e:#}");std::process::exit(1);}
    #[cfg(not(target_os="android"))]
    eprintln!("This experiment requires Android KMS; host protocol tests use cargo test --example venus_display_owner.");
}
