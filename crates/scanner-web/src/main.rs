//! The scanner as a web server: the receiver is here, and the listening and
//! the controls are in a browser.

mod http;
mod hub;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use scanner::db::Db;

use http::{Access, Site};
use hub::{Clients, Hub};

const USAGE: &str = "\
usage: scanner-web [options]

Scans with the receiver attached to this computer and serves a web page to
listen and control it from. Unless an access code is set, whoever can reach
the page can listen and change settings.

      --listen WHO   who can connect: `local` (this computer only, the
                     default), `all` (anything that can reach this computer
                     on any of its networks), or one of this computer's
                     addresses, to serve on that network only
      --port N       port to serve on (default 1515)
      --web-root DIR the built browser client (default: a `web` folder beside
                     this program, otherwise web/dist under the current folder)
      --local-audio  also play through this computer's speakers
      --db FILE      channel database (default: as for the desktop app)
      --tls-cert FILE, --tls-key FILE
                     serve over HTTPS with this certificate (PEM, with any
                     intermediate certificates after it) and its private key
                     (PEM). Browsers on other devices need HTTPS to run the
                     page; give both options or neither
      --access-code CODE
                     ask for this code before showing or playing anything.
                     Can also be given in the SCANNER_WEB_ACCESS_CODE
                     environment variable, which keeps it out of the list of
                     running programs";

const DEFAULT_PORT: u16 = 1515;
/// How often listeners are told how every channel stands.
const STATUS_INTERVAL: Duration = Duration::from_millis(250);

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("scanner-web: {msg}");
    exit(1)
}

/// Where the built browser client is, unless told otherwise.
fn default_web_root() -> PathBuf {
    let beside_program = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("web")));
    beside_program
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| PathBuf::from("web/dist"))
}

/// What is needed to serve over HTTPS with the certificate and private key
/// in these PEM files.
fn tls_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|e| format!("can't read the certificate in {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("there is no certificate in {}", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| format!("can't read the private key in {}: {e}", key.display()))?;
    let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(certs, key))
        .map_err(|e| format!("can't use that certificate and key: {e}"))?;
    Ok(Arc::new(config))
}

fn main() {
    let (mut address, mut port) = (IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT);
    let (mut web_root, mut db_path, mut local_audio) = (default_web_root(), Db::default_path(), false);
    let (mut tls_cert, mut tls_key) = (None, None);
    let mut access_code = std::env::var("SCANNER_WEB_ACCESS_CODE").ok();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| die(format!("{a} needs a value")));
        match a.as_str() {
            "--listen" => {
                address = match value().as_str() {
                    "local" => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    "all" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    other => other
                        .parse()
                        .unwrap_or_else(|_| die(format!("--listen takes local, all or an address, not {other:?}"))),
                }
            }
            "--port" => port = value().parse().unwrap_or_else(|_| die("--port takes a number")),
            "--web-root" => web_root = PathBuf::from(value()),
            "--db" => db_path = PathBuf::from(value()),
            "--local-audio" => local_audio = true,
            "--tls-cert" => tls_cert = Some(PathBuf::from(value())),
            "--tls-key" => tls_key = Some(PathBuf::from(value())),
            "--access-code" => access_code = Some(value()),
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0)
            }
            _ => die(format!("unknown option {a}\n{USAGE}")),
        }
    }

    let tls = match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => Some(tls_config(&cert, &key).unwrap_or_else(|e| die(e))),
        (None, None) => None,
        _ => die("--tls-cert and --tls-key go together"),
    };
    let access_code = access_code.map(|code| code.trim().to_string());
    if access_code.as_deref() == Some("") {
        die("the access code can't be empty");
    }
    let site = Arc::new(Site {
        web_root,
        tls,
        access: access_code.as_deref().map(Access::new),
    });
    let (web_root, tls, locked) = (&site.web_root, &site.tls, site.access.is_some());
    let db = Db::open(&db_path).unwrap_or_else(|e| die(e));
    let listener = TcpListener::bind(SocketAddr::new(address, port))
        .unwrap_or_else(|e| die(format!("can't listen on {address}:{port}: {e}")));

    let clients = Clients::default();
    let hub = Arc::new(Mutex::new(Hub::new(
        db,
        db_path.with_file_name("recordings"),
        local_audio,
        clients.clone(),
    )));
    {
        let mut hub = hub.lock().unwrap();
        hub.find_receiver();
        hub.start();
        if let Some(problem) = hub.problem() {
            eprintln!("scanner-web: not scanning yet: {problem}");
        }
    }

    let scheme = if tls.is_some() { "https" } else { "http" };
    if address.is_loopback() {
        println!("Serving on {scheme}://localhost:{port}/ to this computer only (see --listen).");
    } else {
        if address.is_unspecified() {
            println!("Serving on port {port}: {scheme}://localhost:{port}/ here, this computer's address elsewhere.");
        } else {
            println!("Serving on {scheme}://{address}:{port}/");
        }
        if !locked {
            println!(
                "There is no access code (see --access-code): anyone who can reach it can listen and change settings."
            );
        }
        if tls.is_none() {
            println!("Browsers on other devices only run the page over HTTPS: see --tls-cert and --tls-key.");
        }
    }
    if locked {
        println!("An access code is needed to use it.");
        if tls.is_none() && !address.is_loopback() {
            println!("Without HTTPS the code crosses the network unencrypted, where others on it could read it.");
        }
    }
    if !web_root.join("index.html").is_file() {
        eprintln!(
            "scanner-web: no browser client in {}; build it (see README.md) or pass --web-root",
            web_root.display()
        );
    }

    let stopping = Arc::new(AtomicBool::new(false));
    {
        // Stop cleanly, so the receiver is released and recordings are closed.
        let (stopping, hub) = (stopping.clone(), hub.clone());
        ctrlc::set_handler(move || {
            stopping.store(true, Relaxed);
            hub.lock().unwrap().shut_down();
            exit(0)
        })
        .ok();
    }
    {
        let hub = hub.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(STATUS_INTERVAL);
                hub.lock().unwrap().send_status();
            }
        });
    }

    for stream in listener.incoming() {
        if stopping.load(Relaxed) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let (hub, clients, site) = (hub.clone(), clients.clone(), site.clone());
        std::thread::spawn(move || http::serve(stream, &site, &hub, &clients));
    }
}
