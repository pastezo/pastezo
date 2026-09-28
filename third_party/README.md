# Local copies of third-party crates

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Each copy is
the published crate plus a small, marked patch (`PASTEZO PATCH`). When updating
Slint, copy the new versions from `~/.cargo/registry/src/*/` and re-apply the
patches below, then re-measure (`CLAUDE.md` → Performance).

| crate | version | patch |
|---|---|---|
| `softbuffer` | 0.4.8 | `src/backends/cg.rs` (macOS): frames go to persistent IOSurfaces set as the layer contents instead of a new heap buffer per frame in a CGImage (which CoreAnimation copied). Up to three surfaces take turns (one shown, one the window server may still composite, one drawn into) so fast scrolling never allocates a new 10 MB surface and redraws in full; surfaces not shown are marked purgeable after each frame. `release_spare()` (`lib.rs`, `backend_interface.rs`, `backend_dispatch.rs`; no-op on other backends) frees the ones not on screen and returns `false` while the window server still holds one. `Cargo.toml`: `objc2-io-surface` + CoreFoundation features. |
| `i-slint-backend-winit` | 1.18.1 | `renderer/sw.rs`: row stride = `buffer.len() / height` (IOSurface rows are padded for alignment; other platforms are unchanged, their stride equals the width); blending on the packed pixel, two channels per multiply, bit-exact with Slint's own blend; `release_spare()` 1 s after the last frame, again every second (up to 10 times) while it returns `false` (after a key-window change the window server keeps the old surface a moment: +10 MB until the next frame otherwise); `SLINT_LINE_BY_LINE` read once. `winitwindowadapter.rs`: with Ctrl/⌘ held, a non-ASCII character from a non-Latin layout is replaced by the US character of the physical key, as browsers do (⌘F, ⌘C… work in Russian, Greek, Arabic…). `lib.rs`, `muda.rs`, `event_loop.rs`, `winitwindowadapter.rs`: `set_settings_menu_item(title, callback)` adds "Settings…" (⌘,) to the macOS app menu and calls back on the event loop; `set_window_menu(titles)` adds the standard Window menu (Minimize ⌘M, Zoom, Close Window ⌘W) with translated titles. |
| `i-slint-core` | 1.18.1 | `items/text.rs`: a read-only `TextInput` no longer forces the I-beam cursor, the parent's cursor stays (clip rows show the pointing hand: a click copies; the text is still selectable). Editable text keeps the I-beam. `window.rs`: a key pressed while nothing has the focus (the focused item was removed, scrolled out of view or hidden) focuses the first item that takes it, in tree order, and goes there — stock Slint drops it, so the window's shortcuts and arrows stopped working until a click. |

Effect (Pastezo window, release, Apple Silicon, active window): 41–43 MB → 29 MB; CPU while scrolling −29% (2.48 → 1.75 s over a 4.3 s scripted scroll).
The presented surface was verified to be pixel-identical to Slint's own snapshot.
