use mio::{Ready};
use mio::net::{TcpStream,UdpSocket,TcpListener};
use std::net::{SocketAddr,IpAddr};
use context::{Context, Transfer};
use std::collections::VecDeque;
use std::time::{Duration};
use std::io;
use std::mem::transmute;
use std::io::{Read,Write};
use std::mem::swap;
use std::ptr;
use std::os::raw::{c_void};
use fiber::*;
use std::marker::PhantomData;
use native_tls::{TlsAcceptor, TlsStream, TlsConnector, HandshakeError};
use dns::{dns_parse};
use poller::{self, poller};

/// Runner executes fibers on main stack. There can be multiple pollers each with its own fibers
/// of a particular type.
pub struct Runner<P,R> {
    pos: i8,
    p: PhantomData<P>,
    r: PhantomData<R>,
}

type TlsResult = Result<TlsStream<TcpStream>,HandshakeError<TcpStream>>;

impl<P,R> Runner<P,R> {
    /// Start runner. This function does not block. Can fail only if we ran out of space for new runners.
    /// ATM max runners is 20.
    pub fn new() -> io::Result<Runner<P,R>> {
        let pos = start_runner::<P,R>()?;
        Ok(Runner {
            pos,
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
    pub fn run(&self) -> bool {
        runner::<P,R>(self.pos).run()
    }

    /// Get a response from fiber if any available.
    pub fn get_response(&self) -> Option<R> {
        runner::<P,R>(self.pos).iter_responses()
    }

    /// Get a fiber if any that has called join_main and is blocking waiting for
    /// main stack to resume it.
    pub fn get_fiber(&self) -> Option<FiberRef<P,R>> {
        runner::<P,R>(self.pos).iter_fibers()
    }

    /// Resolve domain, connect and run fiber.
    /// In case domain resolve or connect fails, fiber will still be run but all
    /// socket operations will fail.
    pub fn new_resolve_connect(&self, 
            domain: &str,
            st: SocketType,
            port: u16,
            timeout: Duration,
            func: FiberFn<P,R>,
            param: P) -> io::Result<()> {
        runner::<P,R>(self.pos).new_resolve(None, domain, Some(func),timeout, st, port, param).map(|_|{()})
    }

    /// Start fiber on TCP socket.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner(self.pos).register(Some(func), FiberSock::Tcp(tcp), Some(param), None).map(|_|{()})
    }

    /// Start fiber on TCP listener.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_listener(&self, tcp: TcpListener, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner(self.pos).register(Some(func), FiberSock::Listener(tcp), Some(param), None).map(|_|{()})
    }

    /// Start fiber on UDP socket.
    ///
    /// This function does not block and fiber gets executed on next poll().
    pub fn new_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner(self.pos).register(Some(func), FiberSock::Udp(udp), Some(param), None).map(|_|{()})
    }

    /// Return resp after dur. Timer is one-shot.
    pub fn new_timer(&self, dur: Duration, resp: R) -> Option<TimerRef<P,R>> {
        runner::<P,R>(self.pos).new_timer(None, dur, None, Some(resp))
    }

    /// Run socket-less fiber after dur.
    pub fn new_fiber_timer(&self, dur: Duration, func: FiberFn<P,R>, param: P) -> Option<TimerRef<P,R>> {
        runner::<P,R>(self.pos).new_timer(Some(func), dur, Some(param), None)
    }
}

impl<P,R> Drop for Runner<P,R> {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(runner::<P,R>(self.pos));
            set_runner(self.pos, ptr::null_mut());
        };
    }
}

pub(crate) struct RunnerInt<P,R> {
    pos: i8,
    fibers: Vec<FiberInt<P,R>>,
    free_fibers: VecDeque<usize>,
    tomain_fibers: VecDeque<FiberRef<P,R>>,
    results: VecDeque<R>,
}

impl<P,R> RunnerInt<P,R> {
    fn new(pos: i8) -> io::Result<RunnerInt<P,R>> {
        Ok(RunnerInt {
            pos,
            fibers: Vec::new(),
            free_fibers: VecDeque::with_capacity(4),
            tomain_fibers: VecDeque::with_capacity(4),
            results: VecDeque::new(),
        })
    }

    pub(crate) fn socket_timeout(&mut self, pos: usize, dur: Option<Duration>) {
        self.fibers[pos].timeout = dur;
    }

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
                            self.fibers[pos].register(&poller().poller(), Ready::readable())?;
                        }
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out > 0 {
                            self.fibers[pos].timed_out = 0;
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
                            self.fibers[pos].register(&poller().poller(), Ready::writable())?;
                            // self.poll.register(self.fibers[pos].evented(), Token(pos), rdy, PollOpt::level())?;
                        }
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out > 0 {
                            self.fibers[pos].timed_out = 0;
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
                        self.fibers[pos].register(&poller().poller(), Ready::readable())?;
                        self.timed_step_out(pos);
                        if self.fibers[pos].timed_out > 0 {
                            self.fibers[pos].timed_out = 0;
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
        self.tomain_fibers.push_back(FiberRef::new(self.pos, pos));
        self.step_out(pos);
        let mut rs = None;
        swap(&mut self.fibers[pos].result, &mut rs);
        rs.unwrap()
    }

    pub(crate) fn resume_fiber(&mut self, pos: usize, resp: R) {
        self.fibers[pos].result = Some(resp);
        self.push_toexec(pos);
    }

    pub(crate) fn new_timer(&mut self, 
        func: Option<FiberFn<P,R>>,
        dur: Duration,
        param: Option<P>,
        resp: Option<R>) -> Option<TimerRef<P,R>> {

        if let Ok(pos) = self.register(func, FiberSock::Empty, param, None) {
            self.fibers[pos].result = resp;
            if let Some(tk) = poller().timer_reg(self.fiber_id(pos), Some(dur)) {
                self.fibers[pos].wtoken = Some(tk);
                return Some(TimerRef::new(self.pos, pos));
            }
            // if let Some(tk) = poller().wheel.reserve() {
            //     poller().wheel.set_timeout(tk, Instant::now() + dur, pos);
            //     self.fibers[pos].wtoken = Some(tk);
            //     return Some(TimerRef::new(self.pos, pos));
            // }
        }
        None
    }

    pub(crate) fn cancel_timer(&mut self, pos: usize) {
        if let Some(tk) = self.fibers[pos].wtoken {
            poller().timer_cancel(tk);
            self.fibers[pos].wtoken = None;
            self.fibers[pos].result = None;
            self.fibers[pos].param = None;
            self.free_fibers.push_back(pos);
        }
    }

    fn fiber_id(&self, pos: usize) -> usize {
        pos | ((self.pos as usize) << 40)
    }

    pub(crate) fn new_resolve(&mut self, 
            parent: Option<usize>,
            domain: &str, 
            func: Option<FiberFn<P,R>>,
            to: Duration, 
            st: SocketType, 
            port: u16, 
            param: P) -> io::Result<usize> {

        if domain.len() == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "empty domain"));
        }
        let pos = self.register(func, FiberSock::Empty, Some(param), parent)?;
        let mut cp = ConnectParam {
            port,
            st,
            // empty for now
            host: String::new(),
        };
        let poller = poller();
        let fiber_id = self.fiber_id(pos);
        self.fibers[pos].wtoken = poller.timer_reg(fiber_id, None);
        let mut timeout_ticks = ((to.as_secs() * 1000) + ((to.subsec_nanos() as u64) / 1000_000)) / poller::TICK_MS;
        if timeout_ticks == 0 {
            timeout_ticks = 1;
        } else if timeout_ticks > 255 {
            timeout_ticks = 255;
        }
        self.fibers[pos].timed_out = timeout_ticks as u8;
        match poller.dns().start_lookup(fiber_id, self.fibers[pos].generation, domain) {
            Ok(None) => {
                self.fibers[pos].connect_param = Some(cp);
                return Ok(pos);
            }
            Ok(Some(sock)) => {
                cp.host = domain.to_string();
                self.fibers[pos].connect_param = Some(cp);
                self.fibers[pos].sock = FiberSock::Udp(sock);
                self.fibers[pos].register(&poller.poller(), Ready::readable())?;
                return Ok(pos);
            }
            Err(e) => {
                self.fibers[pos].param = None;
                if let Some(parent) = self.fibers[pos].parent {
                    self.fibers[parent].children.pop_back();
                    self.fibers[pos].parent = None;
                }
                self.cancel_timer(pos);
                return Err(e);
            }
        }
    }

    pub(crate) fn register(&mut self, func: Option<FiberFn<P,R>>, 
            ft: FiberSock, param: Option<P>, parent: Option<usize>) -> io::Result<usize> {

        let empty = ft.is_empty();
        let mut fiber = FiberInt {
            ready: Ready::empty(),
            runner: self.pos,
            param,
            me: 0,
            generation: 0,
            state: FiberState::Unstacked,
            func,
            connect_param: None,
            sock: ft,
            t: None,
            stack: None,
            parent,
            children: VecDeque::new(),
            result: None,
            wtoken: None,
            timeout: Some(Duration::from_millis(20000)),
            timed_out: 0,
        };
        let pos = match self.free_fibers.pop_front() {
            Some(free_pos) => {
                let gen = self.fibers[free_pos].generation;
                fiber.me = self.fiber_id(free_pos);
                fiber.generation = gen+1;
                if let FiberSock::Listener(_) = fiber.sock {
                    fiber.register(&poller().poller(), Ready::readable())?;
                }
                self.fibers[free_pos] = fiber;
                free_pos
            }
            _ => {
                fiber.me = self.fiber_id(self.fibers.len());
                if let FiberSock::Listener(_) = fiber.sock {
                    fiber.register(&poller().poller(), Ready::readable())?;
                }
                self.fibers.push(fiber);
                self.fibers.len() - 1
            }
        };
        // Only schedule for immediate execution if it has a socket.
        // Timers and DNS lookups don't.
        if !empty {
            self.push_toexec(pos);
        }
        if let Some(parent) = parent {
            self.fibers[parent].children.push_back(pos);
        }
        Ok(pos)
    }

    fn push_toexec(&self, pos: usize) {
        poller().toexec_push(self.pos as usize, pos);
    }

    fn pop_toexec(&self) -> Option<usize> {
        poller().toexec_pop(self.pos as usize)
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
                    self.push_toexec(child);
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

    pub(crate) fn hibernate_for_read(&mut self, pos: usize) -> io::Result<()> {
        self.fibers[pos].state = FiberState::Unstacked;
        if !self.fibers[pos].ready.is_readable() {
            self.fibers[pos].register(&poller().poller(), Ready::readable())?;
        }
        self.step_out(pos);
        Ok(())
    }

    fn timed_step_out(&mut self, pos: usize) {
        if let Some(dur) = self.fibers[pos].timeout {
            if let Some(tk) = poller().timer_reg(self.fibers[pos].me,Some(dur)) {
                self.fibers[pos].timed_out = 0;
                self.fibers[pos].wtoken = Some(tk);
            }
        }
        self.step_out(pos);
        if let Some(tk) = self.fibers[pos].wtoken {
            poller().timer_cancel(tk);
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
        if self.fibers[pos].func.is_some() {
            if self.fibers[pos].t.is_none() {
                let stack = poller().stack_pop();
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
        } else {
            self.fibers[pos].state = FiberState::Closed;
        }

        self.stepped_out(pos);
    }

    fn unstack(&mut self, pos: usize) {
        let mut stack = None;
        swap(&mut self.fibers[pos].stack, &mut stack);
        let stack = stack.unwrap();
        self.fibers[pos].t = None;
        poller().stack_push(stack);
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
            self.push_toexec(pos);
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

    fn have_addr(&mut self, pos: usize, addr: IpAddr) {
        let mut ok = true;
        {
            let fiber = &mut self.fibers[pos];
            fiber.sock = FiberSock::Empty;
            fiber.ready = Ready::empty();
            fiber.timed_out = 0;
            let mut cp = None;
            swap(&mut fiber.connect_param, &mut cp);
            let cp = cp.unwrap();
            match cp.st {
                SocketType::Tcp => {
                    if let Ok(tcp) = TcpStream::connect(&SocketAddr::new(addr,cp.port)) {
                        fiber.sock = FiberSock::Tcp(tcp);
                    }
                }
                SocketType::Udp(udp) => {
                    if let Ok(()) = udp.connect(SocketAddr::new(addr,cp.port)) {
                        fiber.sock = FiberSock::Udp(udp);
                    }
                }
            }
            fiber.connect_param = None;
            if fiber.register(&poller().poller(), Ready::writable()).is_err() {
                ok = false;
            }
        }
        // if error go straight into fiber so it errors out on socket operation.
        if !ok {
            self.step_into(pos);
        }
    }

    fn run(&mut self) -> bool {
        while let Some(pos) = self.pop_toexec() {
            if self.fibers[pos].connect_param.is_some() {
                let mut dns_buf = [0u8;512];
                if let Ok(nb) = self.fibers[pos].sock.read(&mut dns_buf) {
                    if let Some(adr) = dns_parse(&mut dns_buf[..nb]) {
                        if let Some(tk) = self.fibers[pos].wtoken {
                            poller().timer_cancel(tk);
                        }
                        self.fibers[pos].wtoken = None;
                        self.have_addr(pos, adr);
                    }
                }
            } else {
                self.step_into(pos);
            }
        }

        while let Some((pos,gen,addr)) = poller().dns_check(self.pos as usize) {
            {
                let fiber = &mut self.fibers[pos];
                if fiber.generation != gen {
                    continue;
                }
                if let Some(tk) = fiber.wtoken {
                    poller().timer_cancel(tk);
                    fiber.wtoken = None;
                }
            }
            self.have_addr(pos, addr);
        }

        while let Some(pos) = poller().timedout_pop(self.pos as usize) {
            if let Some(_) = self.fibers[pos].wtoken {
                // Check if this is timer on connect.
                if self.fibers[pos].connect_param.is_some() {
                    // DNS tick down
                    self.fibers[pos].timed_out -= 1;
                    if self.fibers[pos].timed_out > 0 {
                        let fiber_id = self.fiber_id(pos);
                        let fiber = &mut self.fibers[pos];
                        // When using our dns client, we have a UDP socket for it.
                        if let &FiberSock::Udp(ref udp) = &fiber.sock {
                            if let Some(ref cp) = fiber.connect_param {
                                let _ = poller().dns().lookup_on(udp, 
                                    fiber.timed_out as usize,
                                    cp.host.as_str());
                                // if let Some(tk) = poller().wheel.reserve() {
                                //     poller().wheel.set_timeout(tk, Instant::now() + Duration::from_millis(poller::TICK_MS), pos);
                                //     fiber.wtoken = Some(tk);
                                // }
                                fiber.wtoken = poller().timer_reg(fiber_id, None);
                            }
                        }
                    } else {
                        // DNS lookup timed out
                        self.fibers[pos].sock = FiberSock::Empty;
                        self.fibers[pos].wtoken = None;
                        self.step_into(pos);
                    }
                } else {
                    self.fibers[pos].wtoken = None;
                    self.fibers[pos].timed_out = 1;
                    self.step_into(pos);
                }
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
        fiber.result = (fiber.func.unwrap())(Fiber::new(fiber.runner, fiber.me & u32::max_value() as usize), param);
        fiber.state = FiberState::Closed;
        let mut t = None;
        swap(&mut fiber.t, &mut t);
        let t = t.unwrap();
        unsafe { t.context.resume(0) };
    loop {
    }
}
extern "C" {
    fn find_empty() -> i8;
    pub(crate) fn get_runner(pos: i8) -> *mut c_void;
    fn set_runner(pos: i8, r: *mut c_void);
}

fn start_runner<'a,P,R>() -> io::Result<i8> {
    unsafe {
        let pos =  find_empty();
        if pos == -1 {
            return Err(io::Error::new(io::ErrorKind::Other, "no space"));
        }
        let r: *mut RunnerInt<P,R> = Box::into_raw(Box::new(RunnerInt::new(pos)?));
        set_runner(pos, r as *mut c_void);
        Ok(pos)
    }
}

pub(crate) fn runner<'a,P,R>(pos: i8) -> &'a mut RunnerInt<P,R> {
    unsafe {
        let rc = get_runner(pos);
        ::std::mem::transmute::<*mut c_void, &mut RunnerInt<P,R>>(rc)
    }
}


