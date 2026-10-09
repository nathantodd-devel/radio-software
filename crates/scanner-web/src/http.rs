//! Serving browsers: the client's files over HTTP, and each listener's
//! WebSocket.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use scanner_proto::{ClientMessage, SOCKET_PATH};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

use crate::hub::{CLIENT_QUEUE, Clients, Hub, Outgoing};

/// How long a listener's thread waits for it to say something before
/// turning to what there is to send it.
const POLL: Duration = Duration::from_millis(20);
/// How long a browser may take over its request, or to accept what it is
/// sent.
const STALLED: Duration = Duration::from_secs(10);
/// Longest request head read; ours are a few hundred bytes.
const MAX_HEAD: usize = 16 * 1024;

/// A connection to a browser, encrypted or not.
trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

/// The start of a request: everything before its body.
struct Head {
    method: String,
    /// What was asked for, as sent: path and any query.
    target: String,
    /// Names in lower case.
    headers: Vec<(String, String)>,
}

impl Head {
    /// Read a request's head. `None` if the connection closed, stalled or
    /// sent something else.
    fn read(stream: &mut dyn Stream) -> Option<Self> {
        let mut head = Vec::new();
        let mut chunk = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).ok()? {
                0 => return None,
                _ if head.len() > MAX_HEAD => return None,
                n => head.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&head);
        let mut lines = head.split("\r\n");
        let mut request = lines.next()?.split(' ');
        Some(Self {
            method: request.next()?.to_string(),
            target: request.next()?.to_string(),
            headers: lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
                .collect(),
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
    }
}

/// Deal with one connection, whichever kind it turns out to be. With `tls`
/// it is encrypted.
pub fn serve(tcp: TcpStream, tls: Option<Arc<ServerConfig>>, hub: &Mutex<Hub>, clients: &Clients, web_root: &Path) {
    tcp.set_read_timeout(Some(STALLED)).ok();
    // A browser that stops taking what it is sent is dropped.
    tcp.set_write_timeout(Some(STALLED)).ok();
    // Kept to change the timeouts once the connection is wrapped up.
    let Ok(socket) = tcp.try_clone() else { return };
    let mut stream: Box<dyn Stream> = match tls.map(ServerConnection::new) {
        Some(Ok(session)) => Box::new(StreamOwned::new(session, tcp)),
        Some(Err(_)) => return,
        None => Box::new(tcp),
    };
    let Some(head) = Head::read(&mut *stream) else { return };
    if head.target == SOCKET_PATH {
        listen(stream, &socket, &head, hub, clients);
    } else {
        send_file(stream, &head, web_root).ok();
    }
}

/// Answer a request that is being refused.
fn refuse(mut stream: Box<dyn Stream>, status: &str, why: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{why}",
        why.len()
    )
    .ok();
    stream.flush().ok();
}

/// Run one listener's WebSocket until it goes away.
fn listen(mut stream: Box<dyn Stream>, socket: &TcpStream, head: &Head, hub: &Mutex<Hub>, clients: &Clients) {
    let upgrade = head
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let (Some(key), true) = (head.header("sec-websocket-key"), upgrade) else {
        return refuse(
            stream,
            "400 Bad Request",
            "This address is for the scanner's page to connect to.\n",
        );
    };
    // Browsers let any page open a WebSocket to any address, so without this
    // check a page from elsewhere could listen in and change settings
    // through a visitor's browser. Programs that aren't browsers send no
    // Origin and are let through.
    if let (Some(origin), Some(host)) = (head.header("origin"), head.header("host"))
        && origin.split("://").nth(1) != Some(host)
    {
        return refuse(stream, "403 Forbidden", "This page isn't the scanner's.\n");
    }
    let accepted = write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        derive_accept_key(key.as_bytes())
    )
    .and_then(|()| stream.flush());
    if accepted.is_err() {
        return;
    }
    let mut socket_stream = WebSocket::from_raw_socket(stream, Role::Server, None);
    socket.set_read_timeout(Some(POLL)).ok();
    let (sender, outgoing) = sync_channel(CLIENT_QUEUE);
    // Greeted and added under one lock, so nothing sent to everyone in
    // between is missed.
    let greeting = {
        let hub = hub.lock().unwrap();
        clients.add(sender);
        hub.greeting()
    };
    let send = |socket: &mut WebSocket<Box<dyn Stream>>, message: Outgoing| {
        socket.send(match message {
            Outgoing::Text(json) => Message::text(&*json),
            Outgoing::Audio(pcm) => Message::binary(pcm.to_vec()),
        })
    };
    let socket = &mut socket_stream;
    for message in greeting {
        if send(socket, message).is_err() {
            return;
        }
    }
    loop {
        match socket.read() {
            Ok(Message::Text(json)) => {
                // Anything that isn't one of ours is ignored.
                if let Ok(message) = serde_json::from_str::<ClientMessage>(&json) {
                    hub.lock().unwrap().handle(message);
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        for message in outgoing.try_iter() {
            if send(socket, message).is_err() {
                return;
            }
        }
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Answer an HTTP request for one of the client's files.
fn send_file(mut stream: Box<dyn Stream>, head: &Head, web_root: &Path) -> std::io::Result<()> {
    let (method, target) = (head.method.as_str(), head.target.as_str());
    let path = target.split(['?', '#']).next().unwrap_or("").trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // Only plain files under the web root: no climbing out of it.
    let safe = !path.split('/').any(|part| part.is_empty() || part.starts_with('.')) && !path.contains('\\');
    let (status, kind, body) = match (method, safe) {
        ("GET" | "HEAD", true) => match std::fs::read(web_root.join(path)) {
            Ok(body) => ("200 OK", content_type(path), body),
            Err(_) if path == "index.html" => (
                "503 Service Unavailable",
                "text/plain; charset=utf-8",
                format!(
                    "The scanner is running, but its browser client hasn't been built.\n\
                     Build it (see README.md) so that {} contains index.html,\n\
                     or start scanner-web with --web-root pointing at it.\n",
                    web_root.display()
                )
                .into_bytes(),
            ),
            Err(_) => ("404 Not Found", "text/plain; charset=utf-8", b"Not found.\n".to_vec()),
        },
        ("GET" | "HEAD", false) => ("404 Not Found", "text/plain; charset=utf-8", b"Not found.\n".to_vec()),
        _ => (
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Only GET is supported.\n".to_vec(),
        ),
    };
    // The two Cross-Origin headers let the page use shared memory, which a
    // multi-threaded WebAssembly client needs.
    // The page is always fetched afresh. What it loads is named after its
    // contents (`name-0123456789abcdef.wasm`), so can be kept for good.
    let stem = path.split('.').next().unwrap_or("");
    let hashed = stem
        .rsplit(['-', '_'])
        .any(|part| part.len() == 16 && part.bytes().all(|b| b.is_ascii_hexdigit()));
    let cache = if status == "200 OK" && hashed {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(&body)?;
    }
    stream.flush()
}
