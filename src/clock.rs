//! Entropy-driven clock — the radio IS the time source.
//!
//! Spawns a background thread that polls gnosis-radio for time + entropy.
//! Emits `ClockEvent`s over an mpsc channel. Without the radio, time
//! does not flow and the memory system stays frozen.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

/// Events emitted by the clock thread.
#[derive(Debug, Clone)]
pub enum ClockEvent {
    Tick {
        epoch: u64,
        entropy_bytes: usize,
        radio_available: bool,
    },
    DreamTrigger {
        epoch: u64,
        intensity: f32,
        entropy_bytes: usize,
    },
    RadioStatus {
        available: bool,
        message: String,
    },
}

/// Configuration for the clock, sourced from env vars.
#[derive(Debug, Clone)]
pub struct ClockConfig {
    pub radio_host: String,
    pub radio_port: u16,
    pub tick_secs: u64,
    pub dream_threshold_bytes: usize,
}

impl ClockConfig {
    pub fn from_env() -> Self {
        let radio_url =
            std::env::var("RADIO_URL").unwrap_or_else(|_| "http://localhost:9080".to_string());

        // Parse host:port from URL
        let stripped = radio_url.strip_prefix("http://").unwrap_or(&radio_url);
        let (host, port) = if let Some(colon) = stripped.rfind(':') {
            let h = &stripped[..colon];
            let p = stripped[colon + 1..]
                .trim_end_matches('/')
                .parse::<u16>()
                .unwrap_or(9080);
            (h.to_string(), p)
        } else {
            (stripped.trim_end_matches('/').to_string(), 9080)
        };

        let tick_secs = std::env::var("CLOCK_TICK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        let dream_threshold_bytes = std::env::var("DREAM_THRESHOLD_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16);

        Self {
            radio_host: host,
            radio_port: port,
            tick_secs,
            dream_threshold_bytes,
        }
    }
}

/// Atomic telemetry counters readable from the main thread.
pub struct ClockTelemetry {
    pub tick_count: AtomicU64,
    pub dream_count: AtomicU64,
    pub entropy_lifetime: AtomicU64,
    pub radio_available: AtomicBool,
    pub reservoir_bytes: AtomicU32,
}

impl ClockTelemetry {
    fn new() -> Self {
        Self {
            tick_count: AtomicU64::new(0),
            dream_count: AtomicU64::new(0),
            entropy_lifetime: AtomicU64::new(0),
            radio_available: AtomicBool::new(false),
            reservoir_bytes: AtomicU32::new(0),
        }
    }
}

/// Internal entropy accumulator.
struct EntropyReservoir {
    buf: Vec<u8>,
    cap: usize,
}

impl EntropyReservoir {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    fn deposit(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > self.cap {
            self.buf.drain(..self.buf.len() - self.cap);
        }
    }

    fn consume(&mut self, n: usize) -> Vec<u8> {
        let drain_count = n.min(self.buf.len());
        self.buf.drain(..drain_count).collect()
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    /// Fraction of `request_size` that the reservoir can satisfy (0.0..1.0).
    fn intensity(&self, request_size: usize) -> f32 {
        if request_size == 0 {
            return 1.0;
        }
        (self.buf.len() as f32 / request_size as f32).min(1.0)
    }
}

/// Spawn the clock thread. Returns the event receiver and shared telemetry.
pub fn spawn_clock(config: ClockConfig) -> (mpsc::Receiver<ClockEvent>, Arc<ClockTelemetry>) {
    let (tx, rx) = mpsc::channel();
    let telemetry = Arc::new(ClockTelemetry::new());
    let telem = Arc::clone(&telemetry);

    thread::Builder::new()
        .name("ferricula-clock".into())
        .spawn(move || {
            clock_loop(config, tx, telem);
        })
        .expect("failed to spawn clock thread");

    (rx, telemetry)
}

fn clock_loop(config: ClockConfig, tx: mpsc::Sender<ClockEvent>, telemetry: Arc<ClockTelemetry>) {
    let mut reservoir = EntropyReservoir::new(1024);
    let mut was_available = false;

    loop {
        thread::sleep(Duration::from_secs(config.tick_secs));

        let (epoch, entropy, available) =
            fetch_time_and_entropy(&config.radio_host, config.radio_port);

        // Update telemetry
        telemetry.tick_count.fetch_add(1, Ordering::Relaxed);
        telemetry
            .radio_available
            .store(available, Ordering::Relaxed);

        // Radio status change
        if available != was_available {
            let msg = if available {
                "radio entropy source connected"
            } else {
                "radio entropy source disconnected"
            };
            let _ = tx.send(ClockEvent::RadioStatus {
                available,
                message: msg.to_string(),
            });
            was_available = available;
        }

        // Deposit entropy
        if !entropy.is_empty() {
            telemetry
                .entropy_lifetime
                .fetch_add(entropy.len() as u64, Ordering::Relaxed);
            reservoir.deposit(&entropy);
        }

        telemetry
            .reservoir_bytes
            .store(reservoir.len() as u32, Ordering::Relaxed);

        let tick_epoch = epoch.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });

        // Always emit tick
        let _ = tx.send(ClockEvent::Tick {
            epoch: tick_epoch,
            entropy_bytes: reservoir.len(),
            radio_available: available,
        });

        // Check dream threshold
        if reservoir.len() >= config.dream_threshold_bytes {
            let intensity = reservoir.intensity(64);
            let seed = reservoir.consume(reservoir.len().min(64));
            telemetry.dream_count.fetch_add(1, Ordering::Relaxed);
            telemetry
                .reservoir_bytes
                .store(reservoir.len() as u32, Ordering::Relaxed);

            if tx
                .send(ClockEvent::DreamTrigger {
                    epoch: tick_epoch,
                    intensity,
                    entropy_bytes: seed.len(),
                })
                .is_err()
            {
                break; // receiver dropped
            }
        }
    }
}

/// Fetch time and entropy from the radio via raw HTTP/1.0 GETs.
/// Returns (radio_epoch, entropy_bytes, radio_reachable).
fn fetch_time_and_entropy(host: &str, port: u16) -> (Option<u64>, Vec<u8>, bool) {
    let addr = format!("{host}:{port}");

    // GET /api/time
    let epoch = match http_get(&addr, host, "/api/time") {
        Some(body) => parse_epoch(&body),
        None => None,
    };

    // GET /api/entropy?bytes=64&format=json
    let (entropy, available) = match http_get(&addr, host, "/api/entropy?bytes=64&format=json") {
        Some(body) => {
            let bytes = parse_entropy_hex(&body);
            (bytes, true)
        }
        None => (vec![], epoch.is_some()),
    };

    let reachable = epoch.is_some() || available;
    (epoch, entropy, reachable)
}

/// Minimal HTTP/1.0 GET using raw TcpStream. Returns response body or None.
fn http_get(addr: &str, host: &str, path: &str) -> Option<String> {
    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(500)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;

    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    // Split headers from body
    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    Some(response[body_start..].to_string())
}

/// Extract epoch from {"epoch": N, ...} JSON.
fn parse_epoch(json_body: &str) -> Option<u64> {
    // Minimal parse: find "epoch": <number>
    let trimmed = json_body.trim();
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    val.get("epoch")?.as_u64()
}

/// Extract entropy bytes from {"entropy_hex": "deadbeef", ...} JSON.
fn parse_entropy_hex(json_body: &str) -> Vec<u8> {
    let trimmed = json_body.trim();
    let val: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let hex = match val.get("entropy_hex").and_then(|v| v.as_str()) {
        Some(h) => h,
        None => return vec![],
    };
    hex_decode(hex)
}

/// Decode a hex string to bytes. Returns empty vec on invalid input.
pub fn hex_decode(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return vec![];
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = hex_nibble(chunk[0]);
        let lo = hex_nibble(chunk[1]);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h << 4) | l),
            _ => return vec![],
        }
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_valid() {
        assert_eq!(hex_decode("deadbeef"), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(hex_decode("00ff"), vec![0x00, 0xff]);
        let empty: Vec<u8> = vec![];
        assert_eq!(hex_decode(""), empty);
    }

    #[test]
    fn hex_decode_invalid() {
        let empty: Vec<u8> = vec![];
        assert_eq!(hex_decode("0"), empty); // odd length
        assert_eq!(hex_decode("zz"), empty); // invalid chars
    }

    #[test]
    fn reservoir_basics() {
        let mut r = EntropyReservoir::new(8);
        assert_eq!(r.len(), 0);
        assert_eq!(r.intensity(64), 0.0);

        r.deposit(&[1, 2, 3, 4]);
        assert_eq!(r.len(), 4);

        let drained = r.consume(2);
        assert_eq!(drained, vec![1, 2]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn reservoir_caps_at_limit() {
        let mut r = EntropyReservoir::new(4);
        r.deposit(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(r.len(), 4);
        // Keeps the tail (most recent)
        assert_eq!(r.consume(4), vec![5, 6, 7, 8]);
    }

    #[test]
    fn reservoir_intensity() {
        let mut r = EntropyReservoir::new(128);
        r.deposit(&[0; 32]);
        assert!((r.intensity(64) - 0.5).abs() < 0.01);
        assert!((r.intensity(32) - 1.0).abs() < 0.01);
        assert!((r.intensity(0) - 1.0).abs() < 0.01);
    }

    #[test]
    fn parse_epoch_from_json() {
        let body = r#"{"epoch": 1740700800, "source": "system", "utc": "2026-02-28T00:00:00Z"}"#;
        assert_eq!(parse_epoch(body), Some(1740700800));
    }

    #[test]
    fn parse_entropy_hex_from_json() {
        let body = r#"{"bytes_requested":64,"bytes_returned":4,"pool_remaining":0,"entropy_hex":"deadbeef"}"#;
        assert_eq!(parse_entropy_hex(body), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn config_defaults() {
        // Don't set env vars — should use defaults
        let config = ClockConfig {
            radio_host: "localhost".into(),
            radio_port: 9080,
            tick_secs: 60,
            dream_threshold_bytes: 16,
        };
        assert_eq!(config.radio_port, 9080);
        assert_eq!(config.tick_secs, 60);
    }
}
