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
  On Wayland the app can't see where its windows are on screen, so a dragged-out tab always opens a new window.
- New documents are named Untitled, Untitled 2, and so on.
- Opening a file that's already open switches to its tab. An empty, untouched Untitled tab is reused
  rather than left behind.
- Closing the last tab leaves a fresh empty one instead of quitting. In a second window, closing its last tab
  closes that window.

**Files**
- New Tab, Open, Save, Save As, Rename, Close Tab, Print
- The Open dialog can select several files. You can also drag files onto the window or pass several paths on
  the command line; each opens in its own tab.
- Saving writes a temporary file and renames it over the original, so a failed save never truncates your file.
  Symlinks and file permissions are kept.
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

**Editing**
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
- URL detection for `http(s)://`, `ftp://`, `file://`, `mailto:` and `www.`. URLs are coloured,
  underlined on hover, and Ctrl+click opens them in the system browser.

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

**Find bar:** Enter finds the next match and Shift+Enter the previous one. In the Replace field, Enter replaces
the current match. Tab switches between the Find and Replace fields, and Esc closes the bar.

**Font dialog:** Type to filter the list. Up, Down, PageUp and PageDown change the selection, Tab moves to the
size field, Enter applies and Esc cancels.

**macOS:** Option+Left/Right moves by word, Cmd+Left/Right moves to the start or end of the line, and
Cmd+Up/Down moves to the start or end of the document. The Alt+letter shortcuts for menus and word wrap are not
available on macOS, because Option types characters there.

## Intentional optimizations

These choices keep memory use, CPU use and dependencies small. Some trade away polish, and those trade-offs are deliberate.

- **The cursor doesn't blink.** The event loop uses `ControlFlow::Wait` and only repaints after input,
  so an idle window uses no CPU and never wakes up on a timer.
- **No GTK.** On Linux, file dialogs use the XDG desktop portal (via `rfd`) and the clipboard talks to
  X11/Wayland directly (`arboard`). This avoids loading GTK, which adds about 30 MB of memory.
- **The GUI is drawn by the app itself.** The menus, find bar, dialogs and status bar are drawn by the app
  rather than taken from a widget toolkit, so the same code runs on every platform with no toolkit underneath.
- **No GPU.** Pixels go to the window through `softbuffer`. No wgpu, OpenGL or Vulkan drivers are loaded,
  and GPU stacks typically use 60–150 MB of memory on their own.
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
- [x] Drag a tab out into its own window, or onto another window
- [ ] Use the native macOS menu bar at the top of the screen instead of the in-window menu bar
- [ ] Add a print dialog on macOS and Linux (page setup, choice of printer, copies)
- [ ] Add a horizontal scrollbar when word wrap is off
- [ ] Highlight all matches while the find bar is open
- [ ] Support find/replace across line breaks, and regular expressions
- [ ] Go to line (Ctrl+G, as in Notepad; this conflicts with the current Find Next binding)
- [ ] Add a right-click context menu
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
