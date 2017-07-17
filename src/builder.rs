use std::cmp;
use std::time::Duration;

// Shamelessly taken from tokio-timer
// Copyright Tokio contributors

/// Configures and builds a `Timer`
///
/// A `Builder` is obtained by calling `wheel()`.
#[derive(Debug)]
pub struct Builder {
    tick_duration: Option<Duration>,
    num_slots: Option<usize>,
    initial_capacity: Option<usize>,
    max_capacity: Option<usize>,
    max_timeout: Option<Duration>,
    channel_capacity: Option<usize>,
    thread_name: Option<String>,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            tick_duration: None,
            num_slots: None,
            initial_capacity: None,
            max_capacity: None,
            max_timeout: None,
            channel_capacity: None,
            thread_name: None,
        }
    }

    pub fn get_tick_duration(&self) -> Duration {
        self.tick_duration.unwrap_or(Duration::from_millis(100))
    }

    /// Set the timer tick duration.
    ///
    /// See the crate docs for more detail.
    ///
    /// Defaults to 100ms.
    pub fn tick_duration(mut self, tick_duration: Duration) -> Self {
        self.tick_duration = Some(tick_duration);
        self
    }

    pub fn get_num_slots(&self) -> usize {
        // About 6 minutes at a 100 ms tick size
        self.num_slots.unwrap_or(4_096)
    }

    /// Set the number of slots in the timer wheel.
    ///
    /// The number of slots must be a power of two.
    ///
    /// See the crate docs for more detail.
    ///
    /// Defaults to 4,096.
    pub fn num_slots(mut self, num_slots: usize) -> Self {
        self.num_slots = Some(num_slots);
        self
    }

    pub fn get_initial_capacity(&self) -> usize {
        let cap = self.initial_capacity.unwrap_or(256);
        cmp::max(cap, self.get_channel_capacity())
    }

    /// Set the initial capacity of the timer
    ///
    /// The timer's timeout storage vector will be initialized to this
    /// capacity. When the capacity is reached, the storage will be doubled
    /// until `max_capacity` is reached.
    ///
    /// Default: 128
    pub fn initial_capacity(mut self, initial_capacity: usize) -> Self {
        self.initial_capacity = Some(initial_capacity);
        self
    }

    pub fn get_max_capacity(&self) -> usize {
        self.max_capacity.unwrap_or(4_194_304)
    }

    /// Set the max capacity of the timer
    ///
    /// The timer's timeout storage vector cannot get larger than this capacity
    /// setting.
    ///
    /// Default: 4,194,304
    pub fn max_capacity(mut self, max_capacity: usize) -> Self {
        self.max_capacity = Some(max_capacity);
        self
    }

    fn get_max_timeout(&self) -> Duration {
        let default = self.get_tick_duration() * self.get_num_slots() as u32;
        self.max_timeout.unwrap_or(default)
    }

    /// Set the max timeout duration that can be requested
    ///
    /// Setting the max timeout allows preventing the case of timeout collision
    /// in the hash wheel and helps guarantee optimial runtime characteristics.
    ///
    /// See the crate docs for more detail.
    ///
    /// Defaults to `num_slots * tick_duration`
    pub fn max_timeout(mut self, max_timeout: Duration) -> Self {
        self.max_timeout = Some(max_timeout);
        self
    }

    fn get_channel_capacity(&self) -> usize {
        self.channel_capacity.unwrap_or(128)
    }

    /// Set the timer communication channel capacity
    ///
    /// The timer channel is used to dispatch timeout requests to the timer
    /// thread. In theory, the timer thread is able to drain the channel at a
    /// very fast rate, however it is always possible for the channel to fill
    /// up.
    ///
    /// This setting indicates the max number of timeout requests that are able
    /// to be buffered before timeout requests are rejected.
    ///
    /// Defaults to 128
    pub fn channel_capacity(mut self, channel_capacity: usize) -> Self {
        self.channel_capacity = Some(channel_capacity);
        self
    }
}