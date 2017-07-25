# easyfibers

easyfibers is a closure-less couroutine library for executing asynchronous tasks as painlessly as possible. It is a small layer on top of mio and context-rs.

# Description

easyfibers allows one to write code as if it used blocking sockets and does not require putting your code in awkward closures. It will seamlessly poll and schedule fibers on read, write and accept function calls.

# Vision

Have easyfibers stacks run protocols, have client code on main stack (using Fiber::join_main). 

This way:

- Protocol implementations can be done efficiently and in a straightforward way.

- Users of protocol clients and servers have no issues with stack size.

- Users are free to implement their code whichever way they want outside of any framework restrictions; without forcing their code into callbacks or closures.

# Warning

Eeach fiber is executed in its own stack. These stacks are much more limited and one must be careful as to not go over limit (as it will kill your app with a SIGBUS).

Is the risk worth it? I think so. Given the ease of use compared to other coroutine/fiber libraries. I disagree with the (ab)use of clousures of other libraries. 

Heavy use of closures makes the code ugly, produces awful compile errors and makes it hard to integrate with the rest of your code.

# [Documentation](https://docs.rs/easyfibers/0.6.1/easyfibers/)

# TODO

- [x] Fiber scheduling on read,write,accept
- [x] Child fibers
- [x] Streaming responses
- [ ] Files (using a thread pool)
- [x] join_main to call main stack from fiber and resume_fiber to resume it
- [x] SSL/TLS
- [x] Fiber::hibernate_for_read for keep-alive scenarios
- [ ] FiberLayer trait to compose services
- [x] Timer based fibers
- [x] Async DNS lookups


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
use std::time::{Duration,Instant};
use std::io;
use std::net::{SocketAddr,Ipv4Addr,IpAddr};
use std::str;
use native_tls::{TlsConnector};
use dns::Dns;

#[derive(Clone)]
struct Param {
    chosen: Option<String>,
    is_https: bool,
    proxy_client: bool,
    http_hosts: Vec<String>,
    https_hosts: Vec<String>,
}

#[derive(PartialEq)]
enum Resp<'a> {
    Done,
    Bytes(&'a[u8])
}

// Receive list of hosts.
// Return slices.
fn get_http(mut fiber: Fiber<Param,Resp>, p: Param) -> Option<Resp> {
    // We will read in 500B chunks
    let mut v = [0u8;2000];
    let host = p.chosen.unwrap();

    let timeout = if p.is_https {
        let connector = TlsConnector::builder().unwrap().build().unwrap();
        fiber.tcp_tls_connect(connector, host.as_str());
        // https requires longer timeout
        fiber.socket_timeout(Some(Duration::from_millis(2000)));
    } else {
        fiber.socket_timeout(Some(Duration::from_millis(2000)));
    };

    // We want to time out so use keep-alive
    let req = format!("GET / HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nUser-Agent: test\r\n\r\n",host);
    fiber.write(req.as_bytes()).expect("Can not write to socket");
    loop {
        // Whenever socket would normally return WouldBlock, fiber gets executed out and another
        // one takes its place in the background.
        match fiber.read(&mut v[..]) {
            Ok(sz) => {
                // Return slice to parent, directly from our stack!
                fiber.resp_chunk(Resp::Bytes(&v[0..sz]));
            }
            Err(e) => {
                // assert_eq!(e.kind(), io::ErrorKind::TimedOut);
                break;
            }
        }
    }
    println!("Client fiber closing {}", p.proxy_client);
    Some(Resp::Done)
}

fn rand_http_proxy(mut fiber: Fiber<Param,Resp>, p: Param) -> Option<Resp> {
    fiber.socket_timeout(Some(Duration::from_millis(500)));

    // Pick a random host from our list.
    let chosen = rand::random::<usize>() % p.http_hosts.len();
    // Pick http or https.
    let port = if rand::random::<u8>() % 2 == 0 { 80 } else { 443 };
    let p1 = if port == 443 {
        Param {
            chosen: Some(p.https_hosts[chosen].clone()),
            is_https: port == 443,
            http_hosts: Vec::new(),
            https_hosts: Vec::new(),
            proxy_client: true,
        }
    } else {
            Param {
            chosen: Some(p.http_hosts[chosen].clone()),
            is_https: port == 443,
            http_hosts: Vec::new(),
            https_hosts: Vec::new(),
            proxy_client: true,
        }
    };
    let addr = if let &Some(ref ch) = &p1.chosen {
            ch.clone()
    } else {
        panic!("")
    };
    let a = Instant::now();
    fiber.join_resolve_connect(addr.as_str(), SocketType::Tcp, port, Duration::from_millis(3000), get_http, p1).unwrap();
    println!("Returning: {}{}", if port == 443 { "https://" } else { "http://" },  addr);

    // Fibers can stream response to parent. So we iterate on responses.
    // We could also create multiple children and iterate on all of them.
    while let Some(resp) = fiber.get_child() {
        if let Resp::Bytes(slice) = resp {
            println!("Server got {}", slice.len());
            fiber.write(slice);
        }
    }
    println!("Server socket fiber closing");
    // return empty slice, so main stack knows a server connection has closed
    None
}

// Accept sockets in an endless loop.
fn sock_acceptor(mut fiber: Fiber<Param,Resp>, p: Param) -> Option<Resp> {
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
    None
}

fn main() {
    println!("Starting random http proxy. To query call: curl \"http://127.0.0.1:10000\"");
    // First time calling random requires a large stack, we must initialize it on main stack!
    rand::random::<u8>();
    let p = Param {
        chosen: None,
        is_https: false,
        proxy_client: false,
        http_hosts: vec!["www.liquiddota.com".to_string(),"www.google.com".to_string(),
            "www.sqlite.org".to_string(),"edition.cnn.com".to_string()],
        https_hosts: vec!["www.reddit.com".to_string(), "www.google.com".to_string(),
            "arstechnica.com".to_string(), "news.ycombinator.com".to_string()],
    };
    // Start our fiber poller.
    // Set this stack lower to see some SIGBUS action.
    let poll:Poller<Param,Resp> = Poller::new(Some(4096*10)).unwrap();
    // Start a TCP listener socket
    let listener = TcpListener::bind(&"127.0.0.1:10000".parse().unwrap()).unwrap();
    // Create a fiber from it. Listener socket will use sock_acceptor function.
    poll.new_listener(listener, sock_acceptor, p).unwrap();
    // Poll for 3 requests before exiting.
    let mut reqs_remain = 20;
    for i in 0..reqs_remain {
        let p = Param {
            chosen: Some("127.0.0.1:10000".to_string()),
            is_https: false,
            proxy_client: false,
            http_hosts: Vec::new(),
            https_hosts: Vec::new(),
        };
        let addr = IpAddr::V4(Ipv4Addr::new(127,0,0,1));
        let client_sock = TcpStream::connect(&SocketAddr::new(addr, 10000)).unwrap();
        poll.new_tcp(client_sock, get_http, p);
    }
    while reqs_remain > 0 {
        if poll.poll(Duration::from_millis(10)) {
            while let Some(r) = poll.get_response() {
                if Resp::Done == r {
                    reqs_remain -= 1;
                    println!("Finished executing, req_remain: {}", reqs_remain);
                } else if let Resp::Bytes(slice) = r {
                    println!("Main stack got {} bytes", slice.len());
                }
            }

            // we arent using 
            while let Some(f) = poll.get_fiber() {
            }
        }
    }
    println!("poll out");
}

```