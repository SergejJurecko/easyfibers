use std::io;
use std::ptr;
use std::os::raw::{c_void};
use mio::{Events, Poll};
use context::stack::ProtectedFixedSizeStack;
use wheel::Wheel;
use std::collections::VecDeque;
use dns::Dns;
use builder::Builder;
use std::time::{Duration,Instant};
use std::mem::swap;
use wheel::Token as WToken;
use std::net::{IpAddr};

/// Used to poll on sockets and timers. There can only be 1 poller per thread.
pub struct Poller;

impl Poller {
    /// Start poller. This function does not block.
    /// stack_size will be rounded up to next power of two.
    /// If not set default value of context-rs will be used.
    pub fn new(stack_size: Option<usize>) -> io::Result<Poller> {
        if unsafe { get_poller() } != ptr::null_mut() {
            return Err(io::Error::new(io::ErrorKind::NotConnected,"poller exists"));
        }
        let r: *mut PollerInt = Box::into_raw(Box::new(PollerInt::new(stack_size)));
        unsafe { set_poller(r as *mut c_void); }
        Ok(Poller {})
    }
    /// Execute socket poll, timers and check dns lookups.
    /// Will block for a maximum of dur or not at all if a socket is signalled.
    /// Returns true if something changed and runners should be executed.
    pub fn poll(&self, dur: Duration) -> bool {
        poller().poll(dur)
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(get_poller());
            set_poller(ptr::null_mut());
        }
    }
}

pub(crate) static TICK_MS: u64 = 250;

pub(crate) struct PollerInt {
    events: Events,
    poll: Poll,
    free_stacks: VecDeque<ProtectedFixedSizeStack>,
    wheel: Wheel,
    stack_size: Option<usize>,
    dns: Dns,
    toexec_fibers: [VecDeque<usize>;20],
    timedout_fibers: [VecDeque<usize>;20],
    // for now only used on macOS
    lookup_results: [VecDeque<(usize,usize,IpAddr)>;20],
}

impl PollerInt {
    fn new(stack_size: Option<usize>) -> PollerInt {
        PollerInt {
            toexec_fibers: [VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4)],
            timedout_fibers: [VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4),VecDeque::with_capacity(4),
                VecDeque::with_capacity(4),VecDeque::with_capacity(4)],
            lookup_results: [VecDeque::new(),VecDeque::new(),VecDeque::new(),VecDeque::new(),
                VecDeque::new(),VecDeque::new(),VecDeque::new(),VecDeque::new(),
                VecDeque::new(),VecDeque::new(),VecDeque::new(),VecDeque::new(),
                VecDeque::new(),VecDeque::new(),VecDeque::new(),VecDeque::new(),
                VecDeque::new(),VecDeque::new(),VecDeque::new(),VecDeque::new()],
            stack_size,
            poll: Poll::new().expect("unable to start poller"),
            events: Events::with_capacity(1024),
            free_stacks: VecDeque::with_capacity(4),
            wheel: Wheel::new(&Builder::new().tick_duration(Duration::from_millis(TICK_MS))),
            dns: Dns::new(),
        }
    }

    pub(crate) fn poller(&mut self) -> &Poll {
        &self.poll
    }

    pub(crate) fn dns(&mut self) -> &Dns {
        &self.dns
    }

    fn split_id(pos: usize) -> (usize,usize) {
        let runner = (pos >> 40) & 255;
        let pos = pos & u32::max_value() as usize;
        (runner,pos)
    }

    fn poll(&mut self, dur: Duration) -> bool {
        let mut smth = false;
        if let Err(_) = self.poll.poll(&mut self.events, Some(dur)) {
            return smth;
        }

        if self.events.len() > 0 {
            smth = true;
            let mut events = Events::with_capacity(0);
            swap(&mut self.events, &mut events);
            for ev in events.into_iter() {
                let (runner, pos) = Self::split_id(ev.token().0);
                self.toexec_fibers[runner].push_back(pos);
            }
            swap(&mut self.events, &mut events);
        }

        let now = Instant::now();
        while let Some(id) = self.wheel.poll(now) {
            smth = true;
            let (runner, pos) = Self::split_id(id);
            self.timedout_fibers[runner].push_back(pos);
        }

        while let Some((id,gen,addr)) = self.dns.check_result() {
            smth = true;
            println!("dns got result");
            let (runner, pos) = Self::split_id(id);
            self.lookup_results[runner].push_back((pos,gen,addr));
        }
        smth
    }

    pub(crate) fn timer_reg(&mut self, id: usize, dur: Option<Duration>) -> Option<WToken> {
        let dur = if let Some(d) = dur {
            d
        } else {
            Duration::from_millis(TICK_MS)
        };
        if let Some(tk) = self.wheel.reserve() {
            self.wheel.set_timeout(tk, Instant::now() + dur, id);
            return Some(tk);
        }
        None
    }

    pub(crate) fn timer_cancel(&mut self, tk: WToken) {
        self.wheel.cancel(tk);
    }

    pub(crate) fn dns_check(&mut self, pos: usize) -> Option<(usize,usize,IpAddr)> {
        self.lookup_results[pos].pop_front()
    }

    pub(crate) fn toexec_push(&mut self, runner: usize, fiber: usize) {
        self.toexec_fibers[runner as usize].push_back(fiber);
    }

    pub(crate) fn toexec_pop(&mut self, runner: usize) -> Option<usize> {
        self.toexec_fibers[runner as usize].pop_front()
    }

    pub(crate) fn stack_pop(&mut self) -> ProtectedFixedSizeStack {
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

    pub(crate) fn stack_push(&mut self, stack: ProtectedFixedSizeStack) {
        self.free_stacks.push_front(stack)
    }

    pub(crate) fn timedout_pop(&mut self, runner: usize) -> Option<usize> {
        self.timedout_fibers[runner].pop_front()
    }
}

pub(crate) fn poller<'a>() -> &'a mut PollerInt {
    unsafe {
        let rc = get_poller();
        ::std::mem::transmute::<*mut c_void, &mut PollerInt>(rc)
    }
}

extern "C" {
    pub(crate) fn get_poller() -> *mut c_void;
    fn set_poller(r: *mut c_void);
}
