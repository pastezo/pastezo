//! Window → agent requests over a Unix socket in the data folder.
//!
//! Needed on Linux: X11/Wayland keep clipboard content only while the process
//! that set it runs, so a copy made from the window (which exits when closed)
//! is done by the agent instead. macOS and Windows keep the content
//! themselves, so the window writes the clipboard directly there.

use std::io::{BufRead, BufReader, Write};
use std::ops::Range;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn socket(dir: &Path) -> PathBuf {
    dir.join("agent.sock")
}

/// A request line: `copy <id>` (the whole clip) or `copy <id> <start> <end>`
/// (bytes `start..end` of its text).
fn parse(line: &str) -> Option<(i64, Option<Range<usize>>)> {
    let mut words = line.trim().strip_prefix("copy ")?.split(' ');
    let id = words.next()?.parse().ok()?;
    let part = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(a), Some(b)) => Some(a.parse().ok()?..b.parse().ok()?),
        _ => return None,
    };
    words.next().is_none().then_some((id, part))
}

/// Agent side: answers `copy …` lines until the process ends.
/// Call only while holding the agent lock (a stale socket file is replaced).
pub fn serve(dir: &Path, copy: impl Fn(i64, Option<Range<usize>>) -> bool + Send + 'static) -> std::io::Result<()> {
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
            let ok = parse(&line).is_some_and(|(id, part)| copy(id, part));
            let _ = (&stream).write_all(if ok { b"ok\n" } else { b"err\n" });
        }
    })?;
    Ok(())
}

/// Window side: asks the agent to put clip `id` (or bytes `part` of its text)
/// on the clipboard. `false` if the agent is not reachable or failed.
pub fn request_copy(dir: &Path, id: i64, part: Option<Range<usize>>) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket(dir)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let sent = match part {
        None => writeln!(stream, "copy {id}"),
        Some(r) => writeln!(stream, "copy {id} {} {}", r.start, r.end),
    };
    if sent.is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn copy_request_reaches_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!request_copy(dir.path(), 1, None), "no agent yet");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        serve(dir.path(), move |id, part| {
            s.lock().unwrap().push((id, part));
            id != 13
        })
        .unwrap();
        assert!(request_copy(dir.path(), 7, None));
        assert!(!request_copy(dir.path(), 13, None));
        assert!(request_copy(dir.path(), 7, Some(2..5)));
        assert_eq!(*seen.lock().unwrap(), [(7, None), (13, None), (7, Some(2..5))]);
    }

    #[test]
    fn requests_parse() {
        assert_eq!(parse("copy 7\n"), Some((7, None)));
        assert_eq!(parse("copy 7 2 5"), Some((7, Some(2..5))));
        assert_eq!(parse("copy 7 2"), None);
        assert_eq!(parse("copy 7 2 5 9"), None);
        assert_eq!(parse("copy x"), None);
        assert_eq!(parse("paste 7"), None);
    }
}
