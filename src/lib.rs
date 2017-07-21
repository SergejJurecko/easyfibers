
//! easyfibers is a closure-less couroutine library for executing asynchronous tasks as painlessly as possible. 
//!
//! Code example at: https://github.com/SergejJurecko/easyfibers
extern crate context;
extern crate mio;
extern crate slab;
extern crate native_tls;
extern crate rand;
extern crate byteorder;
#[macro_use(quick_error)]
extern crate quick_error;
#[cfg(test)]
#[macro_use]
extern crate matches;

mod fiber;
mod runner;
#[allow(dead_code)]
mod wheel;
#[allow(dead_code)]
mod builder;
#[allow(dead_code)]
mod dns_parser;

pub use runner::Poller;
pub use fiber::{Fiber, FiberFn, FiberRef};

#[cfg(test)]
mod tests {
    extern crate rand;
    use super::*;
    use mio::net::{TcpStream,TcpListener};
    use std::io::{Write,Read};
    use std::time::{Duration,Instant};
    use std::io;
    use std::str;
    use native_tls::{TlsConnector};


    fn scutil_parse(s: String) -> Vec<String> {
        let mut out = Vec::with_capacity(2);
        for line in s.lines() {
            let mut words = line.split_whitespace();
            if let Some(s) = words.next() {
                if s.starts_with("nameserver[") {
                    if let Some(s) = words.next() {
                        if s == ":" {
                            if let Some(s) = words.next() {
                                out.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn get_dns_servers() -> Vec<String> {
        let out = ::std::process::Command::new("scutil")
            .arg("--dns")
            .output();
        if let Ok(out) = out {
            if let Ok(s) = String::from_utf8(out.stdout) {
                return scutil_parse(s);
            }
        }
        return vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    }

    use dns_parser;
    use dns_parser::{Packet,RRData};
    #[test]
    fn dnsq() {
        let mut buf_send = [0; 512];
        let nsend = {
            let mut builder = dns_parser::Builder::new(&mut buf_send[..]);
            builder.start(rand::random::<u16>(), true);
            builder.add_question("www.liquiddota.com", 
                dns_parser::QueryType::A,
                dns_parser::QueryClass::IN);
            builder.finish()
        };
        let srvs = get_dns_servers();
        println!("Sending {} {}", srvs[0], nsend);

        let mut socket = ::std::net::UdpSocket::bind("0.0.0.0:0").expect("sock fails");
        let ip = srvs[0].parse().unwrap();
        let sockaddr = ::std::net::SocketAddrV4::new(ip, 53);

        socket.send_to(&buf_send[..nsend], &sockaddr);
        let mut buf2 = [0; 1000];
        let (amt, src) = socket.recv_from(&mut buf2).expect("dns recv failed");
        println!("Received {}", amt);
        let packet = Packet::parse(&buf2[..amt]).unwrap();
        for a in packet.answers {
            match a.data {
                RRData::A(ip) => {
                    print!("IP: {} ", ip);
                }
                _ => {
                    // Bad value. Log it?
                }
            }
        }
    }

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

        println!("get_http {}", host);

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
        //let port = if rand::random::<u8>() % 2 == 0 { 80 } else { 443 };
        let port = 80;
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
             ch.clone() + ":" + port.to_string().as_str()
        } else {
            panic!("")
        };
        let a = Instant::now();
        // Start connection to host
        let client_sock = TcpStream::from_stream(::std::net::TcpStream::connect(addr.clone()).unwrap()).unwrap();
        // Join our fiber to it. This way we can receive its output.
        fiber.join_tcp(client_sock, get_http, p1);
        println!("Returning ({}ms): {}{}", 
            a.elapsed().subsec_nanos() / 1000000, 
            if port == 443 { "https://" } else { "http://" },  addr);

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

    #[test]
    fn http_proxy() {
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
            let client_sock = TcpStream::from_stream(::std::net::TcpStream::connect("127.0.0.1:10000").unwrap()).unwrap();
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
}
