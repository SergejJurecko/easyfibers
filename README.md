# easyfibers

easyfibers is a closure-less couroutine library for executing asynchronous tasks as painlessly as possible. It is a small layer on top of mio and context-rs.

# Description

easyfibers allows one to write code as if it used blocking sockets and does not require putting your code in awkward closures. It will seamlessly poll and schedule fibers on read, write and accept function calls.

# Warning

Eeach fiber is executed in its own stack. These stacks are much more limited and one must be careful as to not go over limit (as it will kill your app with a SIGBUS).

Is the risk worth it? I think so. Given the ease of use compared to other coroutine/fiber libraries. I disagree with the (ab)use of clousures of other libraries. 

Heavy use of closures makes the code ugly, produces awful compile errors and makes it hard to integrate with the rest of your code.

# TODO

- [x] Fiber scheduling on read,write,accept
- [x] Child fibers
- [x] Streaming responses
- [ ] Files (using a thread pool)
- [ ] Scheduling on mpsc::Receiver<R>
- [ ] Scheduling on external poller


# Example - random http/1.1 proxy

Uses 3 types of fibers:

- TcpListener that accepts connections.

- TcpStream server that receives request and spawns a http client fiber.

- TcpStream client that creates a request to external service and streams response back to parent fiber.

Run the bottom example from one terminal:

```
cargo test -- --nocapture
```

And call it from another:

```
curl "http://127.0.0.1:10000"
```

```rust
extern crate easyfibers;
extern crate rand;

use easyfibers::*;
use mio::net::{TcpStream,TcpListener};
use std::io::{Write,Read};
use std::time::Duration;
use std::io;
use std::str;

#[derive(Clone)]
struct Param {
    chosen: Option<String>,
    hosts: Vec<String>,
}

// Return slices.
fn get_http(mut fiber: Fiber<Param,&[u8]>, p: Param) -> &[u8] {
    // Because we are too dumb to read content-length, we will use socket read timeout to finish
    // http client request.
    fiber.socket_timeout(Some(Duration::from_millis(500)));
    // We will read in 500B chunks
    let mut v = [0u8;500];

    // We want to time out so use keep-alive
    let req = format!("GET / HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nUser-Agent: test\r\n\r\n",p.chosen.unwrap());
    fiber.write(req.as_bytes()).expect("Can not write to socket");
    loop {
        // Whenever socket would normally return WouldBlock, fiber gets executed out and another
        // one takes its place in the background.
        match fiber.read(&mut v[..]) {
            Ok(sz) => {
                // Return slice to parent, directly from our stack!
                fiber.resp_chunk(&v[0..sz]);
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::TimedOut);
                break;
            }
        }
    }
    println!("Client fiber closing");
    b"client"
}

fn rand_http_proxy(mut fiber: Fiber<Param,&[u8]>, p: Param) -> &[u8] {
    fiber.socket_timeout(Some(Duration::from_millis(500)));

    // Pick a random host from our list.
    let chosen = rand::random::<usize>() % p.hosts.len();
    let p1 = Param {
        chosen: Some(p.hosts[chosen].clone()),
        hosts: Vec::new(),
    };
    println!("Returning: {}", &p.hosts[chosen]);

    // Start connection to host
    let client_sock = TcpStream::from_stream(::std::net::TcpStream::connect(p.hosts[chosen].clone() + ":80").unwrap()).unwrap();
    // Join our fiber to it. This way we can receive its output.
    fiber.join_tcp(client_sock, get_http, p1);

    // Fibers can stream response to parent. So we iterate on responses.
    // We could also create multiple children and iterate on all of them.
    while let Some(slice) = fiber.iter_children() {
        fiber.write(slice);
    }
    println!("Server socket fiber closing");
    b"server"
}

// Accept sockets in an endless loop.
fn sock_acceptor(mut fiber: Fiber<Param,&[u8]>, p: Param) -> &[u8] {
    loop {
        // If no sockets available, fiber will be scheduled out for execution until something connects. 
        match fiber.accept_tcp() {
            Ok((sock,_)) => {
                // Create a new fiber on received socket. Use rand_http_proxy function to run it.
                fiber.new_tcp(sock,rand_http_proxy, p.clone());
            }
            _ => {
                println!("Listen socket error");
                break;
            }
        }
    }
    b"listener"
}

fn main() {
    println!("Starting random http proxy. To query call: curl \"http://127.0.0.1:10000\"");
    // First time calling random requires a large stack, we must initialize it on main stack!
    rand::random::<u8>();
    let p = Param {
        chosen: None,
        hosts: vec!["www.liquiddota.com".to_string(),"www.google.com".to_string(),
            "www.sqlite.org".to_string(),"edition.cnn.com".to_string()],
    };
    // Start our fiber poller.
    // All fibers receive Param as parameter and return &[u8] as response.
    // Set this stack lower to see some SIGBUS action.
    let poll:Poller<Param,&[u8]> = Poller::new(Some(4096*3)).unwrap();
    // Start a TCP listener socket
    let listener = TcpListener::bind(&"127.0.0.1:10000".parse().unwrap()).unwrap();
    // Create a fiber from it. Listener socket will use sock_acceptor function.
    poll.new_listener(listener, sock_acceptor, p).unwrap();
    // Poll for 3 requests before exiting.
    let mut reqs_remain = 3;
    while reqs_remain > 0 {
        for res in poll.poll(Duration::from_millis(10)).drain(..) {
            println!("Finished executing {}, req_remain: {}", str::from_utf8(res).unwrap(),reqs_remain);
            reqs_remain -= 1;
        }
    }
    println!("poll out");
}

```