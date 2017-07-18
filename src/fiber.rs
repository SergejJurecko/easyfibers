use std::io;
use runner::runner;
use std::io::{Read,Write};
use std::net::SocketAddr;
use context::{Context, Transfer};
use context::stack::ProtectedFixedSizeStack;
use mio::net::{TcpStream,UdpSocket,TcpListener};
use mio::event::Evented;
use mio::{Ready,Poll,PollOpt,Token};
use std::marker::PhantomData;
use wheel::Token as WToken;
use std::time::Duration;
use std::collections::VecDeque;

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

    /// Start fiber on TCP socket.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(self.id), func, FiberSock::Tcp(tcp), param, None).map(|_|{()})
    }

    /// Start fiber on TCP listener.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_listener(&self, tcp: TcpListener, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(self.id), func, FiberSock::Listener(tcp), param, None).map(|_|{()})
    }

    /// Start fiber on UDP socket.
    ///
    /// This function does not block and fiber gets executed on next poll(). There is no relationship
    /// between calling and created fiber.
    pub fn new_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        runner().register(Some(self.id), func, FiberSock::Udp(udp), param, None).map(|_|{()})
    }

    /// Get result of child. If fiber has multiple children it will return first available result.
    /// Will block current fiber if nothing available immediately.
    ///
    /// If none is returned all children have finished executing.
    pub fn get_child(&self) -> Option<R> {
        runner::<P,R>().child_iter(self.id)
    }

    /// Start a child fiber with tcp socket.
    ///
    /// This function does not block current fiber.
    pub fn join_tcp(&self, tcp: TcpStream, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        match runner::<P,R>().register(Some(self.id), func, FiberSock::Tcp(tcp), param, Some(self.id)) {
            Ok(Some(_)) => {
                Ok(())
            }
            Ok(None) => {
                Err(io::Error::new(io::ErrorKind::Other, "can not create child"))
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    /// Start a child fiber with an udp socket.
    ///
    /// This function does not block current fiber.
    pub fn join_udp(&self, udp: UdpSocket, func: FiberFn<P,R>, param: P) -> io::Result<()> {
        match runner::<P,R>().register(Some(self.id), func, FiberSock::Udp(udp), param, Some(self.id)) {
            Ok(Some(_)) => {
                Ok(())
            }
            Ok(None) => {
                Err(io::Error::new(io::ErrorKind::Other, "can not create child"))
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    /// Call main stack.
    ///
    /// This function blocks until main stack produces response.
    pub fn join_main(&self, param: P) -> R {
        runner::<P,R>().join_main(self.id)
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

#[derive(PartialEq)]
pub(crate) enum FiberState {
    Closed,
    Stacked,
    Unstacked,
}

pub(crate) struct FiberInt<P,R> {
    pub(crate) ready: Ready,
    // pub(crate) blocking_on: Ready,
    pub(crate) param: Option<P>,
    pub(crate) state: FiberState,
    pub(crate) me: usize,
    pub(crate) func: FiberFn<P,R>,
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
    pub(crate) timed_out: bool,
}

impl<P,R> FiberInt<P,R> {
    pub(crate) fn evented(&self) -> &Evented {
        self.sock.evented()
    }

    pub(crate) fn register(&mut self, poll: &Poll, rdy: Ready) -> io::Result<()> {
        let res = if self.ready.is_empty() {
            poll.register(self.evented(), Token(self.me), rdy, PollOpt::edge())
        } else {
            poll.reregister(self.evented(), Token(self.me), rdy, PollOpt::edge())
        };
        self.ready = rdy;
        res
    }
}

pub(crate) enum FiberSock {
    Tcp(TcpStream),
    Udp(UdpSocket),
    Listener(TcpListener),
    Empty,
}

impl FiberSock {
    pub fn evented(&self) -> &Evented {
        match self {
            &FiberSock::Tcp(ref tcp) => tcp,
            &FiberSock::Udp(ref udp) => udp,
            &FiberSock::Listener(ref tcp) => tcp,
            _ => panic!("Evented on an empty fiber"),
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
            FiberSock::Listener(ref tcp) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not read on listen socket")),
            _ => panic!("Read on an empty fiber"),
        }
    }
}

impl Write for FiberSock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match *self {
            FiberSock::Tcp(ref mut tcp) => tcp.write(buf),
            FiberSock::Udp(ref mut udp) => udp.send(buf),
            FiberSock::Listener(ref tcp) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not write on listen socket")),
            _ => panic!("Write on an empty fiber"),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match *self {
            FiberSock::Tcp(ref mut tcp) => tcp.flush(),
            FiberSock::Udp(ref mut udp) => Ok(()),
            FiberSock::Listener(ref tcp) => 
                Err(io::Error::new(io::ErrorKind::InvalidInput,"can not flush on listen socket")),
            _ => panic!("Flush on an empty fiber"),
        }
    }
}
