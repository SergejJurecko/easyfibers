
//! easyfibers is a closure-less couroutine library for executing asynchronous tasks as painlessly as possible. 
//!
//! Code example at: https://github.com/SergejJurecko/easyfibers
extern crate context;
extern crate mio;
extern crate slab;
extern crate native_tls;

#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_imports)]
mod fiber;
#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_imports)]
mod runner;
#[allow(dead_code)]
mod wheel;
#[allow(dead_code)]
mod builder;

pub use runner::Poller;
pub use fiber::{Fiber, FiberFn, FiberRef};

#[cfg(test)]
mod tests {
    extern crate rand;
    use super::*;
    use mio::net::{TcpStream,TcpListener};
    use std::io::{Write,Read};
    use std::time::Duration;
    use std::io;
    use std::str;
    use native_tls::{TlsConnector};

    #[derive(Clone)]
    struct Param {
        chosen: Option<String>,
        is_https: bool,
        http_hosts: Vec<String>,
        https_hosts: Vec<String>,
    }

    // Receive list of hosts.
    // Return slices.
    fn get_http(mut fiber: Fiber<Param,&[u8]>, p: Param) -> Option<&[u8]> {
        // We will read in 500B chunks
        let mut v = [0u8;500];
        let host = p.chosen.unwrap();

        let timeout = if p.is_https {
            let connector = TlsConnector::builder().unwrap().build().unwrap();
            fiber.tcp_tls_connect(connector, host.as_str());
            // https requires longer timeout
            fiber.socket_timeout(Some(Duration::from_millis(1000)));
        } else {
            fiber.socket_timeout(Some(Duration::from_millis(500)));
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
                    fiber.resp_chunk(&v[0..sz]);
                }
                Err(e) => {
                    assert_eq!(e.kind(), io::ErrorKind::TimedOut);
                    break;
                }
            }
        }
        println!("Client fiber closing");
        None
    }

    fn rand_http_proxy(mut fiber: Fiber<Param,&[u8]>, p: Param) -> Option<&[u8]> {
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
            }
        } else {
             Param {
                chosen: Some(p.http_hosts[chosen].clone()),
                is_https: port == 443,
                http_hosts: Vec::new(),
                https_hosts: Vec::new(),
            }
        };
        let addr = if let &Some(ref ch) = &p1.chosen {
             ch.clone() + ":" + port.to_string().as_str()
        } else {
            panic!("")
        };
        println!("Returning: {}{}", if port == 443 { "https://" } else { "http://" },  addr);
        // Start connection to host
        let client_sock = TcpStream::from_stream(::std::net::TcpStream::connect(addr).unwrap()).unwrap();
        // Join our fiber to it. This way we can receive its output.
        fiber.join_tcp(client_sock, get_http, p1);

        // Fibers can stream response to parent. So we iterate on responses.
        // We could also create multiple children and iterate on all of them.
        while let Some(slice) = fiber.get_child() {
            fiber.write(slice);
        }
        println!("Server socket fiber closing");
        // return empty slice, so main stack knows a server connection has closed
        Some(&[])
    }

    // Accept sockets in an endless loop.
    fn sock_acceptor(mut fiber: Fiber<Param,&[u8]>, p: Param) -> Option<&[u8]> {
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

    #[test]
    fn it_works() {
        println!("Starting random http proxy. To query call: curl \"http://127.0.0.1:10000\"");
        // First time calling random requires a large stack, we must initialize it on main stack!
        rand::random::<u8>();
        let p = Param {
            chosen: None,
            is_https: false,
            http_hosts: vec!["www.liquiddota.com".to_string(),"www.google.com".to_string(),
                "www.sqlite.org".to_string(),"edition.cnn.com".to_string()],
            https_hosts: vec!["www.reddit.com".to_string(), "www.google.com".to_string(),
                "arstechnica.com".to_string(), "news.ycombinator.com".to_string()],
        };
        // Start our fiber poller.
        // Set this stack lower to see some SIGBUS action.
        let poll:Poller<Param,&[u8]> = Poller::new(Some(4096*10)).unwrap();
        // Start a TCP listener socket
        let listener = TcpListener::bind(&"127.0.0.1:10000".parse().unwrap()).unwrap();
        // Create a fiber from it. Listener socket will use sock_acceptor function.
        poll.new_listener(listener, sock_acceptor, p).unwrap();
        // Poll for 3 requests before exiting.
        let mut reqs_remain = 3;
        while reqs_remain > 0 {
            if poll.poll(Duration::from_millis(10)) {
                while let Some(r) = poll.get_response() {
                    println!("Finished executing, req_remain: {}", reqs_remain);
                    reqs_remain -= 1;
                }

                // we arent using 
                while let Some(f) = poll.get_fiber() {
                }
            }
        }
        println!("poll out");
    }
}
