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
- Big files never freeze the window. Opening, reloading, saving, Find and Replace All on a document of 4 MB or
  more run in the background, with progress in the status bar, while you keep using other tabs and windows.
  The document is read-only until it has loaded, and while it's being saved or replaced; quitting waits for
  saves in progress. Opening a file of 1 GB or more asks first.
- Closing a tab with unsaved changes asks whether to save it. Closing a window asks about each of its modified
  tabs, and Exit asks about every modified tab in every window, in turn. Cancel stops the close or exit. The window title shows `*`
  while the file is modified, and it clears if you undo back to the saved state.

**Protecting your work**
- **Crash recovery:** about 2 seconds after you stop typing, every document with unsaved changes is
  snapshotted to a private folder (`<settings dir>/plainpad/recovery`, readable only by you). If the editor
  crashes or is killed, the next start reopens those documents as modified tabs, pointing at their original
  files. Snapshots are deleted as soon as a document is saved, closed or undone back to its saved state, and
  the folder is removed on a normal exit. Each running instance keeps its own locked folder, so several
  instances never take each other's snapshots. The timer only runs while there are unsnapshotted edits, so an
  idle editor still never wakes up. Documents of 1 MB or more are snapshotted as an edit log on top of the
  saved file (or one full copy), so each pause in typing writes only what changed. If such a file is changed
  by another program and the editor then crashes before its next snapshot, its edits can't be rebuilt; the
  next start says so rather than restoring the wrong text.
- **Files changed by other programs:** when a window regains focus, its tabs' files are checked. A tab with
  no unsaved changes reloads silently, keeping its scroll position. A tab with unsaved changes asks whether to
  reload or keep your version (keeping is the default). A deleted file is reported and the tab marked modified.
  Saving over a file that changed since you opened it asks first (Cancel is the default).

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
  takes the original's permissions. A new file is created exclusively too, so a symlink planted at its name
  is never followed. Files owned by another user, or with extra hard links, are written in place
  so they keep their owner and links.
- **Printing:** the PDF sent to the printer is created the same way, private and with an unpredictable name.
- **Opening files:** only regular files are opened. A FIFO or a device such as `/dev/zero` is refused rather
  than hanging the app or filling memory.
- **Clipboard:** pasting never blocks the editor (see "Pasting never freezes" below). On Wayland, text pasted
  from another app is capped at 256 MB, so an app that never stops sending can't exhaust memory, and reading
  it gives up after 10 seconds, so an app that never finishes can't keep a thread and its buffer waiting. At most 8
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
- **Freed memory goes back to the OS.** glibc's allocator keeps freed memory for reuse, so after closing a large
  document the process would otherwise stay at its peak size. The app calls `malloc_trim` after closing tabs or
  windows, printing, Replace All, large undo/redo steps, and clearing caches. Closing a 27 MB file now drops
  the app's own memory from 33 MB to 1.8 MB.
- **The allocator is tuned for a long session.** The soak test found the process's memory creeping up
  (33 MB to 81 MB in six minutes of pasting, editing and reloading) although nothing was leaking: valgrind
  showed what was still allocated grew by only 1.3 MB. glibc raises its threshold for giving large blocks
  their own mapping after freeing one, up to 32 MB, so big temporary buffers (pasted text, file data) end up
  inside the main heap among small long-lived allocations, and the gaps they leave can't be returned. Every
  short-lived thread (clipboard reads, file jobs) can also leave behind its own allocator arena. At startup the
  app now fixes that threshold at 128 KB, so large blocks go back to the OS as soon as they're freed, and
  allows at most two arenas.
- **Bounded glyph cache.** Rendered glyph images are cached, but the cache is cleared when the zoom or font
  changes (old sizes are never drawn again), and whenever it passes 8,192 glyphs. Zooming through 30 sizes
  on CJK text now uses 4.5 MB instead of 13.5 MB.
- **Compact, bounded undo.** Replace All is stored as one record: match positions, the removed text, and one
  copy of the replacement, about 25 bytes per match instead of ~140. 246,706 replacements cost 10 MB instead of
  34 MB, and undo/redo of it allocates nothing extra. Undo and redo history together are capped at 64 MB. Past
  that, the oldest undo steps are dropped (the latest is always kept), and a document whose saved state falls
  out of history correctly stays marked as modified.
- **Long lines are split for display.** cosmic-text lays out a line as a whole, so a minified file with a
  5-million-character line took 5 s to open, and each keystroke on a 100k+ character line took 100–200 ms.
  Lines over 2,048 bytes are now shown as display segments, broken after a space or punctuation where
  possible and never inside a character. Only the segments on screen are laid out, and typing re-lays out
  just the segment being edited. A 5-million-character line now opens in ~55 ms, and a keystroke costs ~3 ms.
  Like classic Notepad (which did this at 1,024 characters), such lines wrap at a segment boundary even with
  word wrap off. A link that straddles a segment boundary isn't recognised as one.
- **Repaints only when something changes.** Moving the mouse repaints only if the hovered URL, the menu
  highlight or a drag actually changes.
- **The document is stored in a rope (`ropey`), outside the layout engine.** The whole text lives in a
  compact rope, which adds about 10% over the file size. Only a window of lines around the viewport is
  loaded into cosmic-text: a few hundred normal lines, or just enough long ones to fill the view and its
  margins. That matters because cosmic-text spends about 330 bytes of
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
- **Crash-recovery snapshots of big documents log edits.** Rewriting a 300 MB document after every pause
  in typing wrote 290 MB each time, and kept a 284 MB copy in the recovery folder. From 1 MB up, the first
  snapshot either names the file on disk (with its modified time and size), when the file still holds the
  saved text, or writes one full copy; after that, each snapshot appends just the edits made since. The same
  300 MB document now writes under 1 KB per pause, and its recovery folder holds 16 KB. The text is written
  whole again only after a Replace All, once the log reaches a quarter of the document, or after a failed
  write. Smaller documents are still written whole each time, which is simplest and costs little.
- **Undo stores changes, not copies.** Each undo step records only what changed, never a snapshot of the
  document, and consecutive keystrokes are merged into one step per word. Replace All is a single step.
- **Case-insensitive search doesn't copy the document.** Text is compared character by character in place,
  instead of lowercasing a copy of each line. Only positions where the first character could match are
  tried, which for an ASCII letter is a plain byte comparison.
- **Big documents are handled in the background, in one pass.** On a 300 MB, 3.3-million-line file:

  | | Before | Now |
  |---|---|---|
  | Open | 1.8 s before the window appeared | window in 0.04 s; text ready after 0.65 s, window usable throughout |
  | Save | 0.8 s frozen, memory peak 930 MB | 0.4 s in the background, no extra memory |
  | Find with no match | 4 s frozen | 0.3 s in the background |
  | Replace All, 3.3M matches | ~8.5 s frozen | ~1 s in the background |
  | Undo / redo of that | ~4 s / 1.3 s | 0.45 s / 0.58 s (still on the window thread) |

  Saving streams the rope through the encoder into a buffered file instead of building the text and then
  its encoded bytes. Search scans the rope in 1 MB windows instead of copying out each line. Replace All, and
  undo and redo of a big one, rebuild the text in a single walk through the rope instead of editing it once
  per match. Replace All briefly needs the old and new text at once (807 MB peak in that test).
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
| 300 MB, 3.3M-line file | window in 0.04 s, loaded in 0.65 s, ~350 MB; saving adds nothing to that |

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
- [x] Handle very long single lines, such as minified files (see "Long lines" under optimizations)
- [x] Open several documents at once, in tabs
- [ ] Restore the open tabs when the app restarts
- [x] Autosave unsaved changes for crash recovery
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
      (deferred; see C3 below for measurements)
- [ ] Accessibility support for screen readers (AccessKit)
- [ ] Detect BOM-less UTF-16 files
- [ ] Shape and run the bidi algorithm across a whole long line, not per 2 KB display segment: today a
      ligature or Arabic joining across a segment boundary, or right-to-left runs crossing one, may draw wrongly
- [ ] Decide what the status bar's column means: it counts code points, so a decomposed accented letter counts
      as 2, a tab as 1 (not to the next tab stop) and a wide CJK character as 1
- [ ] Check what happens with a file that is valid UTF-8 for its first 64 KB and then isn't (it should be
      re-read as Windows-1252, not left half-decoded)
- [x] Notice when the open file changes on disk and offer to reload it
- [ ] Optional blinking cursor, switched off by default
- [ ] Test on real Windows and macOS hardware, especially the URL launcher (`ShellExecuteW`) and printing.
      So far Linux (X11 and Wayland) has been run; the other two have only been compile-checked and built in CI.

**Performance**
- [x] Load, save, search and Replace All on very large files in the background with progress, instead of
      blocking the window; warn before opening something enormous
- [ ] Print in the background too (layout needs the font system, which lives on the window thread)
- [ ] Undo and redo of a huge Replace All in the background (0.5 s on the window thread for 3.3M matches in 300 MB)
- [ ] Measure startup on systems with thousands of fonts; consider caching the font database if it's slow
- [x] Crash-recovery snapshots rewrote the whole document 2 s after each pause in typing (290 MB per pause for
      a 300 MB file). Big documents now log their edits instead: under 1 KB per pause
- [ ] Restore crash-recovered documents in the background too (a 300 MB one takes ~0.5 s at startup)
- [x] A benchmark job in CI that fails on regressions: startup time, idle memory, memory with a 30 MB file and
      after closing it, keystroke time on a long line, binary size
- [x] A long-session soak test: an hour of scripted random edits, undos, tab moves and pastes, then check
      memory returns to its starting level
- [ ] Run the soak test on Wayland too (it drives X11 only; the Wayland frame presenter and clipboard code
      aren't covered)

From a code review (November 2026), memory:
- [x] **M1.** The layout window held about 200 document lines in full, so a file of a few hundred 1 MB lines
      (minified bundles, wide CSV or JSON) copied ~200 MB into cosmic-text on top of the rope, plus ~330 bytes
      per 2 KB segment. It's now bounded by display segments: every segment takes at least one row, so lines
      are added around the view only until there are enough segments for it and its margins. Normal files get
      the same window as before; in a test file of 300 lines of 20 KB, the window went from 1.7 MB to under
      600 KB, and with 1 MB lines it holds a few lines instead of ~200. A single huge line is still loaded
      whole (see "Long lines").
- [x] **M2.** Hovering a link copies its whole line on every repaint (`draw_status` calls `doc.line`): 5 MB
      per frame on a 5 MB line. Take just the link's bytes from the rope.
- [x] **M3.** A Wayland paste briefly needs twice its size: `String::from_utf8_lossy(&buf).into_owned()`
      always copies, so a 256 MB paste peaks at 512 MB. Try `String::from_utf8(buf)` first, which reuses the
      buffer.
- [x] **M4.** The crash-recovery edit log kept text it didn't need. Each edit's inserted text was copied
      before the 16 MB cap was checked, so a big paste was copied only to be thrown away, and if the recovery
      folder couldn't be created, nothing ever collected the log. Now the size is checked first, and there's
      no log without crash recovery. Documents under 1 MB still keep one, which the writer ignores; it's taken
      at every autosave, so it only ever holds about 2 seconds of typing.
- [x] **M5.** The UI label cache filled with one-off strings: every new "Ln X, Col Y", progress percentage and
      IME composition got a shaped buffer. At 512 entries the whole cache was cleared, menu labels included,
      and they were reshaped. Now text that keeps changing (the status message, progress, IME composition, a
      field's width up to the caret) is drawn without the cache. The cache keeps two generations of 256, so
      labels drawn every frame are never reshaped and unused ones age out. It's still capped at 512 entries:
      fewer would make a window with a few hundred tabs reshape every tab name on every frame.
- [x] **M6.** `word_at`, `clamp` and `make_lines` copied the whole line: double-clicking in a 5 MB line copied
      it and segmented words from its start. `clamp` now works on the rope directly, `word_at` looks 256 bytes
      either side of the cursor (a longer word is cut at that edge), and `make_lines` copies a long line from
      the rope one segment at a time.

From the same review, CPU:
- [x] **C1.** Skip glyphs entirely outside the text area. With word wrap off, every glyph of a 2 KB segment
      goes through the glyph cache, and each pixel is then rejected by the clip test.
- [x] **C2.** Link checking, punycode and script checks included, ran on every visible link every frame. Its
      answers are now remembered per link (up to 256 links of up to 512 bytes, about 128 KB at most). Finding
      the links still runs per frame: it's a single pass over the line since S1.
- [ ] **C3.** Repaint only the changed parts of the window (see the damage-regions item above). Deferred
      (November 2026): measured, it isn't worth it yet. A full frame at 2560×1600 (release build, Linux)
      takes about 1.2 ms to draw: 0.55 ms to clear the window, 0.68 ms for a screen of text (57 rows), plus
      0.05 ms to lay out a keystroke. Typing changes the text area, status bar and tab bar, almost the whole
      window, so saving most of that needs diffing the text area row by row and tracking what each buffer
      last showed, at the risk of stale pixels, for about 1 ms per keystroke. An idle window already uses no
      CPU. Not measured: the cost of sending the frame to the screen. If that turns out to be large on real
      hardware, the cheaper first step is to present only the damaged region, without changing the drawing.
- [x] **C4.** Pastes are timed out by the event loop's timer (`WaitUntil`, as autosave does) instead of a
      thread per paste that only slept for 10 s.

**Security**
- [ ] Coverage-guided fuzzing (`cargo-fuzz`) of the decoder, link checker, search, and document edit/undo code
- [ ] `cargo-deny` in CI (advisories, licences, duplicate or banned crates); pin the Rust toolchain
- [ ] Sanitise file names shown in the title and tabs (bidi overrides and invisible characters could disguise them)

From the code review (November 2026):
- [x] **S1.** A crafted file can slow every repaint. When `find_urls` trims trailing `)` from a link, it
      recounts every bracket in the link for each character removed, so the cost grows with the square of
      its length. A file of `http://a)))))…` lines costs millions of comparisons per visible line, on every
      frame and mouse move. Count the brackets once and adjust the count while trimming.
- [x] **S2.** If the app that owns the clipboard never closed the pipe, the Wayland paste reader thread blocked
      forever with a buffer of up to 256 MB. The 10 s timeout only gave up in the UI, so each further paste
      leaked another thread. The reader now polls the pipe with the same 10 s deadline and then gives up,
      freeing what it read.
- [x] **S3.** Saving a new file follows a symlink planted at its path between the check and the open
      (`write_in_place` uses `create` + `truncate`), for example in `/tmp`. Use `create_new` when the file
      didn't exist.
- [x] **S4.** `open_regular` checked the path's type, then opened the path. A file swapped for a FIFO in between
      froze the window, since files under 4 MB are read on the window thread. On Unix it now opens with
      `O_NONBLOCK`, and on every platform it checks the type of what it actually opened.
- [x] **S5.** `tests/bench/__pycache__/bench.cpython-313.pyc` is committed. Remove it and add `__pycache__/`
      to `.gitignore`.

**Suggested order for the review items**
1. Quick, low-risk fixes (a few lines each, no change in behaviour): S1, S3, S5, M2, M3, M4, C1
2. Moderate: S2, S4, M5, M6, C2, C4
3. Larger design changes: M1 (a byte budget for the layout window) and C3 (damage regions)

**Quality**
- [x] Run the GUI smoke tests in CI: Xvfb for X11 and headless sway for Wayland, scripted with xdotool and
      a virtual pointer
- [ ] GUI tests for Windows and macOS (CI only builds and runs unit tests there)
- [ ] Show an error dialog instead of crashing if a window or graphics surface can't be created
- [ ] On a panic, write crash-recovery snapshots at once from a panic hook, instead of losing up to 2 s of typing
- [ ] Packaging and releases: versioning, installers (MSI, DMG, AppImage/Flatpak), code signing

## Building

```
cargo build --release
```

On Linux you need the `libxkbcommon-x11` and `fontconfig` development packages.
CI builds and tests on Windows, macOS and Linux, and uploads the binaries as artifacts.

### GUI tests

`tests/gui/run.sh` drives the release build under X11 (Xvfb) and Wayland (headless sway) with simulated
keyboard and mouse input, and checks real outcomes: file contents, the clipboard, window titles and exit
codes. It covers typing and saving, the clipboard both ways, the unsaved-changes prompt, dragging tabs into
new and existing windows, crash recovery after `kill -9`, reloading a file changed on disk, opening and
editing a 5 MB single-line file, opening, saving and searching a 60 MB file in the background, a paste from
a clipboard owner that never answers, and a clean exit. Each test runs a fresh instance with its own
settings folder. CI runs it on every push; on failure it keeps a screenshot and the app's log in
`target/gui-test-output`.

```
sudo apt-get install xvfb x11-utils xdotool xclip imagemagick sway wtype wl-clipboard grim
cargo build --release
cargo build --release --manifest-path tests/gui/testkit/Cargo.toml
tests/gui/run.sh            # or: tests/gui/run.sh clipboard drag
```

### Benchmarks

`tests/bench/bench.py` measures the release build under Xvfb: binary size, startup time, memory (in total and
the app's own heap) with an empty document, with a 30 MB file open and after closing it, how long that file
takes to open, and CPU time per keystroke on a normal line and on a 5-million-character line. CI runs it after
the GUI tests and puts the table in the job summary. It fails if memory or binary size is more than 20% worse
than `tests/bench/baseline.json`, or a timing more than 3 times worse; timings on shared CI machines vary too
much for anything tighter. Each metric also has a small absolute allowance, so tiny values don't fail on noise.

The baseline holds numbers from the CI runner, not a developer machine (memory there depends on the fonts
installed). After an intended change, copy the new numbers from the job summary into the baseline. Locally,
`tests/bench/bench.py --update` writes your own machine's numbers as the baseline, for comparing before and
after a change.

### Soak test

`tests/soak/soak.py` drives the release build for a long session (an hour by default) with seeded random
actions: typing in several scripts, deleting, moving and selecting, scrolling, undo and redo bursts, copy, cut
and paste, pastes of up to 3 MB from another program, opening, switching and closing tabs, Find, Replace All,
zoom, word wrap, resizing, right-click menus, saving, and dragging tabs out into new windows and back. It starts
with four files: two small ones, a 6 MB one (handled in the background) and a 300 KB single line.

Every few minutes it returns the app to its starting state: tabs dragged back, untitled tabs discarded, the
files saved and then rewritten with their original text (so they reload, clearing undo history), and zoom,
word wrap, window size and clipboard reset. It then records memory still allocated (glibc's `malloc_stats`,
read through gdb), the process's own memory, CPU use while idle and how fast a new tab opens. It fails if:

- the app crashes or stops responding,
- it uses CPU while idle,
- memory still allocated ends more than 20% (at least 4 MB) above where it was early in the session (a leak),
- or the process's own memory ends more than 50% (at least 10 MB) above it. This one is looser, because what
  the allocator holds on to wanders by several MB between checkpoints.

"Early" is the median of the first half of the checkpoints, since caches fill up during the first part of a
session. "The end" is the lowest of the last three checkpoints: one checkpoint can catch work still settling,
such as a big paste landing, and show a spike that the next one doesn't, while leaked memory never comes back.
In an hour-long CI run, memory in use settled at 15–17 MB after about half an hour, with spikes of 25 and
34 MB that went away again.

It runs nightly in CI (`.github/workflows/soak.yml`), and on demand from the Actions tab with a chosen length
and seed. The results table goes to the run summary; the action log, the app's log and, on failure, a
screenshot are kept as an artifact. To reproduce a run, pass its seed.

```
sudo apt-get install xvfb xdotool xclip gdb
cargo build --release
tests/soak/soak.py --minutes 10 --seed 1234
```

`tests/gui/testkit` is a small helper for the Wayland tests: a virtual pointer (sway has no input devices
when headless) and a clipboard owner that never sends its data.
