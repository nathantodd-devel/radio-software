//! Serving browsers: the client's files over HTTP, and each listener's
//! WebSocket.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

use scanner_proto::{ClientMessage, SOCKET_PATH};
use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;

use crate::hub::{CLIENT_QUEUE, Clients, Hub, Outgoing};

/// How long a listener's thread waits for it to say something before
/// turning to what there is to send it.
const POLL: Duration = Duration::from_millis(20);
/// How long a listener may take to accept a message.
const STALLED: Duration = Duration::from_secs(10);
/// Longest request head read; ours are a few hundred bytes.
const MAX_HEAD: usize = 16 * 1024;

/// Deal with one connection, whichever kind it turns out to be.
pub fn serve(stream: TcpStream, hub: &Mutex<Hub>, clients: &Clients, web_root: &Path) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    // Look at the request without consuming it: a WebSocket's handshake has
    // to be read by the WebSocket library.
    let mut start = [0u8; 1024];
    let seen = stream.peek(&mut start).unwrap_or(0);
    let request = String::from_utf8_lossy(&start[..seen]);
    let path = request.split_whitespace().nth(1).unwrap_or("");
    if path == SOCKET_PATH {
        listen(stream, hub, clients);
    } else {
        send_file(stream, web_root).ok();
    }
}

/// Run one listener's WebSocket until it goes away.
fn listen(stream: TcpStream, hub: &Mutex<Hub>, clients: &Clients) {
    // Browsers let any page open a WebSocket to any address, so without this
    // check a page from elsewhere could listen in and change settings
    // through a visitor's browser. Programs that aren't browsers send no
    // Origin and are let through.
    // The refusal's type is the WebSocket library's to choose.
    #[allow(clippy::result_large_err)]
    let same_origin = |request: &Request, response: Response| {
        let header = |name| request.headers().get(name).and_then(|value| value.to_str().ok());
        match (header("origin"), header("host")) {
            (Some(origin), Some(host)) if origin.split("://").nth(1) != Some(host) => {
                let mut refusal = ErrorResponse::new(Some("This page isn't the scanner's.".into()));
                *refusal.status_mut() = StatusCode::FORBIDDEN;
                Err(refusal)
            }
            _ => Ok(response),
        }
    };
    let Ok(mut socket) = tungstenite::accept_hdr(stream, same_origin) else {
        return;
    };
    socket.get_ref().set_read_timeout(Some(POLL)).ok();
    // A listener that stops taking what it is sent is dropped.
    socket.get_ref().set_write_timeout(Some(STALLED)).ok();
    let (sender, outgoing) = sync_channel(CLIENT_QUEUE);
    // Greeted and added under one lock, so nothing sent to everyone in
    // between is missed.
    let greeting = {
        let hub = hub.lock().unwrap();
        clients.add(sender);
        hub.greeting()
    };
    let send = |socket: &mut tungstenite::WebSocket<TcpStream>, message: Outgoing| {
        socket.send(match message {
            Outgoing::Text(json) => Message::text(&*json),
            Outgoing::Audio(pcm) => Message::binary(pcm.to_vec()),
        })
    };
    for message in greeting {
        if send(&mut socket, message).is_err() {
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
            if send(&mut socket, message).is_err() {
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
fn send_file(mut stream: TcpStream, web_root: &Path) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        match stream.read(&mut chunk)? {
            0 => break,
            n => head.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let mut words = head.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
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
