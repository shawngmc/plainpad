//! Printing. Pages are laid out and rasterised with the same text engine as
//! the editor (so fonts, bidi and complex scripts print as they display), then
//! handed to the platform: GDI on Windows, CUPS `lp` (as PDF) elsewhere.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use std::io::{self, Write};

pub struct PageSetup {
    pub w_in: f32,
    pub h_in: f32,
    pub margin_in: f32,
    pub dpi: f32,
}

#[cfg_attr(windows, allow(dead_code))]
impl PageSetup {
    /// US Letter for North American locales, A4 otherwise.
    pub fn default_for_locale() -> Self {
        let lang = ["LC_ALL", "LC_PAPER", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        let letter = ["en_US", "en_CA", "es_MX", "fr_CA"].iter().any(|l| lang.starts_with(l));
        let (w_in, h_in) = if letter { (8.5, 11.0) } else { (8.27, 11.69) };
        Self { w_in, h_in, margin_in: 0.75, dpi: 300.0 }
    }
}

pub struct Job<'a> {
    pub lines: usize,
    pub line: &'a dyn Fn(usize) -> String,
    pub family: &'a str,
    pub font_pt: f32,
}

pub struct Raster<'a> {
    pub w: u32,
    pub h: u32,
    /// Row-major 8-bit grey, 255 = white.
    pub px: &'a [u8],
}

/// Document lines laid out per batch, so even huge documents only ever have
/// one batch of layout in memory.
const BATCH: usize = 256;

struct Pager<'a, F> {
    w: u32,
    h: u32,
    margin: f32,
    raster: Vec<u8>,
    footer: Buffer,
    footer_px: f32,
    page: usize,
    pages: usize,
    sink: &'a mut F,
}

fn plot(raster: &mut [u8], w: u32, h: u32, x: i32, y: i32, c: Color) {
    if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
        let p = &mut raster[(y as u32 * w + x as u32) as usize];
        *p = (*p).min(255 - c.a());
    }
}

impl<F: FnMut(usize, usize, Raster) -> io::Result<()>> Pager<'_, F> {
    fn emit(&mut self, fs: &mut FontSystem, cache: &mut SwashCache) -> io::Result<()> {
        let (w, h) = (self.w, self.h);
        self.footer.set_text(
            &format!("Page {} of {}", self.page + 1, self.pages),
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        self.footer.shape_until_scroll(fs, false);
        let raster = &mut self.raster;
        for run in self.footer.layout_runs() {
            let x = (w as f32 - run.line_w) / 2.0;
            let y = h as f32 - self.margin + self.footer_px * 0.5;
            for g in run.glyphs {
                let pg = g.physical((x, y + run.line_y), 1.0);
                cache.with_pixels(fs, pg.cache_key, Color::rgb(0, 0, 0), |gx, gy, c| {
                    plot(raster, w, h, pg.x + gx, pg.y + gy, c)
                });
            }
        }
        (self.sink)(self.page, self.pages, Raster { w, h, px: &self.raster })?;
        self.raster.fill(255);
        self.page += 1;
        Ok(())
    }
}

/// Lays out `job`, renders each page and passes it to `sink(page_index, page_count, raster)`.
pub fn render_pages(
    fs: &mut FontSystem,
    cache: &mut SwashCache,
    job: &Job,
    page: &PageSetup,
    mut sink: impl FnMut(usize, usize, Raster) -> io::Result<()>,
) -> io::Result<usize> {
    let dpi = page.dpi;
    let (w, h) = ((page.w_in * dpi).round() as u32, (page.h_in * dpi).round() as u32);
    let margin = (page.margin_in * dpi).round();
    let font_px = job.font_pt * dpi / 72.0;
    let lh = (font_px * 1.25).ceil();
    let content_w = (w as f32 - 2.0 * margin).max(dpi);
    let content_h = (h as f32 - 2.0 * margin - lh * 1.5).max(lh);
    let lines_per_page = ((content_h / lh).floor() as usize).max(1);

    let attrs = if job.family.is_empty() {
        Attrs::new().family(Family::Monospace)
    } else {
        Attrs::new().family(Family::Name(job.family))
    };
    let mut buf = Buffer::new(fs, Metrics::new(font_px, lh));
    buf.set_wrap(Wrap::WordOrGlyph);
    buf.set_size(Some(content_w), None);
    let layout = |buf: &mut Buffer, fs: &mut FontSystem, start: usize| {
        let end = (start + BATCH).min(job.lines);
        let text: Vec<String> = (start..end).map(job.line).collect();
        buf.set_text(&text.join("\n"), &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
    };

    // Pass 1: count visual lines so the footer can say "Page n of m".
    let mut total = 0;
    for start in (0..job.lines).step_by(BATCH) {
        layout(&mut buf, fs, start);
        total += buf.layout_runs().count();
    }
    let pages = total.div_ceil(lines_per_page).max(1);

    let footer_px = (9.0 * dpi / 72.0).round();
    let mut footer = Buffer::new(fs, Metrics::new(footer_px, footer_px * 1.3));
    footer.set_size(None, None);
    let mut pager = Pager {
        w,
        h,
        margin,
        raster: vec![255u8; (w * h) as usize],
        footer,
        footer_px,
        page: 0,
        pages,
        sink: &mut sink,
    };

    // Pass 2: render.
    let mut row = 0;
    for start in (0..job.lines).step_by(BATCH) {
        layout(&mut buf, fs, start);
        for run in buf.layout_runs() {
            let top = margin + row as f32 * lh;
            for g in run.glyphs {
                let pg = g.physical((margin, top + run.line_y - run.line_top), 1.0);
                let raster = &mut pager.raster;
                cache.with_pixels(fs, pg.cache_key, Color::rgb(0, 0, 0), |x, y, c| {
                    plot(raster, w, h, pg.x + x, pg.y + y, c)
                });
            }
            row += 1;
            if row == lines_per_page {
                pager.emit(fs, cache)?;
                row = 0;
            }
        }
    }
    if row > 0 || pager.page == 0 {
        pager.emit(fs, cache)?;
    }
    Ok(pager.page)
}

/// Streams rasterised pages into a minimal PDF (one Flate-compressed
/// greyscale image per page).
#[cfg_attr(windows, allow(dead_code))]
pub struct PdfWriter<W: Write> {
    out: W,
    pos: usize,
    offsets: Vec<usize>,
    page_ids: Vec<usize>,
}

#[cfg_attr(windows, allow(dead_code))]
impl<W: Write> PdfWriter<W> {
    pub fn new(mut out: W) -> io::Result<Self> {
        let header = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n";
        out.write_all(header)?;
        // Object 1 = catalog, 2 = page tree; both written at the end.
        Ok(Self { out, pos: header.len(), offsets: vec![0, 0], page_ids: Vec::new() })
    }

    fn obj(&mut self, id: usize, body: &[u8]) -> io::Result<()> {
        while self.offsets.len() < id {
            self.offsets.push(0);
        }
        self.offsets[id - 1] = self.pos;
        let head = format!("{id} 0 obj\n");
        self.out.write_all(head.as_bytes())?;
        self.out.write_all(body)?;
        self.out.write_all(b"\nendobj\n")?;
        self.pos += head.len() + body.len() + 8;
        Ok(())
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    }

    pub fn add_page(&mut self, r: &Raster, w_pt: f32, h_pt: f32) -> io::Result<()> {
        let id = self.offsets.len() + 1;
        let (page, image, content) = (id, id + 1, id + 2);
        let data = miniz_oxide::deflate::compress_to_vec_zlib(r.px, 6);
        let img_dict = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
            r.w, r.h
        );
        let ops = format!("q {w_pt:.2} 0 0 {h_pt:.2} 0 0 cm /Im0 Do Q");
        self.obj(
            page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.2} {h_pt:.2}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {content} 0 R >>"
            )
            .as_bytes(),
        )?;
        self.obj(image, &Self::stream(&img_dict, &data))?;
        self.obj(content, &Self::stream("", ops.as_bytes()))?;
        self.page_ids.push(page);
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let kids: Vec<String> = self.page_ids.iter().map(|i| format!("{i} 0 R")).collect();
        self.obj(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
        self.obj(2, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), kids.len()).as_bytes())?;
        let xref = self.pos;
        let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1);
        for off in &self.offsets {
            tail.push_str(&format!("{off:010} 00000 n \n"));
        }
        tail.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", self.offsets.len() + 1));
        self.out.write_all(tail.as_bytes())?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Prints the job. Returns a short status message on success.
#[cfg(not(windows))]
pub fn print(fs: &mut FontSystem, cache: &mut SwashCache, job: &Job, title: &str) -> Result<String, String> {
    let page = PageSetup::default_for_locale();
    let path = std::env::temp_dir().join(format!("plainpad-print-{}.pdf", std::process::id()));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut pdf = PdfWriter::new(io::BufWriter::new(file)).map_err(|e| e.to_string())?;
    let (w_pt, h_pt) = (page.w_in * 72.0, page.h_in * 72.0);
    let pages = render_pages(fs, cache, job, &page, |_, _, r| pdf.add_page(&r, w_pt, h_pt)).map_err(|e| e.to_string())?;
    pdf.finish().map_err(|e| e.to_string())?;

    match std::process::Command::new("lp").arg("-t").arg(title).arg(&path).output() {
        Ok(out) if out.status.success() => {
            let _ = std::fs::remove_file(&path);
            Ok(format!("Sent {pages} page(s) to the default printer"))
        }
        // No CUPS or no default printer: hand the PDF to the system viewer.
        _ => {
            open::that_detached(&path).map_err(|e| e.to_string())?;
            Ok(format!("No default printer; opened {pages}-page PDF in viewer"))
        }
    }
}

#[cfg(windows)]
pub fn print(fs: &mut FontSystem, cache: &mut SwashCache, job: &Job, title: &str) -> Result<String, String> {
    win::print(fs, cache, job, title)
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::Storage::Xps::*;
    use windows_sys::Win32::UI::Controls::Dialogs::*;

    #[repr(C)]
    struct GrayBitmapInfo {
        header: BITMAPINFOHEADER,
        colors: [RGBQUAD; 256],
    }

    pub fn print(fs: &mut FontSystem, cache: &mut SwashCache, job: &Job, title: &str) -> Result<String, String> {
        unsafe {
            let mut pd: PRINTDLGW = zeroed();
            pd.lStructSize = size_of::<PRINTDLGW>() as u32;
            pd.Flags = PD_RETURNDC | PD_NOSELECTION | PD_NOPAGENUMS;
            pd.nCopies = 1;
            if PrintDlgW(&mut pd) == 0 {
                return Err("Printing cancelled".into());
            }
            let dc = pd.hDC;
            let result = print_to_dc(dc, fs, cache, job, title);
            DeleteDC(dc);
            if !pd.hDevMode.is_null() {
                windows_sys::Win32::Foundation::GlobalFree(pd.hDevMode);
            }
            if !pd.hDevNames.is_null() {
                windows_sys::Win32::Foundation::GlobalFree(pd.hDevNames);
            }
            result
        }
    }

    unsafe fn print_to_dc(dc: HDC, fs: &mut FontSystem, cache: &mut SwashCache, job: &Job, title: &str) -> Result<String, String> {
        unsafe {
            let dpi_x = GetDeviceCaps(dc, LOGPIXELSX as _).max(1) as f32;
            let dpi_y = GetDeviceCaps(dc, LOGPIXELSY as _).max(1) as f32;
            let phys_w = GetDeviceCaps(dc, PHYSICALWIDTH as _);
            let phys_h = GetDeviceCaps(dc, PHYSICALHEIGHT as _);
            let off_x = GetDeviceCaps(dc, PHYSICALOFFSETX as _);
            let off_y = GetDeviceCaps(dc, PHYSICALOFFSETY as _);
            let page = PageSetup {
                w_in: phys_w as f32 / dpi_x,
                h_in: phys_h as f32 / dpi_y,
                margin_in: 0.75,
                dpi: dpi_x.min(300.0),
            };

            let mut bmi: GrayBitmapInfo = zeroed();
            for (i, c) in bmi.colors.iter_mut().enumerate() {
                *c = RGBQUAD { rgbBlue: i as u8, rgbGreen: i as u8, rgbRed: i as u8, rgbReserved: 0 };
            }
            let title_w: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let mut di: DOCINFOW = zeroed();
            di.cbSize = size_of::<DOCINFOW>() as i32;
            di.lpszDocName = title_w.as_ptr();
            if StartDocW(dc, &di) <= 0 {
                return Err("Could not start print job".into());
            }
            let mut padded = Vec::new();
            let res = render_pages(fs, cache, job, &page, |_, _, r| {
                // DIB rows must be 4-byte aligned.
                let stride = (r.w as usize + 3) & !3;
                padded.clear();
                padded.resize(stride * r.h as usize, 255);
                for (y, row) in r.px.chunks_exact(r.w as usize).enumerate() {
                    padded[y * stride..y * stride + row.len()].copy_from_slice(row);
                }
                bmi.header = BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: r.w as i32,
                    biHeight: -(r.h as i32),
                    biPlanes: 1,
                    biBitCount: 8,
                    biCompression: BI_RGB,
                    biClrUsed: 256,
                    ..zeroed()
                };
                if StartPage(dc) <= 0 {
                    return Err(io::Error::other("StartPage failed"));
                }
                StretchDIBits(
                    dc,
                    -off_x,
                    -off_y,
                    phys_w,
                    phys_h,
                    0,
                    0,
                    r.w as i32,
                    r.h as i32,
                    padded.as_ptr().cast(),
                    (&raw const bmi).cast(),
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                if EndPage(dc) <= 0 {
                    return Err(io::Error::other("EndPage failed"));
                }
                Ok(())
            });
            match res {
                Ok(n) => {
                    EndDoc(dc);
                    Ok(format!("Printed {n} page(s)"))
                }
                Err(e) => {
                    AbortDoc(dc);
                    Err(e.to_string())
                }
            }
        }
    }
}
