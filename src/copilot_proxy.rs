//! Per-run authenticated TLS CONNECT broker. Payloads and auth headers are never logged.
use crate::Result;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use std::{
    io::{Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const BYTE_LIMIT: usize = 4 * 1024 * 1024;
const CONNECTION_LIMIT: usize = 24;
#[derive(Clone, Default, Debug, Serialize)]
pub struct NetworkReport {
    pub approved_tunnels: usize,
    pub denied_connections: usize,
    pub sent_bytes: usize,
    pub received_bytes: usize,
}
struct Shared {
    /// Exact approved TLS authorities for this session's deployment.
    authorities: Vec<String>,
    /// GitHub Enterprise Server opt-in: RFC 1918/CGNAT/ULA destinations.
    private_network: bool,
    stop: AtomicBool,
    deadline: Instant,
    nonce: String,
    approved: AtomicUsize,
    denied: AtomicUsize,
    sent: AtomicUsize,
    received: AtomicUsize,
    sockets: Mutex<Vec<TcpStream>>,
}
pub struct Broker {
    pub port: u16,
    authorization: String,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl Broker {
    pub fn start(seconds: u16, authorities: Vec<String>, private_network: bool) -> Result<Self> {
        if authorities.is_empty() || authorities.len() > 8 {
            return Err("Copilot broker needs 1..8 exact approved authorities".into());
        }
        let mut random = [0u8; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "cannot create Copilot broker nonce")?;
        let nonce = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|_| "cannot bind Copilot egress broker")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "cannot bound Copilot broker accepts")?;
        let port = listener
            .local_addr()
            .map_err(|_| "cannot identify Copilot broker")?
            .port();
        let shared = Arc::new(Shared {
            authorities,
            private_network,
            stop: AtomicBool::new(false),
            deadline: Instant::now() + Duration::from_secs(u64::from(seconds)),
            nonce: nonce.clone(),
            approved: AtomicUsize::new(0),
            denied: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            received: AtomicUsize::new(0),
            sockets: Mutex::new(Vec::new()),
        });
        let state = shared.clone();
        let worker = thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = vec![];
            let mut total = 0;
            while !expired(&state) {
                workers.retain(|worker| !worker.is_finished());
                match listener.accept() {
                    Ok((stream, _)) => {
                        total += 1;
                        if total > CONNECTION_LIMIT || workers.len() >= 4 {
                            state.denied.fetch_add(1, Ordering::Relaxed);
                            let _ = stream.shutdown(Shutdown::Both);
                            continue;
                        }
                        let state = state.clone();
                        workers.push(thread::spawn(move || {
                            if tunnel(stream, &state).is_err() {
                                state.denied.fetch_add(1, Ordering::Relaxed);
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
            close_sockets(&state);
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            port,
            authorization: format!("http://kborg:{nonce}@127.0.0.1:{port}"),
            shared,
            worker: Some(worker),
        })
    }
    pub fn url(&self) -> &str {
        &self.authorization
    }
    pub fn report(&self) -> NetworkReport {
        NetworkReport {
            approved_tunnels: self.shared.approved.load(Ordering::Relaxed),
            denied_connections: self.shared.denied.load(Ordering::Relaxed),
            sent_bytes: self.shared.sent.load(Ordering::Relaxed),
            received_bytes: self.shared.received.load(Ordering::Relaxed),
        }
    }
    #[cfg(test)]
    fn authorization(&self) -> String {
        use base64::Engine;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("kborg:{}", self.shared.nonce))
        )
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        close_sockets(&self.shared);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn expired(state: &Shared) -> bool {
    state.stop.load(Ordering::Relaxed) || Instant::now() >= state.deadline
}
fn close_sockets(state: &Shared) {
    if let Ok(sockets) = state.sockets.lock() {
        for socket in sockets.iter() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}
fn track(stream: &TcpStream, state: &Shared) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|_| "cannot bound broker read")?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .map_err(|_| "cannot bound broker write")?;
    state
        .sockets
        .lock()
        .map_err(|_| "broker tracking unavailable")?
        .push(
            stream
                .try_clone()
                .map_err(|_| "cannot supervise broker socket")?,
        );
    Ok(())
}
fn read_until(
    stream: &mut TcpStream,
    state: &Shared,
    bytes: &mut Vec<u8>,
    target: usize,
    deadline: Instant,
) -> Result<()> {
    while bytes.len() < target && Instant::now() < deadline && !expired(state) {
        let mut buffer = [0u8; 4096];
        let count = (target - bytes.len()).min(buffer.len());
        match stream.read(&mut buffer[..count]) {
            Ok(0) => return Err("incomplete broker request".into()),
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Err("broker input failed".into()),
        }
    }
    if bytes.len() != target {
        return Err("broker request deadline exceeded".into());
    }
    Ok(())
}
fn authority(header: &[u8], nonce: &str, approved: &[String]) -> Result<String> {
    use base64::Engine;
    let text = std::str::from_utf8(header).map_err(|_| "invalid proxy header")?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or("missing proxy request")?;
    let words = first.split(' ').collect::<Vec<_>>();
    if words.len() != 3 || words[0] != "CONNECT" || words[2] != "HTTP/1.1" {
        return Err("only TLS CONNECT is allowed".into());
    }
    let host = words[1]
        .strip_suffix(":443")
        .filter(|host| approved.iter().any(|approved| approved == host))
        .ok_or("unapproved Copilot egress authority")?;
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("kborg:{nonce}"))
    );
    let mut authenticated = false;
    let mut hosts = 0;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid proxy header field")?;
        if name.eq_ignore_ascii_case("proxy-authorization") {
            if authenticated || value.trim() != expected {
                return Err("broker session authentication failed".into());
            }
            authenticated = true;
        }
        if name.eq_ignore_ascii_case("host") {
            hosts += 1;
            if value.trim() != words[1] {
                return Err("proxy Host disagrees with CONNECT authority".into());
            }
        }
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err("proxy CONNECT bodies are forbidden".into());
        }
    }
    if !authenticated || hosts > 1 {
        return Err("broker session authentication required".into());
    }
    Ok(host.into())
}
/// Inspect the actual TLS ClientHello; CONNECT alone cannot establish SNI binding.
fn sni(hello: &[u8]) -> Result<&str> {
    if hello.len() < 4 {
        return Err("truncated TLS ClientHello".into());
    }
    let get = |at: usize, len: usize| hello.get(at..at + len).ok_or("truncated TLS ClientHello");
    if get(0, 1)? != [1] {
        return Err("TLS ClientHello required".into());
    }
    let length =
        (usize::from(hello[1]) << 16) | (usize::from(hello[2]) << 8) | usize::from(hello[3]);
    if length + 4 != hello.len() || hello.len() < 39 {
        return Err("invalid TLS ClientHello size".into());
    }
    let mut at = 38;
    let session_len = usize::from(get(at, 1)?[0]);
    at += 1 + session_len;
    let cipher = get(at, 2)?;
    at += 2 + usize::from(u16::from_be_bytes([cipher[0], cipher[1]]));
    let compression = usize::from(get(at, 1)?[0]);
    at += 1 + compression;
    let extensions = get(at, 2)?;
    at += 2;
    let end = at + usize::from(u16::from_be_bytes([extensions[0], extensions[1]]));
    if end != hello.len() {
        return Err("invalid TLS extension length".into());
    }
    let mut found = None;
    while at < end {
        let extension = get(at, 4)?;
        at += 4;
        let kind = u16::from_be_bytes([extension[0], extension[1]]);
        let length = usize::from(u16::from_be_bytes([extension[2], extension[3]]));
        let value = get(at, length)?;
        at += length;
        if kind == 0xfe0d {
            return Err("encrypted ClientHello is unsupported by hostname-bound broker".into());
        }
        if kind == 0 {
            if found.is_some() || value.len() < 5 {
                return Err("invalid TLS server name extension".into());
            }
            let list = usize::from(u16::from_be_bytes([value[0], value[1]]));
            let name_len = usize::from(u16::from_be_bytes([value[3], value[4]]));
            if value[2] != 0 || list + 2 != value.len() || name_len + 5 != value.len() {
                return Err("exactly one TLS DNS server name is required".into());
            }
            found = Some(std::str::from_utf8(&value[5..]).map_err(|_| "invalid TLS hostname")?);
        }
    }
    found.ok_or_else(|| "TLS hostname is absent".into())
}
fn client_hello(stream: &mut TcpStream, state: &Shared) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut wire = Vec::new();
    let mut handshake = Vec::new();
    loop {
        let at = wire.len();
        read_until(stream, state, &mut wire, at + 5, deadline)?;
        if wire[at] != 22 || wire[at + 1] != 3 {
            return Err("only a TLS handshake is allowed through the broker".into());
        }
        let size = usize::from(u16::from_be_bytes([wire[at + 3], wire[at + 4]]));
        if size == 0 || size > 16384 || wire.len() + size > 65536 {
            return Err("TLS ClientHello exceeds broker bound".into());
        }
        read_until(stream, state, &mut wire, at + 5 + size, deadline)?;
        handshake.extend_from_slice(&wire[at + 5..]);
        if handshake.len() >= 4 {
            let length = (usize::from(handshake[1]) << 16)
                | (usize::from(handshake[2]) << 8)
                | usize::from(handshake[3]);
            if length + 4 == handshake.len() {
                return Ok(wire);
            }
            if length + 4 < handshake.len() {
                return Err("unexpected TLS data before ClientHello verification".into());
            }
        }
    }
}
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 192 && b == 0 && (c == 0 || c == 2))
                && !(a == 192 && b == 88 && c == 99)
        }
        IpAddr::V6(ip) => {
            // Only ordinary global unicast. Reject mapped/transition/special-use
            // prefixes instead of treating any non-private IPv6 as public.
            let [a, b, _, _, _, _, _, _] = ip.segments();
            (0x2000..=0x3fff).contains(&a)
                && a != 0x2002
                && !(a == 0x2001 && (b < 0x0200 || b == 0x0db8))
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

/// Internal enterprise ranges a GitHub Enterprise Server may use. Loopback,
/// link-local (including cloud metadata), multicast and unspecified addresses
/// are never reachable, even with the opt-in.
fn enterprise_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_private() || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(ip) => (ip.segments()[0] & 0xfe00) == 0xfc00,
    }
}
fn resolve(host: &str, state: &Shared) -> Result<Vec<SocketAddr>> {
    // The system resolver is a supervised fixed subprocess, not an unbounded
    // getaddrinfo thread that could keep a dropped UI runtime alive forever.
    let mut child = Command::new("/usr/bin/dscacheutil")
        .args(["-q", "host", "-a", "name", host])
        .env_clear()
        .env("PATH", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start bounded Copilot DNS resolver")?;
    let mut output = child.stdout.take().ok_or("DNS output unavailable")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .by_ref()
            .take(4097)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline && !expired(state) => {
                thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("Copilot DNS deadline exceeded".into());
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "DNS capture failed")?
        .map_err(|_| "DNS read failed")?;
    if !status.success() || bytes.len() > 4096 {
        return Err("Copilot DNS resolution failed or exceeded bound".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid DNS response")?;
    let addresses = text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("ip_address: ")
                .or_else(|| line.strip_prefix("ipv6_address: "))
        })
        .filter_map(|ip| ip.parse::<IpAddr>().ok())
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses
            .iter()
            .any(|ip| !public(*ip) && !(state.private_network && enterprise_private(*ip)))
    {
        return Err("Copilot DNS returned absent/private/oversized addresses".into());
    }
    Ok(addresses
        .into_iter()
        .map(|ip| SocketAddr::new(ip, 443))
        .collect())
}
fn tunnel(mut client: TcpStream, state: &Shared) -> Result<()> {
    track(&client, state)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err("broker header exceeds8KiB".into());
        }
        let target = header.len() + 1;
        read_until(&mut client, state, &mut header, target, deadline)?;
    }
    let host = match authority(&header, &state.nonce, &state.authorities) {
        Ok(host) => host,
        Err(error) => {
            let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            return Err(error);
        }
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .map_err(|_| "broker acknowledgment failed")?;
    let wire = client_hello(&mut client, state)?;
    let mut handshake = Vec::new();
    let mut at = 0;
    while at < wire.len() {
        let size = usize::from(u16::from_be_bytes([wire[at + 3], wire[at + 4]]));
        handshake.extend_from_slice(&wire[at + 5..at + 5 + size]);
        at += 5 + size;
    }
    if sni(&handshake)? != host {
        return Err("TLS hostname differs from approved CONNECT authority".into());
    }
    let mut server = None;
    for address in resolve(&host, state)?.into_iter().take(3) {
        if expired(state) {
            break;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
            server = Some(stream);
            break;
        }
    }
    let mut server = server.ok_or("approved Copilot host unavailable")?;
    track(&server, state)?;
    server
        .write_all(&wire)
        .map_err(|_| "Copilot TLS handshake forwarding failed")?;
    state.approved.fetch_add(1, Ordering::Relaxed);
    state.sent.fetch_add(wire.len(), Ordering::Relaxed);
    let mut up_client = client
        .try_clone()
        .map_err(|_| "cannot supervise outbound stream")?;
    let mut up_server = server
        .try_clone()
        .map_err(|_| "cannot supervise outbound stream")?;
    thread::scope(|scope| {
        scope.spawn(|| {
            let _ = copy(&mut up_client, &mut up_server, state, &state.sent);
            let _ = up_server.shutdown(Shutdown::Write);
        });
        let _ = copy(&mut server, &mut client, state, &state.received);
        let _ = client.shutdown(Shutdown::Both);
        let _ = server.shutdown(Shutdown::Both);
    });
    Ok(())
}
fn copy(
    input: &mut TcpStream,
    output: &mut TcpStream,
    state: &Shared,
    count: &AtomicUsize,
) -> Result<()> {
    let mut buffer = [0u8; 8192];
    while !expired(state) {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(size) => {
                if count.fetch_add(size, Ordering::Relaxed) + size > BYTE_LIMIT {
                    state.stop.store(true, Ordering::Relaxed);
                    close_sockets(state);
                    return Err("Copilot network byte limit exceeded".into());
                }
                output
                    .write_all(&buffer[..size])
                    .map_err(|_| "Copilot forwarding failed")?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Err("Copilot connection ended".into()),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authority_requires_child_nonce_exact_public_host_and_tls_port() {
        // Generated per run so the test never embeds a fixed credential.
        let nonce = format!(
            "test-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        );
        let nonce = nonce.as_str();
        use base64::Engine;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("kborg:{nonce}"));
        let good = format!(
            "CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\nProxy-Authorization: Basic {auth}\r\n\r\n"
        );
        let approved = crate::copilot::Deployment::GitHub.authorities();
        assert_eq!(
            authority(good.as_bytes(), nonce, &approved).ok(),
            Some("api.github.com".into())
        );
        let tenant =
            crate::copilot::Deployment::EnterpriseCloud("acme.ghe.com".into()).authorities();
        assert!(authority(good.as_bytes(), nonce, &tenant).is_err());
        let ghe = good.replace("api.github.com", "copilot-api.acme.ghe.com");
        assert_eq!(
            authority(ghe.as_bytes(), nonce, &tenant).ok(),
            Some("copilot-api.acme.ghe.com".into())
        );
        assert!(
            authority(
                ghe.replace("acme.ghe.com", "other.ghe.com").as_bytes(),
                nonce,
                &tenant
            )
            .is_err()
        );
        for bad in [
            good.replace(":443", ":80"),
            good.replace("api.github.com", "127.0.0.1"),
            good.replace("api.github.com", "api.github.com.evil.example"),
            good.replace(&auth, "invalid"),
            good.replace("Host: api.github.com:443", "Host: evil.example:443"),
        ] {
            assert!(authority(bad.as_bytes(), nonce, &approved).is_err());
        }
    }
    #[test]
    fn public_resolution_rejects_loopback_private_and_mapped_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "100.64.0.1",
            "100.127.255.254",
            "198.18.0.1",
            "198.19.1.1",
            "240.0.0.1",
            "250.1.2.3",
            "192.0.0.9",
            "192.88.99.1",
            "2001:db8::1",
            "2001::1",
            "2002:0808:0808::1",
            "3fff::1",
            "3fff:123::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!public(
                address.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
            ));
        }
        assert!(public(IpAddr::from([140, 82, 112, 5])));
    }
    #[test]
    fn enterprise_private_opt_in_never_reaches_loopback_link_local_or_metadata() {
        for allowed in [
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "100.64.0.1",
            "fd00::1",
        ] {
            assert!(
                enterprise_private(allowed.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))),
                "{allowed}"
            );
        }
        for denied in [
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(
                !enterprise_private(denied.parse().unwrap_or(IpAddr::from([127, 0, 0, 1]))),
                "{denied}"
            );
        }
    }
    #[test]
    fn unauthenticated_local_clients_cannot_use_broker() -> Result<()> {
        let broker = Broker::start(5, crate::copilot::Deployment::GitHub.authorities(), false)?;
        let mut stream = TcpStream::connect(("127.0.0.1", broker.port))
            .map_err(|_| "test broker unavailable")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|_| "test timeout")?;
        stream
            .write_all(b"CONNECT api.github.com:443 HTTP/1.1\r\n\r\n")
            .map_err(|_| "test send")?;
        let mut response = [0u8; 128];
        let count = stream.read(&mut response).map_err(|_| "test receive")?;
        assert!(response[..count].starts_with(b"HTTP/1.1 403"));
        assert_eq!(broker.report().approved_tunnels, 0);
        assert!(broker.authorization().starts_with("Basic "));
        Ok(())
    }
}
#[cfg(test)]
mod tls_tests {
    use super::*;
    fn hello(host: &str, ech: bool) -> Vec<u8> {
        let mut name = vec![0];
        name.extend((host.len() as u16).to_be_bytes());
        name.extend(host.as_bytes());
        let mut list = (name.len() as u16).to_be_bytes().to_vec();
        list.extend(name);
        let mut extension = vec![0, 0];
        extension.extend((list.len() as u16).to_be_bytes());
        extension.extend(list);
        if ech {
            extension.extend([0xfe, 0x0d, 0, 0]);
        }
        let mut body = vec![3, 3];
        body.extend([0u8; 32]);
        body.extend([0, 0, 2, 0x13, 1, 1, 0]);
        body.extend((extension.len() as u16).to_be_bytes());
        body.extend(extension);
        let size = body.len();
        let mut result = vec![1, (size >> 16) as u8, (size >> 8) as u8, size as u8];
        result.extend(body);
        result
    }
    #[test]
    fn exact_sni_is_required_and_truncation_or_encrypted_hello_fails_closed() {
        let valid = hello("api.github.com", false);
        assert_eq!(sni(&valid).ok(), Some("api.github.com"));
        for length in 0..valid.len() {
            assert!(sni(&valid[..length]).is_err());
        }
        assert!(sni(&hello("api.github.com", true)).is_err());
        assert_eq!(
            sni(&hello("api.github.com.evil.example", false)).ok(),
            Some("api.github.com.evil.example")
        );
    }
}
