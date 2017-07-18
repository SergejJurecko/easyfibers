# 0.6.0 (Jul 18, 2017)

* Changed api: Poller::poll(_) now returns bool to signal you should call poll.get_response() and poll.get_get_fiber()
* Changed api: Rename iter_children() to get_child() as we do not return iterator.
* Added: Fiber::join_main to call main stack and FiberRef::resume_fiber to return result from main stack.

# 0.5.0 (Jul 17, 2017)

* Initial Release