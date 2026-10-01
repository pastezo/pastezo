# Changelog

## 1.1.0 (2026-10-01)

### Features

- Find clips typed in the English or Russian keyboard layout, with highlighted matching excerpts from the full text
- Settings → General can keep clips for a week, a month or a year: older ones are deleted on their own, pinned ones stay
- Links show their site's icon, taken from the site itself; Settings → General can turn it off
- Settings → Typography has presets: Compact, Default and Large set the text size, line height and spacing in one click; "Defaults" is now "Reset font"

#### Filter clipboard history with colorful smart hashtags

Automatically recognize email addresses, phone numbers, websites, code, JSON, hex colors and images. Tags are hidden by default; enable Show automatic tags in Settings → General. Tags apply to existing history and new copies, and combine with text search.

Recognize websites only when the clip is a standalone HTTP(S) or www address. Remove incorrect website tags from saved prose and code containing links.

Use flat rounded hashtags with pastel color accents and larger 14 px labels, without outlines, shadows or counters. Highlight selection and keyboard focus using a stronger color fill. Translate the automatic-tags visibility setting into every supported language.

Keep the All button fixed at the start of the tag row, including while scrolling tags, and highlight it when no tag filter is selected. Hide tags with no clips and clear a selected tag when its last clip is deleted.

Align the tag row's colored faces with the left edge of clipboard snippets.

### Fixes

- Fewer antivirus false alarms on Windows: "Open Pastezo at login" is off until turned on (never set up from a temp folder), the update check uses the system's WinHTTP instead of running curl.exe, and both .exe files carry version details
- The clip content hash is XXH3 instead of BLAKE3 (no cryptographic code in the app); the history is moved over on the first start, so repeats of older clips are still found

## 1.0.0 (2026-09-29)

### Features

- Set a shortcut in Settings → General to open Pastezo from any app (on Linux under X11; Wayland apps can’t catch keys)
- Settings → General can hide Pastezo from screen sharing, recordings and screenshots on macOS and Windows
- ⌘1…⌘9 (Ctrl+1…9 on Windows and Linux) paste the first nine clips of the list
- Return pastes the selected clip straight into the app you came from
- When a new version is out, Pastezo says so once a day when its window opens, with a Download button

## 0.1.2 (2026-09-28)

### Fixes

- The window no longer flashes black while scrolling or selecting text on macOS

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
