//! HTTP service thread — tiny_http server with channel-based command dispatch.
//!
//! Each request is parsed into an `HttpCommand` and sent to the main thread
//! over a sync channel. The main thread processes commands against the
//! DurableEngine and sends JSON responses back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Commands sent from HTTP thread to main thread.
/// Each variant carries a `SyncSender<String>` for the response.
pub enum HttpCommand {
    Remember {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Recall {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Dream {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Status {
        reply: mpsc::SyncSender<String>,
    },
    Inspect {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    Offer {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Keystone {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    Connect {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Disconnect {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Neighbors {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    Clock {
        reply: mpsc::SyncSender<String>,
    },
    Checkpoint {
        reply: mpsc::SyncSender<String>,
    },
    Identity {
        reply: mpsc::SyncSender<String>,
    },
    GetRow {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    Terms {
        reply: mpsc::SyncSender<String>,
    },
    InversionCheck {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    Skg {
        reply: mpsc::SyncSender<String>,
    },
    SkgTerm {
        term: String,
        reply: mpsc::SyncSender<String>,
    },
    Delete {
        id: u32,
        reply: mpsc::SyncSender<String>,
    },
    MaxId {
        reply: mpsc::SyncSender<String>,
    },
    Search {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Hybrid {
        body: String,
        reply: mpsc::SyncSender<String>,
    },
    Glossary {
        reply: mpsc::SyncSender<String>,
    },
}

/// Spawn the HTTP server thread.
///
/// Returns a JoinHandle and uses the provided `http_tx` channel to send
/// commands to the main loop. Checks `running` flag each iteration.
pub fn spawn_http(
    port: u16,
    http_tx: mpsc::Sender<HttpCommand>,
    running: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ferricula-http".into())
        .spawn(move || {
            let addr = format!("0.0.0.0:{port}");
            let server = match tiny_http::Server::http(&addr) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[http] failed to bind {addr}: {e}");
                    return;
                }
            };
            eprintln!("[http] listening on {addr}");

            while running.load(Ordering::Relaxed) {
                let mut request = match server.recv_timeout(Duration::from_millis(500)) {
                    Ok(Some(req)) => req,
                    Ok(None) => continue, // timeout
                    Err(_) => continue,   // error, keep trying
                };

                let method = request.method().to_string();
                let url = request.url().to_string();

                // Read body for POST requests
                let mut body = String::new();
                if method == "POST" {
                    let mut reader = request.as_reader();
                    let _ = std::io::Read::read_to_string(&mut reader, &mut body);
                }

                // Parse route and build command
                let (status, response_body) = match dispatch(&method, &url, body, &http_tx) {
                    Ok(json) => (200, json),
                    Err(msg) => (400, format!("{{\"error\":\"{}\"}}", escape_json(&msg))),
                };

                let response = tiny_http::Response::from_string(&response_body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    )
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Access-Control-Allow-Origin"[..],
                            &b"*"[..],
                        )
                        .unwrap(),
                    );

                let _ = request.respond(response);
            }

            eprintln!("[http] shutting down");
        })
        .expect("failed to spawn http thread")
}

/// Route a request to the appropriate HttpCommand.
fn dispatch(
    method: &str,
    url: &str,
    body: String,
    http_tx: &mpsc::Sender<HttpCommand>,
) -> Result<String, String> {
    // Handle CORS preflight
    if method == "OPTIONS" {
        return Ok("{}".to_string());
    }

    // Parse path segments
    let path = url.split('?').next().unwrap_or(url);
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    let cmd = match (method, segments.as_slice()) {
        ("POST", ["remember"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Remember {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["recall"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Recall {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["dream"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Dream {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["status"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Status { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["inspect", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Inspect {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["offer"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Offer {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["keystone", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Keystone {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["connect"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Connect {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["disconnect"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Disconnect {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["neighbors", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Neighbors {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["clock"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Clock { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["checkpoint"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Checkpoint { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["identity"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Identity { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["get", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::GetRow {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["terms"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Terms { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["inversion", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::InversionCheck {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["skg"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Skg { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["skg", term]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::SkgTerm {
                term: term.to_string(),
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("DELETE", ["delete", id_str]) | ("POST", ["delete", id_str]) => {
            let id = id_str
                .parse::<u32>()
                .map_err(|_| "invalid id".to_string())?;
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Delete {
                id,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["maxid"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::MaxId { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["search"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Search {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("POST", ["hybrid"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Hybrid {
                body,
                reply: reply_tx,
            }
            .send_and_recv(http_tx, reply_rx)?
        }
        ("GET", ["glossary"]) => {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            HttpCommand::Glossary { reply: reply_tx }.send_and_recv(http_tx, reply_rx)?
        }
        _ => return Err(format!("unknown route: {method} {url}")),
    };

    Ok(cmd)
}

impl HttpCommand {
    /// Send this command to the main thread and wait for the reply.
    fn send_and_recv(
        self,
        http_tx: &mpsc::Sender<HttpCommand>,
        reply_rx: mpsc::Receiver<String>,
    ) -> Result<String, String> {
        http_tx
            .send(self)
            .map_err(|_| "main thread disconnected".to_string())?;

        reply_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "request timed out".to_string())
    }
}

/// Escape a string for inclusion in a JSON string value.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
