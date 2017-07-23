pub(crate) use self::implemented::Dns;

use ::dns_parser;
use ::dns_parser::{Packet,RRData};

pub(crate) fn dns_parse(buf:&[u8]) {
    let packet = Packet::parse(buf).unwrap();
    for a in packet.answers {
        match a.data {
            RRData::A(ip) => {
                print!("IP: {} ", ip);
            }
            _ => {
                // Bad value. Log it?
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod implemented {
    use std::io;
    use std::thread::{JoinHandle,spawn};
    use libc::{c_void, c_uint, c_long};
    use libc::{AF_INET, AF_INET6, sockaddr, sockaddr_in, sockaddr_in6}; //,getnameinfo,NI_NUMERICHOST
    use core_foundation_sys::base::{CFAllocatorRef, kCFAllocatorDefault, CFRelease};
    use core_foundation_sys::messageport::*;
    use core_foundation_sys::array::CFArrayRef;
    use core_foundation_sys::data::*;
    use core_foundation_sys::string::CFStringRef;
    use core_foundation_sys::runloop::{CFRunLoopRef,kCFRunLoopDefaultMode,kCFRunLoopCommonModes, CFRunLoopGetCurrent, CFRunLoopAddSource};
    use core_foundation::string::CFString;
    use core_foundation::array::CFArray;
    use std::str::FromStr;
    use std::ptr;
    use std::mem;
    use core_foundation::base::TCFType;
    use core_foundation::runloop::CFRunLoop;
    use std::sync::mpsc::{Sender,Receiver,channel};
    use std::cell::RefCell;
    use std::net::UdpSocket;

    unsafe impl Send for ThreadResp {}

    struct ThreadResp {
        resp: *mut c_void,
    }

    struct ThreadMsg {
        id: usize,
        host: *mut String,
        resp: *mut String,
    }

    impl Default for ThreadMsg {
        fn default() -> ThreadMsg {
            ThreadMsg {
                id: 0,
                host: ptr::null_mut(),
                resp: ptr::null_mut(),
            }
        }
    }

    thread_local! {
        static TX: RefCell<Option<Sender<ThreadResp>>> = RefCell::new(None);
    }

    pub(crate) struct Dns {
        tx: Sender<ThreadResp>,
        rx: Receiver<ThreadResp>,
        thr: JoinHandle<()>,
    }

    impl Dns {
        pub fn new() -> Dns {
            let (tx,rx) = channel();
            let tx1 = tx.clone();
            let thr = spawn(move || {
                Dns::thread_func(tx1);
            });
            Dns {
                rx,
                tx,
                thr
            }
        }

        pub fn check_result(&self) -> Option<(usize,Box<String>)> {
            match self.rx.try_recv() {
                Ok(r) => {
                    unsafe {
                        let req = Box::from_raw(r.resp as *mut ThreadMsg);
                        let ip = Box::from_raw(req.resp);
                        Some((req.id, ip))
                    }
                }
                _ => {
                    None
                }
            }
        }

        pub fn start_lookup(&self, id: usize, host: &str) -> io::Result<Option<UdpSocket>> {
            unsafe {
                if let Ok(name) = CFString::from_str("com.easyfibers.dns") {
                    let port_ref = CFMessagePortCreateRemote(kCFAllocatorDefault, name.as_concrete_TypeRef());
                    if port_ref == ptr::null() {
                        return Err(io::Error::new(io::ErrorKind::Other,"port create failed"));
                    }
                    // With CFData we create a copy of msg and send it to thread.
                    // This copy will then be returned on receive.
                    let msg = ThreadMsg {
                        id,
                        host: Box::into_raw(Box::new(host.to_string())) as *mut String,
                        resp: ptr::null_mut(),
                    };
                    let msgp = mem::transmute::<&ThreadMsg, *const u8>(&msg);
                    let data_ref = CFDataCreate(kCFAllocatorDefault, msgp, mem::size_of::<ThreadMsg>() as i64);
                    if CFMessagePortSendRequest(port_ref, 10, data_ref, 0.0, 0.0, ptr::null(), ptr::null_mut()) != 0 {
                        return Err(io::Error::new(io::ErrorKind::Other,"can not send port"));
                    }
                }
                Ok(None)
            }
        }

        fn thread_func(tx: Sender<ThreadResp>) {
            TX.with(|t| {
                *t.borrow_mut() = Some(tx);
            });

            if let Ok(name) = CFString::from_str("com.easyfibers.dns") {
                let ctx = CFMessagePortContext {
                    version: 0,
                    info: ptr::null_mut(),
                    retain: None,
                    release: None,
                    copyDescription: None,
                };
                unsafe {
                    let port_ref = CFMessagePortCreateLocal(kCFAllocatorDefault, 
                        name.as_concrete_TypeRef(), 
                        Some(port_cb), 
                        &ctx as *const CFMessagePortContext, 
                        ptr::null_mut());
                    let portsrc = CFMessagePortCreateRunLoopSource(kCFAllocatorDefault, port_ref, 0);
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), portsrc, kCFRunLoopCommonModes);
                    CFRunLoop::run_current();
                    println!("loop done???");
                }
            }
        }
    }

    // Start lookup
    unsafe extern fn port_cb(local: CFMessagePortRef,
                     msgid: i32,
                     data: CFDataRef, info: *mut c_void) -> CFDataRef {
        let msg = ::std::mem::transmute::<*const u8, &mut ThreadMsg>(CFDataGetBytePtr(data));
        let host = Box::from_raw(msg.host);
        let id = msg.id;
        CFRelease(data as *const c_void);
        let msg = Box::new(ThreadMsg {
            id,
            host: ptr::null_mut(),
            resp: ptr::null_mut(),
        });
        if let Ok(host) = CFString::from_str(host.as_str()) {
            let host = CFHostCreateWithName(kCFAllocatorDefault, host.as_concrete_TypeRef());
            let ctx = CFHostClientContext {
                copy_descr: ptr::null(),
                info: Box::into_raw(msg) as *mut c_void,
                release: ptr::null(),
                retain: ptr::null(),
                version: 0,
            };
            let ctxp = mem::transmute::<&CFHostClientContext, *mut c_void>(&ctx);
            CFHostSetClient(host, resolv_cb, ctxp);
            let rl = CFRunLoop::get_current();
            CFHostScheduleWithRunLoop(host, rl.as_concrete_TypeRef(), kCFRunLoopDefaultMode);
            let mut err = CFStreamError {
                domain: 0,
                error: 0,
            };
            CFHostStartInfoResolution(host, kCFHostAddresses, &mut err);
        }
        ptr::null()
    }

    pub type CFHostRef = *const c_void;
    #[repr(C)]
    struct CFStreamError {
        domain: c_long,
        error: i32,
    }
    #[repr(C)]
    struct CFHostClientContext {
        copy_descr: *const c_void,
        info: *mut c_void,
        release: *const c_void,
        retain: *const c_void,
        version: i32,
    }
    pub const kCFHostAddresses:i32 = 0;
    pub const kCFHostNames:i32 = 1;
    pub const kCFHostReachability:i32 = 2;
    type CFHostClientCallBack = unsafe extern fn(theHost: CFHostRef, typeInfo: i32, error: *const CFStreamError, info: *mut c_void);

    extern "C" {
        fn CFHostCreateWithName(allocator: CFAllocatorRef, hostname: CFStringRef) -> CFHostRef;
        fn CFHostSetClient(theHost: CFHostRef, clientCB: CFHostClientCallBack, clientContext: *mut c_void) -> u8;
        fn CFHostScheduleWithRunLoop(theHost: CFHostRef, runLoop: CFRunLoopRef, runLoopMode: CFStringRef);
        fn CFHostStartInfoResolution(theHost: CFHostRef,  info: i32, error: *const CFStreamError);
        fn CFHostGetAddressing(theHost: CFHostRef, hasBeenResolved: *mut u8) -> CFArrayRef;
    }

    // Return reponse
    unsafe extern fn resolv_cb(host: CFHostRef, typeInfo: i32, error: *const CFStreamError, info: *mut c_void) {
        let mut req:Box<ThreadMsg> = Box::from_raw(info as *mut ThreadMsg);
        println!("resolv cb {}",req.id);
        let mut resp = ptr::null_mut();
        if (*error).error == 0 {
            let mut succ = 0;
            let array = CFArray::wrap_under_get_rule(CFHostGetAddressing(host, &mut succ));
            if succ > 0 {
                for i in 0..array.len() {
                    let addr = CFDataGetBytePtr(array.get(i) as CFDataRef) as *const sockaddr;
                    match (*addr).sa_family as i32 {
                        AF_INET => {
                            let in_addr = addr as *const sockaddr_in;
                            let l = (*in_addr).sin_addr.s_addr;
                            resp = Box::into_raw(Box::new(format!("{}.{}.{}.{}",
                                l & 255, (l >> 8) & 255,(l >> 16) & 255, l >> 24)));
                            // println!("afinet! {}.{}.{}.{}", l >> 24, (l >> 16) & 255, (l >> 8) & 255, l & 255);
                        }
                        AF_INET6 => {
                        }
                        _ => {}
                    }
                }
            }
        }
        req.resp = resp;
        TX.with(|tx|{
            let tx = (*tx).borrow();
            if let Some(ref t) = *tx {
                let _ = t.send(ThreadResp{
                    resp: info,
                });
            }
        });
    }
}

// TODO: On windows for native lookup we need to use:
// https://msdn.microsoft.com/en-us/library/windows/desktop/aa365968(v=vs.85).aspx
#[cfg(not(target_os = "macos"))]
mod implemented {
    use std::io;
    use std::net::{UdpSocket,SocketAddrV4};
    use ::dns_parser;
    struct Dns {
        srvs: Vec<String>,
    }

    impl Dns {
        pub fn new() -> Dns {
            Dns {
                srvs: get_dns_servers(),
            }
        }

        pub fn start_lookup(&self, id: usize, host: &str) -> io::Result<Option<UdpSocket>> {
            let mut socket = UdpSocket::bind("0.0.0.0:0")?;
            let ip = srvs[0].parse()?;
            let sockaddr = SocketAddrV4::new(ip, 53);

            let mut buf_send = [0; 512];
            let nsend = {
                let mut builder = dns_parser::Builder::new(&mut buf_send[..]);
                builder.start(rand::random::<u16>(), true);
                builder.add_question(host, 
                    dns_parser::QueryType::A,
                    dns_parser::QueryClass::IN);
                builder.finish()
            };
            socket.send_to(&buf_send[..nsend], &sockaddr)?;
            Ok(Some(UdpSocket))
        }

        pub fn check_result(&self) -> Option<(usize,Box<String>)> {
            None
        }
    }

    #[cfg(unix)]
    pub fn get_dns_servers() -> Vec<String> {
        if let Ok(mut file) = ::std::fs::File::open("/etc/resolv.conf") {
            let mut contents = String::new();
            use std::io::Read;
            if file.read_to_string(&mut contents).is_ok() {
                return resolv_parse(contents);
            }
        }
        get_google()
    }

    #[cfg(windows)]
    pub fn get_dns_servers() -> Vec<String> {
        get_google()
    }

    fn get_google() -> Vec<String> {
        vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    }

    fn resolv_parse(s: String) -> Vec<String> {
        let mut out = Vec::with_capacity(2);
        for line in s.lines() {
            let mut words = line.split_whitespace();
            if let Some(s) = words.next() {
                if s.starts_with("nameserver") {
                    if let Some(s) = words.next() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        out
    }
}

// #[cfg(target_os = "macos")]
// pub fn get_dns_servers() -> Vec<String> {
//     let out = ::std::process::Command::new("scutil")
//         .arg("--dns")
//         .output();
//     if let Ok(out) = out {
//         if let Ok(s) = String::from_utf8(out.stdout) {
//             return scutil_parse(s);
//         }
//     }
//     get_google()
// }
// fn scutil_parse(s: String) -> Vec<String> {
//     let mut out = Vec::with_capacity(2);
//     for line in s.lines() {
//         let mut words = line.split_whitespace();
//         if let Some(s) = words.next() {
//             if s.starts_with("nameserver[") {
//                 if let Some(s) = words.next() {
//                     if s == ":" {
//                         if let Some(s) = words.next() {
//                             out.push(s.to_string());
//                         }
//                     }
//                 }
//             }
//         }
//     }
//     out
// }

