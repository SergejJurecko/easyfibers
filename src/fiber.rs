use runner::runner;
use std::io;
use std::io::{Read,Write};
use std::net::{SocketAddr};
use context::{Transfer};
use context::stack::ProtectedFixedSizeStack;
use mio::net::{TcpStream,UdpSocket,TcpListener};
use mio::event::Evented;
use mio::{Ready,Poll,PollOpt,Token};
use std::marker::PhantomData;
use wheel::Token as WToken;
use std::time::Duration;
use std::collections::VecDeque;
use native_tls::{TlsStream, TlsConnector, TlsAcceptor, MidHandshakeTlsStream};
/// Fiber execute function. It receives current fiber, parameter that was used
/// to start the fiber and returns result that will be result of fiber.
///
/// Generally you return from function once socket is closed or read/write times out.
///
/// This function will be called on an independent execution stack which is much more limited
/// so you must either be careful with how deep your stack can get, or set the stack to a high value.
///
/// You must test your code for any possible SIGBUS situations and be careful with calling external crates.
pub type FiberFn<P,R> = fn(Fiber<P,R>,P) -> Option<R>;

// /// Fiber execute function for DNS lookups.
// pub type FiberResolvedFn<P,R> = fn(Fiber<P,R>, io::Result<IpAddr>, P) -> Option<R>;

/// Available within the fiber execute function to configure fiber or create child fibers. Child fibers
/// return results to parent fibers instead of poller on main stack.
#[derive(PartialEq,Debug)]
pub struct Fiber<P,R> {
    pub(crate) id: usize,
    param: PhantomData<P>,
    resp: PhantomData<R>,
}

impl<P,R> Fiber<P,R> {
    pub(crate) fn new(pos: usize) -> Fiber<P,R> {
        Fiber {
            id: pos,
            param: PhantomData,
            resp: PhantomData,
        }
    }
}

impl<P,R> Fiber<P,R> {
    /// How long to wait on socket operations before timing out. You probably want to set something.
    pub fn socket_timeout(&self, t: Option<Duration>) {
        runner::<P,R>().socket_timeout(self.id, t)
    }

    /// Return intermediate result. A fiber can stream responses to poller or to its parent fiber.
    ///
    /// This function blocks fiber until response is received to poller or parent.
    pub fn resp_chunk(&self, chunk: R) {
        runner::<P,R>().resp_chunk(self.id, chunk);
    }

    /// Accept TCP socket. Works only on fibers from TcpListener.
    pub fn accept_tcp(&self) -> io::Result<(TcpStream, SocketAddr)> {
        runner::<P,R>().accept_tcp(self.id)
    }

    /// Convert fiber from TcpStream created with TcpStream::connect to TlsStream.
    pub fn tcp_tls_connect(&self, con: TlsConnector, domain: &str) -> io::Result<()> {
        runner::<P,R>().tcp_tls_connect(self.id, con, domain)
    }

    /// Convert fiber from TcpStream created with TcpListener::accept to TlsStream.
    pub fn tcp_tls_accept(&self, con: TlsAcceptor) -> io::Result<()> {
        runner::<P,R>().tcp_tls_accept(self.id, con)
    }

    /// Start fiber on TCP socket.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(func), FiberSock::Tcp(tcp), Some(param), None).map(|_|{()})
    }

    /// Start fiber on TCP listener.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_listener(&self, tcp: TcpListener, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(func), FiberSock::Listener(tcp), Some(param), None).map(|_|{()})
    }

    /// Start fiber on UDP socket.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(func), FiberSock::Udp(udp), Some(param), None).map(|_|{()})
    }

    /// Resolve domain, connect and run fiber.
    /// In case domain resolve or connect fails, fiber will still be run but all
    /// socket operations will fail.
    ///
    /// This function does not block, host lookup starts immediately. There is no relationship
    /// between calling and created fiber.
    pub fn new_resolve_connect(&self, 
            domain: &str,
            st: SocketType,
            port: u16,
            timeout: Duration,
            func: FiberFn<P,R>,
            param: P) -> io::Result<()> {
        runner::<P,R>().new_resolve(None, domain, Some(func),timeout, st, port, param).map(|_|{()})
    }

    /// Start a child fiber with tcp socket.
    ///
    /// This function does not block current fiber.
    pub fn join_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner::<P,R>().register(Some(func), FiberSock::Tcp(tcp), Some(param), Some(self.id)).map(|_|{()})
    }

    /// Start a child fiber with an udp socket.
    ///
    /// This function does not block current fiber.
    pub fn join_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner::<P,R>().register(Some(func), FiberSock::Udp(udp), Some(param), Some(self.id)).map(|_|{()})
    }
    
    /// Start a child fiber that resolves domain, connects and runs fiber.
    /// In case domain resolve or connect fails, fiber will still be run but all
    /// socket operations will fail.
    ///
    /// This function does not block, host lookup starts immediately.
    pub fn join_resolve_connect(&self, 
            domain: &str,
            st: SocketType,
            port: u16,
            timeout: Duration,
            func: FiberFn<P,R>,
            param: P) -> io::Result<()> {
        runner::<P,R>().new_resolve(Some(self.id), domain, Some(func),timeout, st, port, param).map(|_|{()})
    }

    /// Call main stack.
    ///
    /// This function blocks until main stack produces response.
    pub fn join_main(&self) -> R {
        runner::<P,R>().join_main(self.id)
    }

    /// Get result of child. If fiber has multiple children it will return first available result.
    /// Will block current fiber if nothing available immediately.
    ///
    /// If none is returned all children have finished executing.
    pub fn get_child(&self) -> Option<R> {
        runner::<P,R>().child_iter(self.id)
    }

    /// Remove stack from current fiber and reuse on other connections. Once socket becomes signalled
    /// for reads, resume from the start of FiberFn.
    pub fn hibernate_for_read(&self) -> io::Result<()> {
        runner::<P,R>().hibernate_for_read(self.id)
    }
}

// We use unwrap directly as we do not allow
// creating fiber without a poller.
impl<P,R> Read for Fiber<P,R> {
    /// Read data from socket. If no data is available to read, fiber will be scheduled out for another one,
    /// until there is data available.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        runner::<P,R>().read(self.id, buf)
    }
}

impl<'a,P,R> Read for &'a Fiber<P,R> {
    /// Read data from socket. If no data is available to read, fiber will be scheduled out for another one,
    /// until there is data available.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        runner::<P,R>().read(self.id, buf)
    }
}

impl<P,R> Write for Fiber<P,R> {
    /// Write data to socket. If data can not be written at this time, 
    /// fiber will be scheduled out for another one until there is data available.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        runner::<P,R>().write(self.id, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        runner::<P,R>().flush(self.id)
    }
}

/// Reference to fiber on main stack.
pub struct FiberRef<P,R> {
    pub(crate) id: usize,
    p: PhantomData<P>,
    r: PhantomData<R>,
}

impl<P,R> FiberRef<P,R> {
    pub(crate) fn new(id: usize) -> FiberRef<P,R> {
        FiberRef {
            id,
            p: PhantomData,
            r: PhantomData,
        }
    }

    /// Resume fiber with response from main stack.
    ///
    /// This function does not block, fiber gets resumed on next poll().
    pub fn resume_fiber(self, resp: R) {
        runner::<P,R>().resume_fiber(self.id, resp);
    }
}

/// Reference to timer on main stack.
pub struct TimerRef<P,R> {
    pub(crate) id: usize,
    p: PhantomData<P>,
    r: PhantomData<R>,
}

impl<P,R> TimerRef<P,R> {
    pub(crate) fn new(id: usize) -> TimerRef<P,R> {
        TimerRef {
            id,
            p: PhantomData,
            r: PhantomData,
        }
    }
}

impl<P,R> Drop for TimerRef<P,R> {
    fn drop(&mut self) {
        runner::<P,R>().cancel_timer(self.id);
    }
}

#[derive(PartialEq)]
pub(crate) enum FiberState {
    Closed,
    Stacked,
    Unstacked,
}

/// Socket type to connect to using new_resolve_connect function.
/// For SSL/TLS use tcp first, then call tcp_tls_connect inside fiber.
pub enum SocketType {
    Tcp,
    Udp(UdpSocket),
}

pub(crate) struct ConnectParam {
    pub(crate) port: u16,
    pub(crate) st: SocketType,
    pub(crate) host: String,
}

pub(crate) struct FiberInt<P,R> {
    pub(crate) ready: Ready,
    // pub(crate) blocking_on: Ready,
    pub(crate) param: Option<P>,
    pub(crate) state: FiberState,
    pub(crate) me: usize,
    pub(crate) func: Option<FiberFn<P,R>>,
    // pub(crate) resolv_func: Option<FiberResolvedFn<P,R>>,
    pub(crate) connect_param: Option<ConnectParam>,
    pub(crate) sock: FiberSock,
    // Only when executing is stack and transfer here
    pub(crate) t: Option<Transfer>,
    pub(crate) stack: Option<ProtectedFixedSizeStack>,
    // If a fiber is waiting on it
    pub(crate) parent: Option<usize>,
    pub(crate) children: VecDeque<usize>,
    // Will hold result of fiber function.
    pub(crate) result: Option<R>,
    pub(crate) wtoken: Option<WToken>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) timed_out: u8,
}

impl<P,R> FiberInt<P,R> {
    pub(crate) fn evented(&self) -> io::Result<&Evented> {
        self.sock.evented()
    }

    pub(crate) fn register(&mut self, poll: &Poll, rdy: Ready) -> io::Result<()> {
        let res = if self.ready.is_empty() {
            poll.register(self.evented()?, Token(self.me), rdy, PollOpt::edge())
        } else {
            poll.reregister(self.evented()?, Token(self.me), rdy, PollOpt::edge())
        };
        self.ready = rdy;
        res
    }
}

pub(crate) enum FiberSock {
    Tcp(TcpStream),
    Udp(UdpSocket),
    Listener(TcpListener),
    // TlsStream starts as TcpStream but morphs into TlsStream
    // either through TlsAcceptor or TlsConnector
    Tls(TlsStream<TcpStream>),
    TlsTemp(MidHandshakeTlsStream<TcpStream>),
    Empty,
}

impl FiberSock {
    pub fn is_empty(&self) -> bool {
        match self {
            &FiberSock::Empty => true,
            _ => false,
        }
    }
    pub fn evented(&self) -> io::Result<&Evented> {
        match self {
            &FiberSock::Tcp(ref tcp) => Ok(tcp),
            &FiberSock::Udp(ref udp) => Ok(udp),
            &FiberSock::Listener(ref tcp) => Ok(tcp),
            &FiberSock::TlsTemp(ref tcp) => Ok(tcp.get_ref()),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput,"no socket")),
        }
    }

    pub fn accept_tcp(&self) -> io::Result<(TcpStream, SocketAddr)> {
        match self {
            &FiberSock::Listener(ref tcp) => {
                tcp.accept()
            }
            _ => {
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not accept on non listen socket"))
            }
        }
    }
}

impl Read for FiberSock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match *self {
            FiberSock::Tcp(ref mut tcp) => tcp.read(buf),
            FiberSock::Udp(ref mut udp) => udp.recv(buf),
            FiberSock::Listener(_) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not read on listen socket")),
            FiberSock::Tls(ref mut tcp) => tcp.read(buf),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput,"no socket")),
        }
    }
}

impl Write for FiberSock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match *self {
            FiberSock::Tcp(ref mut tcp) => tcp.write(buf),
            FiberSock::Udp(ref mut udp) => udp.send(buf),
            FiberSock::Listener(_) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not write on listen socket")),
            FiberSock::Tls(ref mut tcp) => tcp.write(buf),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput,"no socket")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match *self {
            FiberSock::Tcp(ref mut tcp) => tcp.flush(),
            FiberSock::Udp(_) => Ok(()),
            FiberSock::Listener(_) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not flush on listen socket")),
            FiberSock::Tls(ref mut tcp) => tcp.flush(),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput,"no socket")),
        }
    }
}
