//! Window → agent requests over a Unix socket in the data folder.
//!
//! - `copy`, Linux only: X11/Wayland keep clipboard content only while the
//!   process that set it runs, so a copy made from the window (which exits
//!   when closed) is done by the agent instead. macOS and Windows keep the
//!   content themselves, so the window writes the clipboard directly there.
//! - `hotkey`, macOS and Linux: Settings → General changed the global
//!   shortcut; the agent registers the saved one (Windows: a window message,
//!   see `platform/hotkeys_windows.rs`).

use std::io::{BufRead, BufReader, Write};
use std::ops::Range;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn socket(dir: &Path) -> PathBuf {
    dir.join("agent.sock")
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Put clip `id` (or bytes `part` of its text) on the clipboard.
    Copy(i64, Option<Range<usize>>),
    /// Register the shortcut saved in the `hotkey` file.
    Hotkey,
    /// A derived text value (e.g. one JSON node), kept alive by the Linux agent.
    Text(String),
}

/// A request line: `copy <id>` (the whole clip), `copy <id> <start> <end>`
/// (bytes `start..end` of its text), `hotkey`.
fn parse(line: &str) -> Option<Request> {
    let line = line.trim();
    if line == "hotkey" {
        return Some(Request::Hotkey);
    }
    if line == "text" { return Some(Request::Text(String::new())); }
    if let Some(hex) = line.strip_prefix("text ") {
        if hex.len() % 2 != 0 || hex.len() > 2 * crate::MAX_CLIP_BYTES { return None; }
        let bytes: Option<Vec<u8>> = hex.as_bytes().chunks_exact(2).map(|pair| {
            let digit = |c: u8| (c as char).to_digit(16).map(|n| n as u8);
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        }).collect();
        return Some(Request::Text(String::from_utf8(bytes?).ok()?));
    }
    let mut words = line.strip_prefix("copy ")?.split(' ');
    let id = words.next()?.parse().ok()?;
    let part = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(a), Some(b)) => Some(a.parse().ok()?..b.parse().ok()?),
        _ => return None,
    };
    words.next().is_none().then_some(Request::Copy(id, part))
}

fn line(request: &Request) -> String {
    match request {
        Request::Copy(id, None) => format!("copy {id}"),
        Request::Copy(id, Some(r)) => format!("copy {id} {} {}", r.start, r.end),
        Request::Hotkey => "hotkey".into(),
        Request::Text(text) => format!("text {}", text.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()),
    }
}

/// Agent side: answers requests until the process ends; `handle` says
/// whether it worked ("ok" / "no"; a line it does not know: "err").
/// Call only while holding the agent lock (a stale socket file is replaced).
pub fn serve(dir: &Path, handle: impl Fn(Request) -> bool + Send + 'static) -> std::io::Result<()> {
    let path = socket(dir);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("agent-ipc".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let reply: &[u8] = match parse(&line).map(&handle) {
                Some(true) => b"ok\n",
                Some(false) => b"no\n",
                None => b"err\n",
            };
            let _ = (&stream).write_all(reply);
        }
    })?;
    Ok(())
}

/// Window side: sends a request. Whether it worked; `None` if the agent is
/// not reachable or does not know the request (an agent from before it,
/// still running after an update: "err").
pub fn request(dir: &Path, request: Request) -> Option<bool> {
    let mut stream = UnixStream::connect(socket(dir)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    writeln!(stream, "{}", line(&request)).ok()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).ok()?;
    match reply.trim() {
        "ok" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Window side: asks the agent to put clip `id` (or bytes `part` of its text)
/// on the clipboard. `false` if the agent is not reachable or failed.
pub fn request_copy(dir: &Path, id: i64, part: Option<Range<usize>>) -> bool {
    request(dir, Request::Copy(id, part)) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn requests_reach_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!request_copy(dir.path(), 1, None), "no agent yet");
        assert_eq!(request(dir.path(), Request::Hotkey), None, "no agent yet");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        serve(dir.path(), move |r| {
            s.lock().unwrap().push(r.clone());
            r != Request::Copy(13, None)
        })
        .unwrap();
        assert!(request_copy(dir.path(), 7, None));
        assert!(!request_copy(dir.path(), 13, None));
        assert!(request_copy(dir.path(), 7, Some(2..5)));
        assert_eq!(request(dir.path(), Request::Hotkey), Some(true));
        assert_eq!(request(dir.path(), Request::Copy(13, None)), Some(false), "refused: \"no\"");
        // an older agent answers "err" to what it does not know: not a refusal
        let mut stream = UnixStream::connect(socket(dir.path())).unwrap();
        writeln!(stream, "paste 1").unwrap();
        let mut reply = String::new();
        BufReader::new(&stream).read_line(&mut reply).unwrap();
        assert_eq!(reply, "err\n");
        let expected = [Request::Copy(7, None), Request::Copy(13, None), Request::Copy(7, Some(2..5)), Request::Hotkey, Request::Copy(13, None)];
        assert_eq!(*seen.lock().unwrap(), expected);
    }

    #[test]
    fn requests_parse() {
        assert_eq!(parse("copy 7\n"), Some(Request::Copy(7, None)));
        assert_eq!(parse("copy 7 2 5"), Some(Request::Copy(7, Some(2..5))));
        assert_eq!(parse("hotkey\n"), Some(Request::Hotkey));
        for text in ["", "hello\nПривет 😀", "\"quoted\"\ttext"] {
            let request = Request::Text(text.into());
            assert_eq!(parse(&line(&request)), Some(request));
        }
        assert_eq!(parse("text ff"), None);
        assert_eq!(parse("text xyz"), None);
        assert_eq!(parse("copy 7 2"), None);
        assert_eq!(parse("copy 7 2 5 9"), None);
        assert_eq!(parse("copy x"), None);
        assert_eq!(parse("paste 7"), None);
        for r in [Request::Copy(7, None), Request::Copy(7, Some(2..5)), Request::Hotkey] {
            assert_eq!(parse(&line(&r)), Some(r));
        }
    }
}
