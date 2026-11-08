# Plainpad

A small, fast plain-text editor for Windows, macOS and Linux, written in Rust.
It has no AI features and no formatting.

The UI is drawn in software, without a GPU: [winit](https://github.com/rust-windowing/winit)
creates the window, [softbuffer](https://github.com/rust-windowing/softbuffer) puts pixels on it,
and [cosmic-text](https://github.com/pop-os/cosmic-text) shapes text, including bidirectional and complex scripts.
File dialogs come from [rfd](https://github.com/PolyMeilex/rfd), which uses the XDG portal on Linux, so GTK isn't needed.

## Features

- Choose the font family and size. There is no rich formatting.
- Detects line endings (CRLF / LF / CR), warns when a file mixes them, and lets you convert between them.
- Detects the encoding: UTF-8, UTF-8 with BOM, UTF-16 LE/BE with BOM, and Windows-1252 as a fallback.
  Every byte round-trips unchanged.
- Find, Find Next/Previous, Replace and Replace All, with an optional Match case.
- Unicode support: shaping, right-to-left and mixed-direction text, font fallback, and IME input with preedit display.
- Open, Save, Save As, Rename and Print.
  - Saving writes a temporary file and renames it over the original, so a failed save never leaves a truncated file.
  - Printing lays out pages with the same text engine as the editor. On Windows it uses the system print dialog
    (GDI). On macOS and Linux it sends a PDF to the default CUPS printer with `lp`. If there is no printer,
    it opens the PDF in your viewer.
- Word wrap (Alt+Z), zoom (Ctrl +/−/0 or Ctrl+wheel), and a status bar you can hide.
- Detects URLs (`http(s)://`, `ftp://`, `file://`, `mailto:`, `www.`). They are coloured, underlined when you
  hover over them, and Ctrl+click (Cmd+click on macOS) opens them in your browser.
- Word-level undo/redo. The window title marks unsaved changes, and the mark clears if you undo back to the saved state.
- Settings are saved in the platform config directory.

## Build

```
cargo build --release
```

On Linux you need the `libxkbcommon-x11` and `fontconfig` development packages.

## Resource use

Measured on Linux/X11, release build:

| | |
|---|---|
| Binary size | ~5 MB (stripped, LTO) |
| RSS for a small file | ~18 MB, of which ~14 MB is shared libraries and memory-mapped fonts |
| 27 MB / 400k-line file | opens in ~0.2 s, ~175 MB RSS |

Very large files cost about 330 bytes per line. That overhead comes from cosmic-text's per-line layout state.
The planned fix is to keep only the visible part of the document in the layout engine.
