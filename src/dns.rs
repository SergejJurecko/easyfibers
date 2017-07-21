
#[cfg(target_os = "macos")]
pub fn get_dns_servers() -> Vec<String> {
    let out = ::std::process::Command::new("scutil")
        .arg("--dns")
        .output();
    if let Ok(out) = out {
        if let Ok(s) = String::from_utf8(out.stdout) {
            return scutil_parse(s);
        }
    }
    get_google()
}

#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
pub fn get_dns_servers() -> Vec<String> {
    if let Ok(mut file) = ::std::fs::File::open("/etc/resolv.conf") {
        let mut contents = String::new();
        use std::io::Read;
        if file.read_to_string(&mut contents).is_ok() {
            return resolv_parse(contents);
        }
    }
    get_google()
}

// TODO: On windows we need to use:
// https://msdn.microsoft.com/en-us/library/windows/desktop/aa365968(v=vs.85).aspx

#[cfg(windows)]
pub fn get_dns_servers() -> Vec<String> {
    get_google()
}

fn get_google() -> Vec<String> {
    vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
}

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

fn resolv_parse(s: String) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    for line in s.lines() {
        let mut words = line.split_whitespace();
        if let Some(s) = words.next() {
            if s.starts_with("nameserver") {
                if let Some(s) = words.next() {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}