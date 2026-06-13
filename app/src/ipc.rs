//! Single-instance forwarding — the "snap open" path.
//!
//! Cold-starting markdown-delight is GPU-bound: on first launch the wgpu/Vulkan
//! pipeline + dGPU spin up for ~seconds (see the startup-time finding). Launching
//! a SECOND process for every tray/dock click pays that cost every single time —
//! which is exactly the spinner Parker sees.
//!
//! Instead we run as a single instance. The first launch binds a Unix socket and
//! becomes the primary. Every later launch CONNECTS to that socket, hands over
//! the file it was asked to open (or nothing, for a bare click), and exits
//! immediately — no GPU, no window, no wait. The already-resident primary opens
//! the file (or just raises) on its own event loop, so the click feels instant.
//!
//! Same proven pattern Zed/VS Code use; std-only, no extra deps.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::{env, fs, thread};

/// A forwarded launch request: the files to open (empty = bare "just raise").
pub type OpenRequest = Vec<PathBuf>;

/// Per-user socket. `$XDG_RUNTIME_DIR` is already per-user and tmpfs-backed
/// (auto-cleaned on logout); fall back to the temp dir keyed by `$USER`.
fn socket_path() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("markdown-delight.sock");
    }
    let user = env::var("USER").unwrap_or_else(|_| "nobody".into());
    env::temp_dir().join(format!("markdown-delight-{user}.sock"))
}

/// Connect timeout is irrelevant for an AF_UNIX socket (local), so a plain
/// blocking connect is fine and fast — it either refuses instantly or connects.
fn encode(paths: &[PathBuf]) -> Vec<u8> {
    let mut buf = Vec::new();
    for p in paths {
        // absolute so the primary (whose cwd differs) resolves it correctly
        let abs = fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        buf.extend_from_slice(abs.to_string_lossy().as_bytes());
        buf.push(b'\n');
    }
    buf
}

fn decode(bytes: &[u8]) -> OpenRequest {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Client side: if a primary is already running, hand it `paths` and return
/// `true` (the caller should then exit WITHOUT starting the GPU app). Returns
/// `false` if there is no primary (this process should become it).
pub fn try_forward(paths: &[PathBuf]) -> bool {
    forward_to(&socket_path(), paths)
}

fn forward_to(sock: &PathBuf, paths: &[PathBuf]) -> bool {
    match UnixStream::connect(sock) {
        Ok(mut stream) => {
            let _ = stream.write_all(&encode(paths));
            let _ = stream.flush();
            // half-close so the primary's read() returns EOF promptly
            let _ = stream.shutdown(std::net::Shutdown::Write);
            true
        }
        Err(_) => {
            // No listener (first launch) or a stale socket file from a crashed
            // primary — either way we are not forwarding.
            false
        }
    }
}

/// Server side: become the primary. Binds the socket and spawns a blocking
/// accept thread that decodes each connection into an `OpenRequest` and pushes
/// it onto the returned channel. The main GPUI loop drains this channel.
///
/// Returns `None` if we could not bind (then run as a plain lone instance —
/// forwarding is simply unavailable, never fatal).
pub fn start_server() -> Option<Receiver<OpenRequest>> {
    serve_on(&socket_path())
}

fn serve_on(path: &PathBuf) -> Option<Receiver<OpenRequest>> {
    // Clear a stale socket left by a crashed primary; safe because we only get
    // here after `try_forward` found no live listener.
    let _ = fs::remove_file(path);

    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[md] single-instance socket unavailable ({e}); running standalone");
            return None;
        }
    };

    let (tx, rx) = mpsc::channel::<OpenRequest>();
    thread::Builder::new()
        .name("md-ipc".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let mut bytes = Vec::new();
                if stream.read_to_end(&mut bytes).is_ok() && tx.send(decode(&bytes)).is_err() {
                    break; // receiver gone → app is shutting down
                }
            }
        })
        .ok()?;

    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn encode_decode_round_trips() {
        let paths = vec![PathBuf::from("/a/b.md"), PathBuf::from("/c d/e.md")];
        assert_eq!(decode(&encode(&paths)), paths);
    }

    #[test]
    fn decode_skips_blank_lines() {
        assert_eq!(decode(b"\n\n"), Vec::<PathBuf>::new());
        assert_eq!(decode(b"/x.md\n\n/y.md\n"), vec![PathBuf::from("/x.md"), PathBuf::from("/y.md")]);
    }

    #[test]
    fn forward_fails_with_no_server() {
        let sock = env::temp_dir().join("md-ipc-test-absent.sock");
        let _ = fs::remove_file(&sock);
        assert!(!forward_to(&sock, &[PathBuf::from("/whatever.md")]));
    }

    #[test]
    fn server_receives_forwarded_request() {
        // unique path per test process to avoid cross-test collisions
        let sock = env::temp_dir().join(format!("md-ipc-test-{}.sock", std::process::id()));
        let rx = serve_on(&sock).expect("bind");

        // a sibling launch with no server already running would NOT forward...
        // but now that we ARE the server, a forward must succeed and arrive.
        assert!(forward_to(&sock, &[PathBuf::from("/tmp/doc.md")]));

        // poll the channel briefly (accept runs on the listener thread)
        let deadline = Instant::now() + Duration::from_secs(2);
        let got = loop {
            if let Ok(req) = rx.try_recv() {
                break req;
            }
            assert!(Instant::now() < deadline, "no forwarded request arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(got, vec![PathBuf::from("/tmp/doc.md")]);

        // a bare click (no file) forwards as an empty request = "just raise"
        assert!(forward_to(&sock, &[]));
        let deadline = Instant::now() + Duration::from_secs(2);
        let bare = loop {
            if let Ok(req) = rx.try_recv() {
                break req;
            }
            assert!(Instant::now() < deadline, "no bare request arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(bare.is_empty());

        let _ = fs::remove_file(&sock);
    }
}
