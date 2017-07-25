# 0.6.1 (Jul 25, 2017)

* SSL/TLS support using Fiber::tcp_tls_connect and Fiber::tcp_tls_accept
* Fiber::hibernate_for_read function for keep-alive scenarios. Once socket has data to read or is closed, fiber function will be called on it from beginning. While in hibernation stack is reused for other fibers.
* Poller::new_timer and Poller::new_fiber_timer for running time based tasks or fibers.
* new_resolve_connect, join_resolve_connect to asynchronously lookup DNS and connect to host.

# 0.6.0 (Jul 18, 2017)

* Changed api: Poller::poll(_) now returns bool to signal you should call poll.get_response() and poll.get_get_fiber()
* Changed api: Rename iter_children() to get_child() as we do not return iterator.
* Changed api: FiberFn not returns Option<R>.
* Added: Fiber::join_main to call main stack and FiberRef::resume_fiber to return result from main stack.

# 0.5.0 (Jul 17, 2017)

* Initial Release