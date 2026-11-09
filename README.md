# Plainpad

A small, fast plain-text editor for Windows, macOS and Linux, written in Rust.
It has no AI features and no formatting: you open text, edit it, and save it.

The aim is low memory use and fast response. It won't match a hand-written 17 KB assembly
editor, but it should use far less memory than modern Windows Notepad (80+ MB).

## Features

**Tabs**
- Each open document has its own tab, with its own undo history, encoding and line-ending settings.
- A ● on a tab means it has unsaved changes. Click × or middle-click a tab to close it, click + or
  double-click the empty part of the tab bar for a new tab, and drag tabs to reorder them.
- When there are too many tabs to fit, the tab bar scrolls (mouse wheel) and keeps the active tab visible.
- Each tab keeps its own scroll position, so switching away and back returns you to the same place.
- Drag a tab out of the tab bar and drop it outside the window to open it in a new window. Drop it on
  another Plainpad window to move it there. Each window has its own tabs, menus and find bar; the font,
  zoom, word wrap and status bar settings are shared.
  Dropping on another window's tab bar puts the tab at that spot. Dropping back on its own tab bar cancels.
- New documents are named Untitled, Untitled 2, and so on.
- Opening a file that's already open switches to its tab. An empty, untouched Untitled tab is reused
  rather than left behind.
- Closing the last tab leaves a fresh empty one instead of quitting. In a second window, closing its last tab
  closes that window.

**Files**
- New Tab, Open, Save, Save As, Rename, Close Tab, Print
- The Open dialog can select several files. You can also drag files onto the window or pass several paths on
  the command line; each opens in its own tab.
- Saving writes a temporary file, flushes it to disk and renames it over the original, so a failed save (disk
  full, crash) never truncates your file. Symlinks, permissions and hard links are kept.
- Closing a tab with unsaved changes asks whether to save it. Closing a window asks about each of its modified
  tabs, and Exit asks about every modified tab in every window, in turn. Cancel stops the close or exit. The window title shows `*`
  while the file is modified, and it clears if you undo back to the saved state.

**Text and Unicode**
- Shaping, bidirectional text (Arabic, Hebrew and mixed-direction lines), complex scripts and font fallback
  for CJK and other scripts
- IME input with inline display of in-progress composition
- Encoding detection: UTF-8, UTF-8 with BOM, UTF-16 LE/BE (with BOM), and Windows-1252 as a fallback.
  Every byte round-trips unchanged. You can change the encoding used for saving with **Format → Encoding**.
- Line-ending detection: CRLF, LF or CR. Files with mixed endings are flagged and normalised on save.
  You can convert between them with **Format → Line Endings**.

**Right-click menus**
- In the text: Undo, Redo, Cut, Copy, Paste, Delete and Select All, with unavailable items greyed out. On a
  link, Open Link and Copy Link Address too. Right-clicking outside the selection moves the cursor there first.
- On a tab: Close Tab, Close Other Tabs, Close Tabs to the Right, Move to New Window and Copy File Path.
  Closing several tabs asks about each modified one in turn, and Cancel stops the whole thing.
- Open the text menu from the keyboard with the Menu key or Shift+F10. On macOS, Control+click also opens it.

**Editing**
- Cut, Copy and Paste with the system clipboard (Ctrl+X/C/V or the Edit menu), on Windows, macOS, X11 and
  Wayland, including GNOME
- Find, Find Next and Find Previous, with an optional Match case
- Replace and Replace All. Replace All undoes as a single step.
- Undo and redo, grouped by word
- Word selection by double-click, line selection by triple-click, Shift+click to extend a selection

**View**
- Font family and size picker with search and a live preview. There is no rich formatting.
- Word wrap toggle. With wrap off, scroll horizontally with Shift+wheel.
- Zoom
- A status bar you can hide, showing line and column, selection size, zoom, line ending and encoding
- Light or dark theme, following the system
- URL detection for `http(s)://`, `ftp://`, `mailto:` and `www.`. URLs are coloured and underlined on hover,
  the status bar shows where a link goes, and Ctrl+click (Cmd+click on macOS) opens it in the default browser or
  mail app. Links that may be disguised as another address are shown in red and need confirming before they
  open (see Security).

**Printing**
- Pages are laid out with the same text engine as the editor, in your chosen font, with page numbers.
- Windows: the native print dialog, printing through GDI
- macOS and Linux: a PDF is sent to the default CUPS printer with `lp`. If there is no printer, the PDF opens in your viewer.

**Settings**
- Font, size, word wrap, status bar and window size are remembered in the platform config directory
  (`%APPDATA%`, `~/Library/Application Support`, or `$XDG_CONFIG_HOME`).

## Keyboard shortcuts

On macOS, use **Cmd** wherever this table says **Ctrl**, except for Ctrl+Tab and Ctrl+Shift+Tab, which use Control there too. macOS also follows its own conventions for moving the cursor; see the notes below the table.

| Action | Shortcut |
|---|---|
| New tab | Ctrl+N or Ctrl+T |
| Open | Ctrl+O |
| Close tab | Ctrl+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab, or Ctrl+PageDown / Ctrl+PageUp |
| Go to tab 1–8 / last tab | Ctrl+1 … Ctrl+8 / Ctrl+9 |
| Save | Ctrl+S |
| Save As | Ctrl+Shift+S |
| Print | Ctrl+P |
| Exit | Ctrl+Q |
| Undo | Ctrl+Z |
| Redo | Ctrl+Y or Ctrl+Shift+Z |
| Cut / Copy / Paste | Ctrl+X / Ctrl+C / Ctrl+V |
| Select all | Ctrl+A |
| Find | Ctrl+F |
| Replace | Ctrl+H |
| Find next | F3 or Ctrl+G |
| Find previous | Shift+F3 or Ctrl+Shift+G |
| Word wrap | Alt+Z |
| Zoom in / out / reset | Ctrl+= / Ctrl+- / Ctrl+0, or Ctrl+mouse wheel |
| Open the File / Edit / Format / View menu | Alt+F / Alt+E / Alt+O / Alt+V |
| Move between menu items | Arrow keys (Right opens a submenu, Left closes it), Enter to choose, Esc to close |
| Move by word | Ctrl+Left / Ctrl+Right |
| Start / end of line | Home / End |
| Start / end of document | Ctrl+Home / Ctrl+End |
| Page up / down | PageUp / PageDown |
| Extend selection | Shift + any movement key, or Shift+click |
| Delete previous / next word | Ctrl+Backspace / Ctrl+Delete |
| Insert a tab | Tab |
| Clear selection, or close the find bar | Esc |
| Open a URL | Ctrl+click |
| Right-click menu at the cursor | Menu key or Shift+F10 (arrows to move, Enter to choose, Esc to close) |

**Find bar:** Enter finds the next match and Shift+Enter the previous one. In the Replace field, Enter replaces
the current match. Tab switches between the Find and Replace fields, and Esc closes the bar.

**Font dialog:** Type to filter the list. Up, Down, PageUp and PageDown change the selection, Tab moves to the
size field, Enter applies and Esc cancels.

**macOS:** Option+Left/Right moves by word, Cmd+Left/Right moves to the start or end of the line, and
Cmd+Up/Down moves to the start or end of the document. The Alt+letter shortcuts for menus and word wrap are not
available on macOS, because Option types characters there.

## Security

A text editor opens files from anywhere, so their contents are treated as untrusted:

- **Links:** only `http`, `https`, `ftp` and `mailto` links can be opened, and only by an explicit Ctrl+click.
  `file://` and other schemes aren't treated as links, so a document can't make you open or run a local
  program. A link ends at any invisible formatting character, such as a zero-width space or a bidi override,
  so text can't hide or reorder part of the target.
- **Disguised links:** each link's host name is checked for tricks that make it look like a different site,
  following the rules browsers use (Unicode UTS #39 "highly restrictive" profile, simplified, plus the IDNA
  bidi rule). These are flagged:
  - Letters from different alphabets mixed in one name, such as Cyrillic `а` in `аpple.com`. The
    combinations used by Chinese, Japanese and Korean with Latin are allowed.
  - Names written entirely in Cyrillic or Greek letters that look Latin (`аррӏе.com`), unless under a
    Cyrillic or Greek domain such as `.ru` or `.gr`.
  - Latin lookalikes (dotless `ı`, IPA `ɡ`, small capitals), combining accents, and fullwidth letters.
  - Characters that imitate `/`, `.`, `:` or `@`, which make `paypal.com⁄signin.evil.example` one long
    name on `evil.example`.
  - A name before `@`, as in `https://google.com@evil.example/`, where the site is really `evil.example`.
  - IP addresses written as a single number or in hex or octal, percent-encoded host names, and names mixing
    right-to-left and left-to-right letters.
  - The same checks on the domain of `mailto:` links.

  A flagged link is drawn in red and always underlined. Hovering shows why it was flagged and its real
  address in ASCII (punycode, such as `xn--pple-43d.com`), naming the impostor characters and their code
  points. Ctrl+click asks for confirmation, with Cancel as the default. International names that pass are
  shown as written, with their ASCII form alongside. Every link is handed to the browser in ASCII form, so
  the browser receives exactly the address you were shown.
- **Opening links:** links are handed to the OS directly (`ShellExecuteW` on Windows, `open` on macOS,
  `xdg-open` on Linux) as a single argument, never through a shell.
- **Saving:** the temp file is created exclusively with an unpredictable name, so a symlink planted next to
  the file can't redirect the write. It's readable only by you until it replaces the original, and then it
  takes the original's permissions. Files owned by another user, or with extra hard links, are written in place
  so they keep their owner and links.
- **Printing:** the PDF sent to the printer is created the same way, private and with an unpredictable name.
- **Opening files:** only regular files are opened. A FIFO or a device such as `/dev/zero` is refused rather
  than hanging the app or filling memory.
- **Clipboard:** pasting never blocks the editor (see "Pasting never freezes" below). On Wayland, text pasted
  from another app is capped at 256 MB, so an app that never stops sending can't exhaust memory. At most 8
  transfers of our copied text
  run at once, so an app that keeps asking and never reads can't pile up threads. All shared Wayland
  objects are used under one lock, because libwayland forbids destroying an object on one thread while
  another uses it.
- **Closing tabs safely:** save prompts and tab menu actions refer to tabs by a stable id, not their
  position. A tab arriving from another window, or a dropped file, can't redirect "Don't Save" or "Close" onto a
  different tab. Tabs can't be dropped into a window while it shows a dialog, and a cancelled or failed save
  abandons the whole close or exit, rather than leaving earlier "Don't Save" choices armed for later.
- **Dependencies:** `cargo audit` reports no known vulnerabilities. CI uses read-only permissions and runs
  actions pinned to commit SHAs.

## Intentional optimizations

These choices keep memory use, CPU use and dependencies small. Some trade away polish, and those trade-offs are deliberate.

- **The cursor doesn't blink.** The event loop uses `ControlFlow::Wait` and only repaints after input,
  so an idle window uses no CPU and never wakes up on a timer.
- **Native Wayland clipboard.** On Wayland the app talks to the compositor's core clipboard protocol directly,
  which works on every compositor including GNOME, instead of relying on X11 compatibility (Xwayland) or the
  `wlr-data-control` extension. Clipboard requests from other apps are answered on a small dedicated thread, so
  they're served instantly, whatever the editor is doing. X11, Windows and macOS use arboard, built without its
  image support.
- **Pasting never freezes.** Reading the clipboard can mean waiting on the app that copied, which may be slow
  or stuck. Pastes are read on a background thread on every platform, while the editor keeps working; the status
  bar shows "Pasting…". The text is inserted when it arrives, at the cursor of the tab it was pasted into,
  even if that tab has since moved to another window. After 10 seconds without an answer the paste is abandoned
  with a message. Text copied within Plainpad is pasted immediately.
- **Tab dragging on Wayland without a toolkit.** Wayland never tells apps where their windows are, so
  dragging a tab to another window uses Wayland's own drag-and-drop protocol, as browsers do. The app binds
  a data device on winit's existing connection, starts a drag with a private MIME type, and the compositor
  reports which of our windows it was dropped on. X11, Windows and macOS use the pointer position instead.
- **No GTK.** On Linux, file dialogs use the XDG desktop portal (via `rfd`) and the clipboard talks to
  X11/Wayland directly (`arboard`). This avoids loading GTK, which adds about 30 MB of memory.
- **The GUI is drawn by the app itself.** The menus, find bar, dialogs and status bar are drawn by the app
  rather than taken from a widget toolkit, so the same code runs on every platform with no toolkit underneath.
- **No GPU.** Pixels go to the window through `softbuffer`. No wgpu, OpenGL or Vulkan drivers are loaded,
  and GPU stacks typically use 60–150 MB of memory on their own.
- **Lean frame buffers on Wayland.** A software-drawn window needs a pixel buffer the size of the window,
  which is 16 MB at 2560×1600. softbuffer always keeps two, rounded up to a power of two and never shrunk. On
  Wayland the app presents frames itself instead:
  - It uses one buffer, adding a second only while the compositor still holds the first.
  - Buffers are the exact window size and are dropped on resize.
  - About a second after the last frame, the memory of any buffer the compositor has handed back is returned
    to the kernel.

  An idle window therefore keeps a single frame's worth of memory: typing no longer leaves a second buffer
  behind, and shrinking the window frees memory. The release runs on a thread that sleeps until there's been
  activity, so an idle editor still never wakes up. X11, Windows and macOS still use softbuffer.
- **Repaints only when something changes.** Moving the mouse repaints only if the hovered URL, the menu
  highlight or a drag actually changes.
- **The document is stored in a rope (`ropey`), outside the layout engine.** The whole text lives in a
  compact rope, which adds about 10% over the file size. Only a window of a few hundred lines around the
  viewport is loaded into cosmic-text. That matters because cosmic-text spends about 330 bytes of
  bookkeeping on every line it holds, before any layout. As you scroll, lines are swapped in and out of
  the window, and lines already laid out are reused. The cursor, selection and undo history are stored
  in whole-document positions, so they work across the entire file. Search, Replace All, the selection
  count and saving all work on the rope directly.
- **Only visible text is laid out.** Lines are shaped when they scroll into view, and the shaping data
  for lines scrolled past is discarded.
- **Files are decoded as they're read.** Files are read 64 KB at a time and decoded into the rope as they
  arrive, with line endings normalised along the way. The whole file is never in memory as raw bytes or as a
  single string, so loading peaks at the same memory as the finished document. If a file turns out not to
  be valid UTF-8, it's read again from the start as Windows-1252.
- **Fonts are memory-mapped.** The system font database maps font files into memory rather than reading
  them into the heap, and loads only font metadata at startup.
- **UI text is cached.** Labels such as menus and buttons are shaped once and reused. The cache is capped at 512 entries.
- **Printing uses its own short-lived memory.** The document is laid out 256 lines at a time and pages are
  rendered one by one into a separate glyph cache, so printing a huge file never holds its whole layout,
  and high-resolution print glyphs never stay in the editor's memory. On macOS and Linux the PDF is
  written to disk page by page as it's produced.
- **Undo stores changes, not copies.** Each undo step records only what changed, never a snapshot of the
  document, and consecutive keystrokes are merged into one step per word. Replace All is a single step.
- **Case-insensitive search doesn't copy the document.** Text is compared character by character in place,
  instead of lowercasing a copy of each line.
- **Small dependencies where possible.** Windows-1252 conversion uses a 32-entry table instead of
  `encoding_rs`, and settings are a plain `key=value` file instead of using serde.
- **Release build settings.** The release build uses `opt-level = 3`, full LTO, `codegen-units = 1`,
  `panic = "abort"` and stripped symbols.

## Resource use

Measured on Linux/X11 with a release build:

| | |
|---|---|
| Binary size | ~5 MB |
| Memory with a small file open | ~18 MB, of which ~14 MB is shared libraries and memory-mapped fonts and under 2 MB is the app's own heap |
| 27 MB, 400k-line file | opens in ~0.08 s and uses ~48 MB, of which ~33 MB is the app's heap. Peak memory while loading is the same ~48 MB. (Before the rope and window change it used ~175 MB.) |

## Known bugs

None open right now.

Recently fixed:
- Scrolling could go blank for a moment, especially in files with long lines, such as a PDF opened as
  text. Lines loaded into the layout window were sometimes never laid out until a later scroll event.
- When deleting text made a scrollable file short enough to fit the window, the top of the file was
  partly hidden. A leftover part-line scroll offset wasn't being cleared.

## TODO

- [ ] Option to show non-printable characters (tabs, spaces, control characters and other invisibles) as visible placeholders
- [ ] Rename the project. "Plainpad" collides with existing projects, and so does the author's old name
      "QuickJot". Candidates, with the number of GitHub repositories whose names contain each one (checked
      November 2026; trademarks, domains, app stores and crates.io not yet checked):

      | Name | Repos | Notes |
      |---|---|---|
      | **Quiljot** | 0 | A nod to QuickJot: a quill plus a jot. Current favourite. |
      | Jotwright | 0 | Jot + wright (a maker), like playwright |
      | Stillpad | 0 | Quiet: no AI, nothing running in the background |
      | Scrivlet | 0 | A small scrivener (a scribe) |
      | Tersepad | 0 | A lean editor |
      | Sparepad | 0 | Spare as in lean |
      | Barepad | 1 | Bare text, nothing else |
      | Tacitpad | 1 | Quiet and unobtrusive |

      To avoid: Featherpad and Leafpad (both existing Linux text editors), and Jotter, Quire, Inkwell,
      Vellum, Scrawl, Plume and Notary (each in hundreds to thousands of repository names).
- [x] Reduce memory for large files: the document is now held in a rope and only a window of lines is laid out
- [x] Lower the peak memory while loading: files are decoded 64 KB at a time straight into the rope
- [ ] Handle very long single lines, such as minified files. cosmic-text shapes a whole line at once, so a
      multi-megabyte line is slow to display.
- [x] Open several documents at once, in tabs
- [ ] Restore the open tabs when the app restarts
- [ ] Autosave unsaved changes for crash recovery (the release build aborts on a panic, losing unsaved work)
- [x] Drag a tab out into its own window, or onto another window (including on Wayland)
- [ ] On Wayland, make a dragged-out window follow the pointer while dragging, as Chrome does, using the
      `xdg-toplevel-drag` protocol where the compositor supports it (KDE, recent GNOME). Show a tab image while
      dragging.
- [ ] Use the native macOS menu bar at the top of the screen instead of the in-window menu bar
- [ ] Add a print dialog on macOS and Linux (page setup, choice of printer, copies)
- [ ] Add a horizontal scrollbar when word wrap is off
- [ ] Highlight all matches while the find bar is open
- [ ] Support find/replace across line breaks, and regular expressions
- [ ] Go to line (Ctrl+G, as in Notepad; this conflicts with the current Find Next binding)
- [x] Add right-click menus for the text and the tabs
- [ ] Auto-scroll on a timer while drag-selecting outside the view (today it only scrolls while the mouse moves)
- [ ] Let the Tab key move keyboard focus between controls in the find bar and dialogs
- [ ] Repaint only the changed parts of the window, using softbuffer damage regions, instead of the whole window
- [ ] Accessibility support for screen readers (AccessKit)
- [ ] Detect BOM-less UTF-16 files
- [ ] Notice when the open file changes on disk and offer to reload it
- [ ] Optional blinking cursor, switched off by default
- [ ] Test on real Windows and macOS hardware. So far only Linux/X11 has been run; the other two have only
      been compile-checked and built in CI.

## Building

```
cargo build --release
```

On Linux you need the `libxkbcommon-x11` and `fontconfig` development packages.
CI builds and tests on Windows, macOS and Linux, and uploads the binaries as artifacts.
