//! Linux: dragging a file out of the window with XDND (freedesktop.org drag
//! and drop protocol, version 5), as the drag *source*.
//!
//! Runs on the window's own X connection (the one winit uses), so the pointer
//! grab can take over the button already held down. The drag is modal: until
//! it ends, this reads the connection's events itself; the ones that are not
//! part of the drag (resize, expose, focus, window-manager pings…) are handed
//! back to the window afterwards, so winit still sees them. Offers one type,
//! `text/uri-list`, with the file's `file://` URI.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, GrabMode, GrabStatus, PropMode, SelectionNotifyEvent,
    Window, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const VERSION: u32 = 5;
/// X keycode of Escape on every usual keyboard map (evdev / xkb)
const ESCAPE: u8 = 9;
/// how long a drop target may take to fetch the data and answer
const DROP_TIMEOUT: Duration = Duration::from_secs(5);

struct Atoms {
    aware: Atom,
    selection: Atom,
    enter: Atom,
    position: Atom,
    status: Atom,
    leave: Atom,
    drop: Atom,
    finished: Atom,
    action_copy: Atom,
    uri_list: Atom,
    targets: Atom,
}

impl Atoms {
    fn new(conn: &impl Connection) -> Result<Self> {
        let names = [
            "XdndAware", "XdndSelection", "XdndEnter", "XdndPosition", "XdndStatus", "XdndLeave", "XdndDrop",
            "XdndFinished", "XdndActionCopy", "text/uri-list", "TARGETS",
        ];
        let cookies: Vec<_> = names.iter().map(|n| conn.intern_atom(false, n.as_bytes())).collect::<std::result::Result<_, _>>()?;
        let mut atoms = Vec::with_capacity(names.len());
        for c in cookies {
            atoms.push(c.reply()?.atom);
        }
        Ok(Atoms {
            aware: atoms[0],
            selection: atoms[1],
            enter: atoms[2],
            position: atoms[3],
            status: atoms[4],
            leave: atoms[5],
            drop: atoms[6],
            finished: atoms[7],
            action_copy: atoms[8],
            uri_list: atoms[9],
            targets: atoms[10],
        })
    }
}

/// `file://` URI of a path, percent-encoded (RFC 8089), plus the CRLF that
/// ends every line of `text/uri-list`.
pub fn uri_list(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push_str("\r\n");
    out
}

/// Drags `uri_list` from `source` (the window under the pressed button).
/// `false` if the pointer could not be taken over or nothing accepted the drop.
pub fn run(conn: &impl Connection, source: Window, uri_list: &str) -> Result<bool> {
    let atoms = Atoms::new(conn)?;
    let root = conn.get_geometry(source)?.reply()?.root;
    conn.set_selection_owner(source, atoms.selection, CURRENT_TIME)?;

    // the "hand" cursor from the standard cursor font while dragging
    let font = conn.generate_id()?;
    conn.open_font(font, b"cursor")?;
    let cursor = conn.generate_id()?;
    conn.create_glyph_cursor(cursor, font, font, 60, 61, 0, 0, 0, 0xffff, 0xffff, 0xffff)?;
    let grabbed = conn
        .grab_pointer(
            false,
            source,
            EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            cursor,
            CURRENT_TIME,
        )?
        .reply()?
        .status
        == GrabStatus::SUCCESS;

    let mut others = Vec::new();
    let result = if grabbed { drag(conn, &atoms, root, source, uri_list, &mut others) } else { Ok(false) };

    conn.ungrab_pointer(CURRENT_TIME)?;
    // back to the window's own client (winit): an empty mask sends an event to
    // the client that created the destination window
    for event in others {
        conn.send_event(false, source, EventMask::NO_EVENT, event)?;
    }
    conn.free_cursor(cursor)?;
    conn.close_font(font)?;
    conn.flush()?;
    result
}

/// `others`: events read during the drag that belong to the window, not to it.
fn drag(conn: &impl Connection, atoms: &Atoms, root: Window, source: Window, uri_list: &str, others: &mut Vec<[u8; 32]>) -> Result<bool> {
    let send = |to: Window, kind: Atom, data: [u32; 5]| -> Result<()> {
        conn.send_event(false, to, EventMask::NO_EVENT, ClientMessageEvent::new(32, to, kind, data))?;
        conn.flush()?;
        Ok(())
    };
    let mut target: Option<Window> = None;
    let mut accepted = false;
    let mut dropped_at: Option<Instant> = None;

    loop {
        let Some(raw) = conn.poll_for_raw_event()? else {
            if dropped_at.is_some_and(|t| t.elapsed() > DROP_TIMEOUT) {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(4));
            continue;
        };
        let event = conn.parse_event(raw.as_ref())?;
        match event {
            Event::MotionNotify(e) if dropped_at.is_none() => {
                let under = find_target(conn, atoms, root, source, e.root_x, e.root_y)?;
                if under != target {
                    if let Some(old) = target {
                        send(old, atoms.leave, [source, 0, 0, 0, 0])?;
                    }
                    accepted = false;
                    target = under;
                    if let Some(new) = target {
                        send(new, atoms.enter, [source, VERSION << 24, atoms.uri_list, 0, 0])?;
                    }
                }
                if let Some(t) = target {
                    let at = ((e.root_x as u16 as u32) << 16) | e.root_y as u16 as u32;
                    send(t, atoms.position, [source, 0, at, e.time, atoms.action_copy])?;
                }
            }
            Event::ClientMessage(m) if m.type_ == atoms.status => {
                let d = m.data.as_data32();
                if Some(d[0]) == target {
                    accepted = d[1] & 1 == 1;
                }
            }
            Event::ButtonRelease(e) if dropped_at.is_none() => match target {
                Some(t) if accepted => {
                    send(t, atoms.drop, [source, 0, e.time, 0, 0])?;
                    // the pointer is free again; wait for the target to take the data
                    conn.ungrab_pointer(CURRENT_TIME)?;
                    dropped_at = Some(Instant::now());
                }
                Some(t) => {
                    send(t, atoms.leave, [source, 0, 0, 0, 0])?;
                    return Ok(false);
                }
                None => return Ok(false),
            },
            Event::SelectionRequest(r) if r.selection == atoms.selection => {
                let mut property = r.property;
                if r.target == atoms.uri_list {
                    conn.change_property8(PropMode::REPLACE, r.requestor, r.property, atoms.uri_list, uri_list.as_bytes())?;
                } else if r.target == atoms.targets {
                    conn.change_property32(PropMode::REPLACE, r.requestor, r.property, AtomEnum::ATOM, &[atoms.targets, atoms.uri_list])?;
                } else {
                    property = NONE;
                }
                let notify = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: r.time,
                    requestor: r.requestor,
                    selection: r.selection,
                    target: r.target,
                    property,
                };
                conn.send_event(false, r.requestor, EventMask::NO_EVENT, notify)?;
                conn.flush()?;
            }
            Event::ClientMessage(m) if m.type_ == atoms.finished => return Ok(true),
            Event::KeyPress(k) if k.detail == ESCAPE && dropped_at.is_none() => {
                if let Some(t) = target {
                    send(t, atoms.leave, [source, 0, 0, 0, 0])?;
                }
                return Ok(false);
            }
            // the pointer is the drag's while it lasts
            Event::MotionNotify(_) | Event::ButtonPress(_) | Event::ButtonRelease(_) => {}
            _ => {
                // 32-byte core events only (generic ones cannot be re-sent)
                if let Some(bytes) = raw.as_ref().get(..32).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
                    if bytes[0] & 0x7f != GENERIC_EVENT {
                        others.push(bytes);
                    }
                }
            }
        }
    }
}

/// response type of X Generic Events (longer than 32 bytes)
const GENERIC_EVENT: u8 = 35;

/// The XDND-aware window under the pointer (not our own), walking down from
/// the root: window managers put their frames above the apps' windows.
fn find_target(conn: &impl Connection, atoms: &Atoms, root: Window, source: Window, x: i16, y: i16) -> Result<Option<Window>> {
    let mut window = root;
    loop {
        if window != root && window != source {
            let aware = conn.get_property(false, window, atoms.aware, AtomEnum::ATOM, 0, 1)?.reply()?;
            if aware.value32().and_then(|mut v| v.next()).is_some_and(|version| version >= 3) {
                return Ok(Some(window));
            }
        }
        let child = conn.translate_coordinates(root, window, x, y)?.reply()?.child;
        if child == NONE {
            return Ok(None);
        }
        window = child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(uri_list(std::path::Path::new("/tmp/pastezo-drag/Pastezo 2026-09-28 21.53.07.png")), "file:///tmp/pastezo-drag/Pastezo%202026-09-28%2021.53.07.png\r\n");
        assert_eq!(uri_list(std::path::Path::new("/tmp/é")), "file:///tmp/%C3%A9\r\n");
    }
}
