//! Serving browsers: the client's files over HTTP, and each listener's
//! WebSocket.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::digest::{SHA256, digest};
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
/// Longest request body read: only the access code is ever sent.
const MAX_BODY: usize = 4096;
/// How long after a wrong access code before another is looked at, from
/// anyone: guessing is limited to one try in this long.
const WRONG_CODE_DELAY: Duration = Duration::from_secs(2);
/// The cookie a browser that has given the access code is known by.
const COOKIE: &str = "scanner_access";
/// Where the page asking for the access code sends it.
const LOGIN_PATH: &str = "/login";

/// How browsers are served.
pub struct Site {
    /// The built browser client.
    pub web_root: PathBuf,
    /// To serve over HTTPS.
    pub tls: Option<Arc<ServerConfig>>,
    /// To let in only those with the access code.
    pub access: Option<Access>,
}

/// The access code, and who has given it.
pub struct Access {
    code: String,
    /// What a browser that has given the code holds in its cookie. Made from
    /// the code, so it outlasts a restart but not a change of code.
    token: String,
    /// When the next code will be looked at.
    next_try: Mutex<Instant>,
}

impl Access {
    pub fn new(code: &str) -> Self {
        let hash = digest(&SHA256, format!("scanner-web access\n{code}").as_bytes());
        Self {
            code: code.to_string(),
            token: hash.as_ref().iter().map(|byte| format!("{byte:02x}")).collect(),
            next_try: Mutex::new(Instant::now()),
        }
    }

    /// Whether the browser that sent this request has given the code.
    fn admits(&self, head: &Head) -> bool {
        let cookies = head.header("cookie").unwrap_or("");
        let mut cookies = cookies.split(';').filter_map(|cookie| cookie.trim().split_once('='));
        cookies.any(|(name, value)| name == COOKIE && same(value, &self.token))
    }

    /// Whether `code` is the access code. `None` if it is too soon after a
    /// wrong one to say.
    fn check(&self, code: &str) -> Option<bool> {
        let mut next_try = self.next_try.lock().unwrap();
        if Instant::now() < *next_try {
            return None;
        }
        let right = same(code.trim(), &self.code);
        if !right {
            *next_try = Instant::now() + WRONG_CODE_DELAY;
        }
        Some(right)
    }
}

/// Whether two strings are equal, taking the same time wherever they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |differences, (a, b)| differences | (a ^ b))
            == 0
}

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
    /// The start of the body: whatever arrived along with the head.
    body: Vec<u8>,
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
        let end = head.windows(4).position(|w| w == b"\r\n\r\n")?;
        let body = head.split_off(end + 4);
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
            body,
        })
    }

    /// The path asked for, without any query.
    fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or("")
    }

    /// Read the rest of the body and take one field from it, as a form sends
    /// them (`name=value&...`).
    fn form_field(&mut self, stream: &mut dyn Stream, name: &str) -> Option<String> {
        let length: usize = self.header("content-length")?.parse().ok()?;
        if length > MAX_BODY {
            return None;
        }
        let mut chunk = [0u8; 1024];
        while self.body.len() < length {
            match stream.read(&mut chunk).ok()? {
                0 => return None,
                n => self.body.extend_from_slice(&chunk[..n]),
            }
        }
        let body = String::from_utf8_lossy(&self.body[..length]).into_owned();
        let value = body
            .split('&')
            .filter_map(|field| field.split_once('='))
            .find(|field| field.0 == name)?
            .1;
        // Undo the form's encoding: `+` for a space, `%XX` for other bytes.
        let (mut bytes, mut decoded) = (value.bytes(), Vec::new());
        while let Some(byte) = bytes.next() {
            decoded.push(match byte {
                b'+' => b' ',
                b'%' => {
                    let hex = [bytes.next()?, bytes.next()?];
                    u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
                }
                other => other,
            });
        }
        String::from_utf8(decoded).ok()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
    }
}

/// Deal with one connection, whichever kind it turns out to be.
pub fn serve(tcp: TcpStream, site: &Site, hub: &Mutex<Hub>, clients: &Clients) {
    tcp.set_read_timeout(Some(STALLED)).ok();
    // A browser that stops taking what it is sent is dropped.
    tcp.set_write_timeout(Some(STALLED)).ok();
    // Kept to change the timeouts once the connection is wrapped up.
    let Ok(socket) = tcp.try_clone() else { return };
    let mut stream: Box<dyn Stream> = match site.tls.clone().map(ServerConnection::new) {
        Some(Ok(session)) => Box::new(StreamOwned::new(session, tcp)),
        Some(Err(_)) => return,
        None => Box::new(tcp),
    };
    let Some(mut head) = Head::read(&mut *stream) else {
        return;
    };
    // Nothing is served, and no listener let in, without the access code.
    if let Some(access) = site.access.as_ref().filter(|access| !access.admits(&head)) {
        return ask_for_code(stream, &mut head, access, site.tls.is_some());
    }
    if head.target == SOCKET_PATH {
        listen(stream, &socket, &head, hub, clients);
    } else if head.path() == LOGIN_PATH {
        // Someone already let in, sending the code again or going back.
        respond(stream, "303 See Other", "Location: /\r\n", "").ok();
    } else {
        send_file(stream, &head, &site.web_root).ok();
    }
}

/// Deal with a request from a browser that hasn't given the access code: let
/// it in if this is the code, otherwise ask for it.
fn ask_for_code(mut stream: Box<dyn Stream>, head: &mut Head, access: &Access, encrypted: bool) {
    if head.target == SOCKET_PATH {
        return refuse(stream, "401 Unauthorized", "The access code is needed first.\n");
    }
    let mut problem = "";
    if head.method == "POST" && head.path() == LOGIN_PATH {
        let code = head.form_field(&mut *stream, "code").unwrap_or_default();
        match access.check(&code) {
            Some(true) => {
                // Kept from scripts and from other sites; sent only over
                // HTTPS when that is how it was given.
                let cookie = format!(
                    "Set-Cookie: {COOKIE}={}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict{}\r\nLocation: /\r\n",
                    access.token,
                    if encrypted { "; Secure" } else { "" }
                );
                respond(stream, "303 See Other", &cookie, "").ok();
                return;
            }
            Some(false) => problem = "That isn't the access code.",
            None => problem = "Too many tries. Wait a few seconds and try again.",
        }
    }
    let page = LOGIN_PAGE.replace("{problem}", problem);
    // Not an error status: the page is what was asked for, as far as a
    // browser's address bar is concerned.
    respond(stream, "200 OK", "Cache-Control: no-store\r\n", &page).ok();
}

/// The page that asks for the access code.
const LOGIN_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Airspy Scanner</title>
<style>
body { margin: 0; padding: 32px 16px; background: #121318; color: #e6e9ee; font: 16px/1.5 system-ui, sans-serif; }
form { max-width: 360px; margin: 0 auto; padding: 24px; border: 1px solid #313743; border-radius: 8px; background: #1c2027; }
h1 { margin: 0 0 12px; font-size: 20px; }
label { display: block; margin-bottom: 6px; color: #8b93a1; font-size: 14px; }
input, button { box-sizing: border-box; width: 100%; padding: 10px 12px; border-radius: 6px; font: inherit; }
input { border: 1px solid #313743; background: #121318; color: inherit; }
button { margin-top: 12px; border: 0; background: #4cc38a; color: #121318; font-weight: 600; cursor: pointer; }
p { min-height: 1.5em; margin: 12px 0 0; color: #e5645c; font-size: 14px; }
</style>
</head>
<body>
<form method="post" action="/login">
<h1>Airspy Scanner</h1>
<label for="code">Access code</label>
<input id="code" name="code" type="password" autocomplete="current-password" autofocus required>
<button>Open</button>
<p role="alert">{problem}</p>
</form>
</body>
</html>
"#;

/// Send a short answer that is a page, or nothing but headers. `headers`
/// are whole lines, each ending in a line break.
fn respond(mut stream: Box<dyn Stream>, status: &str, headers: &str, page: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{headers}\
         Connection: close\r\n\r\n{page}",
        page.len()
    )?;
    stream.flush()
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
    let method = head.method.as_str();
    let path = head.path().trim_start_matches('/');
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
