# Changelog

## 0.1.1 (2026-09-28)

### Features

- Copied code (source, JSON, SQL, CSS, HTML) is shown in a monospaced font, in the list and in the preview
- Settings → Statistics shows how much you copy: today, the last 7 and 30 days, and a chart by day split into text, links and images
- "Open Pastezo at login" in Settings → General now works: turn it off and Pastezo no longer starts when you log in
- The window opens where you left it, with the same size (and maximized if it was)

### Fixes

- The app icon you chose in Settings stays after updating Pastezo on macOS and Linux
- Copying the same clip twice shows the "Copied" notification again each time
- The chosen theme in Settings → Themes is marked with a red ring, as in Bear
- Notifications now wait their turn instead of replacing the one on screen
- On Wayland without X11 (KDE, Sway and other wlroots desktops) copies are picked up at once, with no polling every second

## 0.1.0 (2026-09-28)

### Features

- First release: clipboard history for macOS (Apple Silicon and Intel), Windows (x64 and arm64) and Linux (x64 and arm64).
- A background agent records what you copy; the window shows, searches and copies it back.
- Search with typos forgiven, keyboard control, themes, typography settings, app icons, import and export, 54 languages.
