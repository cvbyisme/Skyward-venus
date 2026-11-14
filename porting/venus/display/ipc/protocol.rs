//! Private experiment protocol. No raw vendor or DRM ABI crosses this link.
pub const LEN: usize = 64;
pub const HELLO: u16 = 1;
pub const REQUEST: u16 = 2;
pub const REPLY: u16 = 3;
pub const PING: u32 = 1;
pub const PAUSE: u32 = 2;
pub const RESUME: u32 = 3;
pub const READY: u32 = 0;
pub const PAUSED: u32 = 1;
pub const FAILED: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: u16, pub request: u64, pub generation: u64, pub commit: u64,
    pub op: u32, pub status: i32, pub width: u32, pub height: u32, pub state: u32,
}
impl Message {
    pub fn encode(self) -> [u8; LEN] {
        let mut b = [0; LEN];
        b[..4].copy_from_slice(b"VDI1"); b[4..6].copy_from_slice(&1u16.to_le_bytes());
        b[6..8].copy_from_slice(&self.kind.to_le_bytes());
        for (at, v) in [(8, self.request), (16, self.generation), (24, self.commit)] {
            b[at..at+8].copy_from_slice(&v.to_le_bytes());
        }
        for (at, v) in [(32, self.op), (36, self.status as u32), (40, self.width), (44, self.height), (48, self.state)] {
            b[at..at+4].copy_from_slice(&v.to_le_bytes());
        }
        b
    }
    pub fn decode(b: &[u8]) -> Result<Self, &'static str> {
        if b.len()!=LEN || &b[..4]!=b"VDI1" || b[4..6]!=1u16.to_le_bytes() || b[52..].iter().any(|&b| b!=0) {
            return Err("invalid packet size, version, magic or reserved bytes");
        }
        let u32at = |at| u32::from_le_bytes(b[at..at+4].try_into().unwrap());
        let u64at = |at| u64::from_le_bytes(b[at..at+8].try_into().unwrap());
        Ok(Self { kind:u16::from_le_bytes(b[6..8].try_into().unwrap()), request:u64at(8),
            generation:u64at(16), commit:u64at(24), op:u32at(32), status:u32at(36) as i32,
            width:u32at(40), height:u32at(44), state:u32at(48) })
    }
}

pub trait Display {
    fn settle(&mut self) -> Result<(), String>;
    fn refresh(&mut self) -> Result<(), String>;
}

pub struct Owner<D> {
    pub display: D,
    pub current: Message,
    last_request: u64,
}
impl<D: Display> Owner<D> {
    // Caller must really present and settle the initial frame before using this.
    pub fn initialized(display: D, generation:u64, width:u32, height:u32) -> Self {
        Self { display, last_request:0, current:Message { kind:HELLO, request:0,
            generation, commit:1, op:0, status:0, width, height, state:READY } }
    }
    pub fn request(&mut self, m: Message) -> Result<Message, String> {
        if self.current.state==FAILED || m.kind!=REQUEST || m.generation!=self.current.generation ||
            m.request==0 || m.request<=self.last_request || m.commit!=0 || m.status!=0 ||
            m.width!=0 || m.height!=0 || m.state!=0 || ![PING,PAUSE,RESUME].contains(&m.op) {
            return Err("invalid, stale or unsupported owner request".into());
        }
        self.last_request=m.request;
        let result = match m.op {
            PING => Ok(()),
            PAUSE => self.display.settle(),
            RESUME => self.display.refresh(),
            _ => unreachable!(),
        };
        if let Err(error)=result { self.current.state=FAILED; return Err(error); }
        if m.op==PAUSE { self.current.state=PAUSED; }
        if m.op==RESUME { self.current.state=READY; self.current.commit+=1; }
        Ok(Message { kind:REPLY, request:m.request, op:m.op, ..self.current })
    }
    pub fn tick(&mut self) -> Result<(), String> {
        if self.current.state==READY {
            if let Err(e)=self.display.refresh() { self.current.state=FAILED; return Err(e); }
            self.current.commit+=1;
        }
        Ok(())
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[derive(Default)] struct Mock { calls:Vec<&'static str>, fail:bool }
    impl Display for Mock {
        fn settle(&mut self)->Result<(),String> { self.calls.push("settle"); if self.fail {Err("flip timeout".into())}else{Ok(())} }
        fn refresh(&mut self)->Result<(),String> { self.calls.push("present+settle"); if self.fail {Err("commit failed".into())}else{Ok(())} }
    }
    fn req(op:u32,n:u64)->Message { Message{kind:REQUEST,request:n,generation:7,commit:0,op,status:0,width:0,height:0,state:0} }
    #[test] fn strict_wire_and_truncation() {
        let m=req(RESUME,2);let b=m.encode();assert_eq!(Message::decode(&b),Ok(m));
        for i in 0..LEN {assert!(Message::decode(&b[..i]).is_err());}
        let mut bad=b;bad[63]=1;assert!(Message::decode(&bad).is_err());
        let mut bad=b;bad[4]=2;assert!(Message::decode(&bad).is_err());
    }
    #[test] fn pause_has_no_interleaved_refresh_and_resume_is_real() {
        let mut o=Owner::initialized(Mock::default(),7,1440,3200);
        let a=o.request(req(PAUSE,1)).unwrap();assert_eq!((a.state,a.commit),(PAUSED,1));
        o.tick().unwrap();o.tick().unwrap();assert_eq!(o.display.calls,["settle"]);
        let a=o.request(req(RESUME,2)).unwrap();assert_eq!((a.state,a.commit),(READY,2));
        assert_eq!(o.display.calls,["settle","present+settle"]);
    }
    #[test] fn repeated_resume_still_refreshes_and_pause_is_idempotent() {
        let mut o=Owner::initialized(Mock::default(),7,1,1);
        o.request(req(RESUME,1)).unwrap();o.request(req(RESUME,2)).unwrap();
        o.request(req(PAUSE,3)).unwrap();o.request(req(PAUSE,4)).unwrap();
        assert_eq!(o.current.commit,3);assert_eq!(o.display.calls,["present+settle","present+settle","settle","settle"]);
    }
    #[test] fn failed_hardware_is_never_acknowledged() {
        for op in [PAUSE,RESUME] {let mut o=Owner::initialized(Mock{fail:true,..Mock::default()},7,1,1);
            assert!(o.request(req(op,1)).is_err());assert_eq!((o.current.state,o.current.commit),(FAILED,1));
            assert!(o.request(req(PING,2)).is_err());}
    }
    #[test] fn generation_replay_and_unknown_fail_before_touching_display() {
        let mut o=Owner::initialized(Mock::default(),7,1,1);
        let mut bad=req(RESUME,1);bad.generation=8;assert!(o.request(bad).is_err());
        assert!(o.request(req(77,1)).is_err());o.request(req(PING,1)).unwrap();
        assert!(o.request(req(RESUME,1)).is_err());assert!(o.display.calls.is_empty());
    }
}
