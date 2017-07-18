use mio::{Events, Poll, PollOpt, Ready, Token};
use mio::net::{TcpStream,UdpSocket,TcpListener};
use std::net::SocketAddr;
use mio::event::Evented;
use context::{Context, Transfer};
use context::stack::ProtectedFixedSizeStack;
use std::collections::VecDeque;
use std::time::{Duration,Instant};
use std::io;
use std::mem::transmute;
use std::io::{Read,Write};
use std::mem::swap;
use std::ptr;
use std::os::raw::{c_void};
use fiber::*;
use std::marker::PhantomData;
use wheel::Wheel;
use builder::Builder;
use native_tls::{TlsAcceptor, TlsStream, TlsConnector, HandshakeError};

/// Poller that executes fibers on main stack.
pub struct Poller<P,R> {
    p: PhantomData<P>,
    r: PhantomData<R>,
}

type TlsResult = Result<TlsStream<TcpStream>,HandshakeError<TcpStream>>;

impl<P,R> Poller<P,R> {
    /// Start poller. This function does not block.
    /// stack_size will be rounded up to next power of two.
    /// If not set default value of context-rs will be used.
    pub fn new(stack_size: Option<usize>) -> io::Result<Poller<P,R>> {
        unsafe {
            if get_runner() != ptr::null_mut() {
                return Err(io::Error::new(io::ErrorKind::Other, "poller exists"));
            }
            start_runner::<P,R>(stack_size)?;
        }
        Ok(Poller {
            p: PhantomData,
            r: PhantomData,
        })
    }

    /// Execute fibers and timers. 
    /// Returns true if anything changed and you should call pop_response and pop_fiber.
    ///
    /// Set wait to maximum amount of time poller should wait for sockets to wake up. This affects
    /// socket timers so do not set it too large. Recommended values between 0 (if you have
    /// something else to do) and 250.
    pub fn poll(&self, wait: Duration) -> bool {
        runner::<P,R>().poll(wait)
    }

    /// Get a response from fiber if any available.
    pub fn get_response(&self) -> Option<R> {
        runner::<P,R>().iter_responses()
    }

    /// Get a fiber if any that has called join_main and is blocking waiting for
    /// main stack to resume it.
    pub fn get_fiber(&self) -> Option<FiberRef<P,R>> {
        runner::<P,R>().iter_fibers()
    }

    /// Start fiber on TCP socket.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(None, func, FiberSock::Tcp(tcp), param, None).map(|_|{()})
    }

    /// Start fiber on TCP listener.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_listener(&self, tcp: TcpListener, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(None, func, FiberSock::Listener(tcp), param, None).map(|_|{()})
    }

    /// Start fiber on UDP socket.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(None, func, FiberSock::Udp(udp), param, None).map(|_|{()})
    }
}

impl<P,R> Drop for Poller<P,R> {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(runner::<P,R>());
            set_runner(ptr::null_mut());
        };
    }
}

pub(crate) struct RunnerInt<P,R> {
    events: Events,
    poll: Poll,
    free_stacks: VecDeque<ProtectedFixedSizeStack>,
    // sock at index X, has a transfer at index X
    fibers: Vec<FiberInt<P,R>>,
    free_fibers: VecDeque<usize>,
    toexec_fibers: VecDeque<usize>,
    tomain_fibers: VecDeque<FiberRef<P,R>>,
    results: VecDeque<R>,
    wheel: Wheel,
    stack_size: Option<usize>,
}

impl<P,R> RunnerInt<P,R> {
    fn new(stack_size: Option<usize>) -> io::Result<RunnerInt<P,R>> {
        Ok(RunnerInt {
            stack_size,
            poll: Poll::new().expect("unable to start poller"),
            events: Events::with_capacity(1024),
            free_stacks: VecDeque::with_capacity(4),
            fibers: Vec::new(),
            free_fibers: VecDeque::with_capacity(4),
            toexec_fibers: VecDeque::with_capacity(4),
            tomain_fibers: VecDeque::with_capacity(4),
            results: VecDeque::new(),
            wheel: Wheel::new(&Builder::new().tick_duration(Duration::from_millis(250))),
        })
    }

    pub(crate) fn socket_timeout(&mut self, pos: usize, dur: Option<Duration>) {
        self.fibers[pos].timeout = dur;
    }

    // pub(crate) fn close(&mut self, pos: usize) {
    //     self.fibers[pos].state = FiberState::Closed;
    // }

    pub(crate) fn read(&mut self, pos: usize, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.fibers[pos].sock.read(buf) {
                Ok(r) => {
                    if r == 0 && buf.len() > 0 {
                        return Err(io::Error::new(io::ErrorKind::NotConnected,"socket closed"));
                    } else {
                        return Ok(r);
                    }
                } 
                Err(err) => {
                    if err.kind() == io::ErrorKind::WouldBlock {
                        // self.fibers[pos].blocking_on = Ready::readable();
                        // let mut rdy = self.fibers[pos].ready;
                        if !self.fibers[pos].ready.is_readable() {
                            self.fibers[pos].register(&self.poll, Ready::readable())?;
                        }
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out {
                            self.fibers[pos].timed_out = false;
                            return Err(io::Error::new(io::ErrorKind::TimedOut,"read timeout"));
                        }
                    } else if err.kind() == io::ErrorKind::Interrupted {
                    } else {
                        // self.fibers[pos].state = FiberState::Closed;
                        return Err(err)
                    }
                }
            }
        }
    }

    pub(crate) fn write(&mut self, pos: usize, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.fibers[pos].sock.write(buf) {
                Ok(r) =>{
                    if r == 0 && buf.len() == 0 {
                        return Err(io::Error::new(io::ErrorKind::NotConnected,"socket closed"));
                    } else {
                        return Ok(r);
                    }
                }
                Err(err) => {
                    if err.kind() == io::ErrorKind::WouldBlock {
                        // self.fibers[pos].blocking_on = Ready::writable();
                        // let mut rdy = self.fibers[pos].ready;
                        if !self.fibers[pos].ready.is_writable() {
                            self.fibers[pos].register(&self.poll, Ready::writable())?;
                            // self.poll.register(self.fibers[pos].evented(), Token(pos), rdy, PollOpt::level())?;
                        }
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out {
                            self.fibers[pos].timed_out = false;
                            return Err(io::Error::new(io::ErrorKind::TimedOut,"write timeout"));
                        }
                    } else if err.kind() == io::ErrorKind::Interrupted {
                    } else {
                        // self.fibers[pos].state = FiberState::Closed;
                        return Err(err)
                    }
                }
            }
        }
    }

    pub(crate) fn accept_tcp(&mut self, pos: usize) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            match self.fibers[pos].sock.accept_tcp() {
                Ok(r) => {
                    return Ok(r);
                }
                Err(err) => {
                    if err.kind() == io::ErrorKind::WouldBlock {
                        self.step_out(pos);
                    } else if err.kind() == io::ErrorKind::Interrupted {
                    } else {
                        return Err(err)
                    }
                }
            }
        }
    }

    pub(crate) fn tcp_tls_connect(&mut self, pos: usize, con: TlsConnector, domain: &str) -> io::Result<()> {
        let mut sock = FiberSock::Empty;
        swap(&mut self.fibers[pos].sock, &mut sock);
        if let FiberSock::Tcp(tcp) = sock {
            let cr = con.connect(domain, tcp);
            self.tls_handshake(pos, cr)
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid socket"));
        }
    }

    pub(crate) fn tcp_tls_accept(&mut self, pos: usize, con: TlsAcceptor) -> io::Result<()> {
        let mut sock = FiberSock::Empty;
        swap(&mut self.fibers[pos].sock, &mut sock);
        if let FiberSock::Tcp(tcp) = sock {
            let cr = con.accept(tcp);
            self.tls_handshake(pos, cr)
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid socket"));
        }
    }

    fn tls_handshake(&mut self, pos: usize, mut cr: TlsResult) -> io::Result<()> {
        loop {
                match cr {
                    Ok(tls) => {
                        self.fibers[pos].sock = FiberSock::Tls(tls);
                        return Ok(());
                    }
                    Err(HandshakeError::Interrupted(mid)) => {
                        self.fibers[pos].sock = FiberSock::TlsTemp(mid);
                        self.fibers[pos].register(&self.poll, Ready::readable())?;
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out {
                            self.fibers[pos].timed_out = false;
                            return Err(io::Error::new(io::ErrorKind::TimedOut,"write timeout"));
                        }
                        let mut sock = FiberSock::Empty;
                        swap(&mut self.fibers[pos].sock, &mut sock);
                        if let FiberSock::TlsTemp(mid) = sock {
                            cr = mid.handshake();
                        } else {
                            return Err(io::Error::new(io::ErrorKind::Other, "tls failure"));
                        }
                    }
                    Err(HandshakeError::Failure(_)) => {
                        return Err(io::Error::new(io::ErrorKind::Other, "tls failure"));
                    }
                }
            }
    }

    pub(crate) fn flush(&mut self, pos: usize) -> io::Result<()> {
        self.fibers[pos].sock.flush()
    }

    pub(crate) fn join_main(&mut self, pos: usize) -> R {
        self.tomain_fibers.push_back(FiberRef::new(pos));
        self.step_out(pos);
        let mut rs = None;
        swap(&mut self.fibers[pos].result, &mut rs);
        rs.unwrap()
    }

    pub(crate) fn resume_fiber(&mut self, pos: usize, resp: R) {
        self.fibers[pos].result = Some(resp);
        self.toexec_fibers.push_back(pos);
    }

    pub(crate) fn register(&mut self, origin:Option<usize>, func: FiberFn<P,R>, 
            ft: FiberSock, param: P, parent: Option<usize>) -> io::Result<Option<usize>> {

        let mut fiber = FiberInt {
            ready: Ready::empty(),
            param: Some(param),
            me: 0,
            state: FiberState::Unstacked,
            func,
            sock: ft,
            t: None,
            stack: None,
            parent,
            children: VecDeque::new(),
            result: None,
            wtoken: None,
            timeout: Some(Duration::from_millis(20000)),
            timed_out: false,
        };
        let pos = match self.free_fibers.pop_front() {
            Some(free_pos) => {
                fiber.me = free_pos;
                if let FiberSock::Listener(_) = fiber.sock {
                    fiber.register(&self.poll, Ready::readable())?;
                }
                self.fibers[free_pos] = fiber;
                free_pos
            }
            _ => {
                fiber.me = self.fibers.len();
                if let FiberSock::Listener(_) = fiber.sock {
                    fiber.register(&self.poll, Ready::readable())?;
                }
                self.fibers.push(fiber);
                self.fibers.len() - 1
            }
        };
        self.toexec_fibers.push_back(pos);
        if let Some(parent) = parent {
            self.fibers[parent].children.push_back(pos);
            return Ok(Some(pos));
        }
        Ok(None)
    }

    pub(crate) fn child_iter(&mut self, parent: usize) -> Option<R> {
        loop {
            if self.fibers[parent].children.len() == 0 {
                return None;
            }
            let mut child = None;
            let mut index = 0;
            for c in self.fibers[parent].children.iter() {
                if self.fibers[*c].state == FiberState::Closed {
                    child = Some(*c);
                    break;
                }
                else if self.fibers[*c].result.is_some() {
                    child = Some(*c);
                    break;
                }
                index += 1;
            }
            if child.is_some() {
                let child = child.unwrap();
                let mut out = None;
                swap(&mut self.fibers[child].result, &mut out);
                if self.fibers[child].state == FiberState::Closed {
                    self.fibers[parent].children.swap_remove_front(index);
                    self.free_fibers.push_back(child);
                } else {
                    // Now that we have taken its result schedule it back for execution.
                    self.toexec_fibers.push_back(child);
                }

                if out.is_some() {
                    return out;
                }
            } else {
                self.step_out(parent);
            }
        }
    }

    pub(crate) fn resp_chunk(&mut self, pos: usize, chunk: R) {
        self.fibers[pos].result = Some(chunk);
        self.step_out(pos);
    }

    fn get_stack(&mut self) -> ProtectedFixedSizeStack {
        match self.free_stacks.pop_front() {
            None if self.stack_size.is_none() => {
                ProtectedFixedSizeStack::default()
            }
            None => {
                ProtectedFixedSizeStack::new(self.stack_size.unwrap()).expect("Can not create stack")
            }
            Some(stack) => {
                stack
            }
        }
    }

    fn timed_step_out(&mut self, pos: usize) {
        if let Some(dur) = self.fibers[pos].timeout {
            if let Some(tk) = self.wheel.reserve() {
                self.wheel.set_timeout(tk, Instant::now() + dur, pos);
                self.fibers[pos].wtoken = Some(tk);
            }
        }
        self.step_out(pos);
        if let Some(tk) = self.fibers[pos].wtoken {
            self.wheel.cancel(tk);
            self.fibers[pos].wtoken = None;
        }
    }

    // Fiber -> Main stack
    fn step_out(&mut self, pos: usize) {
        let mut t = None;
        swap(&mut self.fibers[pos].t, &mut t);
        let mut t = t.unwrap();
        // t = unsafe { t.context.resume(transmute::<&Fiber, usize>(&self.fibers[pos])) };
        t = unsafe { t.context.resume(0) };
        let mut t = Some(t);
        swap(&mut self.fibers[pos].t, &mut t);
    }

    // Main stack -> Fiber
    fn step_into(&mut self, pos: usize) {
        if self.fibers[pos].t.is_none() {
            let stack = self.get_stack();
            let fiber = &mut self.fibers[pos];
            let t = Transfer::new(unsafe { Context::new(&stack, context_function::<P,R>) }, 0);
            fiber.stack = Some(stack);
            fiber.t = Some(t);
        }
        let mut t = None;
        swap(&mut self.fibers[pos].t, &mut t);
        let mut t = t.unwrap();
        t = unsafe { t.context.resume(transmute::<&FiberInt<P,R>, usize>(&self.fibers[pos])) };
        let mut t = Some(t);
        swap(&mut self.fibers[pos].t, &mut t);

        self.stepped_out(pos);
    }

    fn unstack(&mut self, pos: usize) {
        let mut stack = None;
        swap(&mut self.fibers[pos].stack, &mut stack);
        let stack = stack.unwrap();
        self.fibers[pos].t = None;
        self.free_stacks.push_front(stack);
    }

    // Back on main stack, check state if we should clean up.
    fn stepped_out(&mut self, pos: usize) {
        if self.fibers[pos].state != FiberState::Stacked && self.fibers[pos].t.is_some() {
            self.unstack(pos);
        }
        if self.fibers[pos].state == FiberState::Closed {
            self.fibers[pos].sock = FiberSock::Empty;
            if let Some(parent) = self.fibers[pos].parent {
                // if we have parent, we put results in there
                // and step into it.
                // Parent may not actually be blocking on this child though.
                // It may also be blocking on its socket. Which is why 
                // we push it to free_fibers on iter not here.
                self.step_into(parent);
            } else {
                self.take_result(pos);
                self.free_fibers.push_back(pos);
            }
        } else if let Some(parent) = self.fibers[pos].parent {
            if self.fibers[pos].result.is_some() {
                // Parent iterator will reschedule this fiber to continue
                // once it takes its result.
                self.step_into(parent);
            }
        } else if let Some(_) = self.fibers[pos].result {
            // No parent and we have result. Take it, then reschedule it back
            // on next poll().
            self.take_result(pos);
            self.toexec_fibers.push_back(pos);
        }
    }

    fn take_result(&mut self, pos: usize) {
        let mut rs = None;
        swap(&mut self.fibers[pos].result, &mut rs);
        if let Some(r) = rs {
            self.results.push_back(r);
        }
    }

    fn iter_responses(&mut self) -> Option<R> {
        self.results.pop_front()
    }

    fn iter_fibers(&mut self) -> Option<FiberRef<P,R>> {
        self.tomain_fibers.pop_front()
    }

    fn signaled(&self) -> bool {
        !self.tomain_fibers.is_empty() || !self.results.is_empty()
    }

    fn poll(&mut self, mut dur: Duration) -> bool {
        while let Some(pos) = self.toexec_fibers.pop_front() {
            self.step_into(pos);
        }

        // no waiting if fibers waiting for main stack
        if self.tomain_fibers.len() > 0 {
            dur = Duration::from_millis(0);
        }
        if let Err(_) = self.poll.poll(&mut self.events, Some(dur)) {
            return self.signaled();
        }

        if self.events.len() > 0 {
            let mut events = Events::with_capacity(0);
            swap(&mut self.events, &mut events);
            for ev in events.into_iter() {
                let pos = ev.token().0;
                self.step_into(pos);
            }
            ::std::mem::swap(&mut self.events, &mut events);
        }

        let now = Instant::now();
        while let Some(pos) = self.wheel.poll(now) {
            if let Some(_) = self.fibers[pos].wtoken {
                self.fibers[pos].wtoken = None;
                self.fibers[pos].timed_out = true;
                self.step_into(pos);
            }
        }
        self.signaled()
    }
}

extern "C" fn context_function<P,R>(t: Transfer) -> ! {
        let fiber = unsafe { transmute::<usize, &mut FiberInt<P,R>>(t.data) };
        fiber.state = FiberState::Stacked;
        fiber.t = Some(t);
        let mut param = None;
        swap(&mut fiber.param, &mut param);
        let param = param.unwrap();
        fiber.result = (fiber.func)(Fiber::new(fiber.me), param);
        // if fiber.state == FiberState::Stacked {
            fiber.state = FiberState::Closed;
        // }
        let mut t = None;
        swap(&mut fiber.t, &mut t);
        let t = t.unwrap();
        unsafe { t.context.resume(0) };
    loop {
    }
}
extern "C" {
    fn get_runner() -> *mut c_void;
    fn set_runner(r: *mut c_void);
}

fn start_runner<'a,P,R>(stack_size: Option<usize>) -> io::Result<()> {
    unsafe {
        let r: *mut RunnerInt<P,R> = Box::into_raw(Box::new(RunnerInt::new(stack_size)?));
        set_runner(r as *mut c_void);
        Ok(())
    }
}

pub(crate) fn runner<'a,P,R>() -> &'a mut RunnerInt<P,R> {
    unsafe {
        let rc = get_runner();
        ::std::mem::transmute::<*mut c_void, &mut RunnerInt<P,R>>(rc)
    }
}


