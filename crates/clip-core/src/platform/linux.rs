use std::borrow::Cow;
use std::io::Cursor;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arboard::ImageData;
use image::{ImageFormat, RgbaImage};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, CreateWindowAux, WindowClass};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use crate::model::ClipContent;
use crate::watcher::ClipboardBackend;
use crate::{Error, Result};

/// Marker that KeePassXC, KDE and others put on passwords (value "secret").
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// Clipboard backend: reads and writes through `arboard` (X11, Wayland
/// data-control on KDE/wlroots, XWayland on GNOME).
///
/// Change detection:
/// - with X11 (also XWayland): XFixes `SelectionNotify` events on a separate
///   connection — no work while the clipboard is idle;
/// - without X11 (pure Wayland): data-control `selection` events
///   (`ext-data-control-v1`, else `wlr-data-control-unstable-v1`) on a
///   separate connection — also no work while idle;
/// - neither (GNOME Wayland without XWayland): the content is read every
///   second and compared by fingerprint.
pub struct LinuxClipboard {
    state: Mutex<State>,
    events: Option<Events>,
}

struct Events {
    /// Bumped by the XFixes thread on every CLIPBOARD owner change.
    changes: Arc<AtomicI64>,
    wake: Mutex<Receiver<()>>,
}

struct State {
    /// Kept alive: on X11 the owner of a selection must be around to serve it.
    clipboard: Option<arboard::Clipboard>,
    /// Fingerprint of what we put on the clipboard ourselves, so reading it
    /// back after the resulting change event is not reported as a new copy.
    own_write: Option<i64>,
    /// Polling mode only: fingerprint and content of the last read.
    fingerprint: i64,
    current: Option<ClipContent>,
}

/// Handed to an event thread: counts a clipboard change and wakes the watcher.
struct Changed {
    counter: Arc<AtomicI64>,
    tx: Sender<()>,
}

impl Changed {
    /// `false` once nobody listens any more.
    fn notify(&self) -> bool {
        self.counter.fetch_add(1, Ordering::SeqCst);
        self.tx.send(()).is_ok()
    }
}

impl Events {
    fn spawn(name: &str, run: impl FnOnce(Changed) + Send + 'static) -> Option<Events> {
        let changes = Arc::new(AtomicI64::new(0));
        let (tx, rx) = mpsc::channel();
        let changed = Changed {
            counter: Arc::clone(&changes),
            tx,
        };
        thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                crate::platform::background_priority();
                run(changed);
            })
            .ok()?;
        Some(Events {
            changes,
            wake: Mutex::new(rx),
        })
    }
}

impl Default for LinuxClipboard {
    fn default() -> Self {
        LinuxClipboard {
            state: Mutex::new(State {
                clipboard: None,
                own_write: None,
                fingerprint: 0,
                current: None,
            }),
            events: x11::watch_clipboard().or_else(wayland::watch_clipboard),
        }
    }
}

impl State {
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    fn fetch(&mut self) -> Option<ClipContent> {
        let cb = self.clipboard()?;
        if let Ok(text) = cb.get_text() {
            return Some(ClipContent::Text(text));
        }
        cb.get_image().ok().and_then(|img| rgba_to_png(&img).map(ClipContent::Image))
    }

    /// Current content unless it is private or our own write.
    fn fetch_new(&mut self) -> Option<ClipContent> {
        let content = self.fetch()?;
        if self.own_write.take() == Some(fingerprint(&content)) {
            return None;
        }
        // one round trip to the owner, once per change
        (!x11::has_target(PASSWORD_HINT)).then_some(content)
    }
}

impl ClipboardBackend for LinuxClipboard {
    fn change_count(&self) -> i64 {
        if let Some(events) = &self.events {
            return events.changes.load(Ordering::SeqCst);
        }
        let mut state = self.state.lock().unwrap();
        let content = state.fetch();
        let fp = content.as_ref().map_or(0, fingerprint);
        if fp != state.fingerprint {
            state.fingerprint = fp;
            state.current = content;
        }
        state.fingerprint
    }

    fn read(&self) -> Option<ClipContent> {
        let mut state = self.state.lock().unwrap();
        if self.events.is_some() {
            return state.fetch_new();
        }
        // polling mode: `change_count` already fetched it
        let content = state.current.clone()?;
        if state.own_write.take() == Some(state.fingerprint) {
            return None;
        }
        (!x11::has_target(PASSWORD_HINT)).then_some(content)
    }

    fn write(&self, content: &ClipContent) -> Result<i64> {
        let mut state = self.state.lock().unwrap();
        let cb = state
            .clipboard()
            .ok_or_else(|| Error::Clipboard("no X11 or Wayland clipboard available".into()))?;
        match content {
            ClipContent::Text(s) => cb.set_text(s.as_str()),
            ClipContent::Image(png) => {
                let img = image::load_from_memory_with_format(png, ImageFormat::Png)?.into_rgba8();
                cb.set_image(ImageData {
                    width: img.width() as usize,
                    height: img.height() as usize,
                    bytes: Cow::Owned(img.into_raw()),
                })
            }
        }
        .map_err(|e| Error::Clipboard(e.to_string()))?;
        state.own_write = Some(fingerprint(content));
        drop(state);
        Ok(self.change_count())
    }

    fn frontmost_app(&self) -> Option<String> {
        x11::active_window_class()
    }

    fn wait_for_change(&self, timeout: Duration) {
        match &self.events {
            Some(events) => {
                let rx = events.wake.lock().unwrap();
                match rx.recv_timeout(timeout) {
                    Ok(()) => while rx.try_recv().is_ok() {},
                    Err(RecvTimeoutError::Timeout) => {}
                    // the event thread is gone (display server closed): don't spin
                    Err(RecvTimeoutError::Disconnected) => thread::sleep(timeout),
                }
            }
            None => thread::sleep(timeout),
        }
    }

    fn check_interval(&self) -> Duration {
        match self.events {
            Some(_) => Duration::from_secs(30), // safety net only
            None => Duration::from_secs(1),     // each check reads the content
        }
    }
}

fn fingerprint(content: &ClipContent) -> i64 {
    let hash = content.hash();
    i64::from_str_radix(&hash[..15], 16).unwrap_or(1).max(1)
}

fn rgba_to_png(img: &ImageData) -> Option<Vec<u8>> {
    let rgba = RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.to_vec())?;
    let mut out = Cursor::new(Vec::new());
    rgba.write_to(&mut out, ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Things only X11 can tell (also under XWayland); `None`/`false` elsewhere.
mod x11 {
    use super::*;

    fn connect() -> Option<(RustConnection, u32)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        Some((conn, root))
    }

    fn atom(conn: &RustConnection, name: &str) -> Option<Atom> {
        Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    }

    /// Subscribes to CLIPBOARD owner changes (XFixes) on a dedicated thread.
    pub fn watch_clipboard() -> Option<Events> {
        let (conn, root) = connect()?;
        conn.xfixes_query_version(5, 0).ok()?.reply().ok()?;
        let clipboard = atom(&conn, "CLIPBOARD")?;
        conn.xfixes_select_selection_input(
            root,
            clipboard,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .ok()?;
        conn.flush().ok()?;

        Events::spawn("clipboard-xfixes", move |changed| {
            // blocks in the X socket: no wakeups while nothing happens
            while let Ok(event) = conn.wait_for_event() {
                if let Event::XfixesSelectionNotify(xfixes::SelectionNotifyEvent { selection, .. }) = event {
                    if selection == clipboard && !changed.notify() {
                        break;
                    }
                }
            }
        })
    }

    /// Class of the focused window from `_NET_ACTIVE_WINDOW` + `WM_CLASS`
    /// ("firefox", "Code", "org.gnome.TextEditor").
    pub fn active_window_class() -> Option<String> {
        let (conn, root) = connect()?;
        let active = atom(&conn, "_NET_ACTIVE_WINDOW")?;
        let window = conn
            .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()
            .filter(|&w| w != NONE)?;
        let class = conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        // WM_CLASS = "instance\0Class\0"
        let mut parts = class.value.split(|&b| b == 0).filter(|p| !p.is_empty());
        let instance = parts.next()?;
        let name = parts.next().unwrap_or(instance);
        Some(String::from_utf8_lossy(name).into_owned())
    }

    /// Whether the current CLIPBOARD owner offers `target` (asks for TARGETS).
    pub fn has_target(target: &str) -> bool {
        targets().is_some_and(|t| t.iter().any(|n| n == target))
    }

    fn targets() -> Option<Vec<String>> {
        let (conn, root) = connect()?;
        let window = conn.generate_id().ok()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .ok()?;
        let clipboard = atom(&conn, "CLIPBOARD")?;
        let targets = atom(&conn, "TARGETS")?;
        let property = atom(&conn, "PASTEZO_TARGETS")?;
        conn.convert_selection(window, clipboard, targets, property, CURRENT_TIME)
            .ok()?;
        conn.flush().ok()?;

        let deadline = Instant::now() + Duration::from_millis(300);
        let delivered = loop {
            match conn.poll_for_event().ok()? {
                Some(Event::SelectionNotify(e)) if e.requestor == window => break e.property != NONE,
                Some(_) => {}
                None if Instant::now() > deadline => break false,
                None => thread::sleep(Duration::from_millis(5)),
            }
        };
        let atoms: Vec<Atom> = if delivered {
            conn.get_property(true, window, property, AtomEnum::ATOM, 0, 1024)
                .ok()?
                .reply()
                .ok()?
                .value32()?
                .collect()
        } else {
            Vec::new()
        };
        let _ = conn.destroy_window(window);
        let names = atoms
            .into_iter()
            .filter_map(|a| conn.get_atom_name(a).ok()?.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .collect();
        Some(names)
    }
}

/// Clipboard change events on pure Wayland through the data-control protocol
/// (KDE, wlroots compositors; GNOME doesn't offer it). The compositor sends
/// `selection` with a new offer on every change; the offer itself is only a
/// signal here and is destroyed at once — the content is read by `arboard`.
mod wayland {
    use super::*;
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry::WlRegistry, wl_seat::WlSeat};
    use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1::{self as ext_device, ExtDataControlDeviceV1},
        ext_data_control_manager_v1::ExtDataControlManagerV1,
        ext_data_control_offer_v1::ExtDataControlOfferV1,
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self as wlr_device, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    };

    struct Watch {
        /// `None` until the selection present at start has been received:
        /// that one is not a change.
        changed: Option<Changed>,
        /// Cleared when the compositor drops the device or nobody listens.
        alive: bool,
    }

    impl Watch {
        fn selection_changed(&mut self) {
            if let Some(changed) = &self.changed {
                self.alive &= changed.notify();
            }
        }
    }

    pub fn watch_clipboard() -> Option<Events> {
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<Watch>(&conn).ok()?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
        // the standard protocol first, the older wlroots one as a fallback
        if let Ok(manager) = globals.bind::<ExtDataControlManagerV1, _, _>(&qh, 1..=1, ()) {
            manager.get_data_device(&seat, &qh, ());
        } else {
            let manager: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=2, ()).ok()?;
            manager.get_data_device(&seat, &qh, ());
        }
        let mut watch = Watch { changed: None, alive: true };
        // receives the current selection, so it isn't counted as a change
        queue.roundtrip(&mut watch).ok()?;
        if !watch.alive {
            return None;
        }

        Events::spawn("clipboard-wayland", move |changed| {
            watch.changed = Some(changed);
            // blocks in the Wayland socket: no wakeups while nothing happens
            while watch.alive && queue.blocking_dispatch(&mut watch).is_ok() {}
        })
    }

    impl Dispatch<WlRegistry, GlobalListContents> for Watch {
        fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<WlSeat, ()> for Watch {
        fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ExtDataControlManagerV1, ()> for Watch {
        fn event(_: &mut Self, _: &ExtDataControlManagerV1, _: <ExtDataControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ExtDataControlOfferV1, ()> for Watch {
        fn event(_: &mut Self, _: &ExtDataControlOfferV1, _: <ExtDataControlOfferV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ExtDataControlDeviceV1, ()> for Watch {
        fn event(
            watch: &mut Self,
            device: &ExtDataControlDeviceV1,
            event: ext_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                ext_device::Event::Selection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                    watch.selection_changed();
                }
                ext_device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                ext_device::Event::Finished => {
                    device.destroy();
                    watch.alive = false;
                }
                _ => {}
            }
        }

        event_created_child!(Watch, ExtDataControlDeviceV1, [
            ext_device::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
        ]);
    }

    impl Dispatch<ZwlrDataControlManagerV1, ()> for Watch {
        fn event(_: &mut Self, _: &ZwlrDataControlManagerV1, _: <ZwlrDataControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwlrDataControlOfferV1, ()> for Watch {
        fn event(_: &mut Self, _: &ZwlrDataControlOfferV1, _: <ZwlrDataControlOfferV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwlrDataControlDeviceV1, ()> for Watch {
        fn event(
            watch: &mut Self,
            device: &ZwlrDataControlDeviceV1,
            event: wlr_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wlr_device::Event::Selection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                    watch.selection_changed();
                }
                wlr_device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                wlr_device::Event::Finished => {
                    device.destroy();
                    watch.alive = false;
                }
                _ => {}
            }
        }

        event_created_child!(Watch, ZwlrDataControlDeviceV1, [
            wlr_device::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
        ]);
    }
}
