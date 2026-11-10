//! Software drawing primitives: a clipped ARGB canvas, colours and UI text.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub chrome: Color,
    pub chrome_fg: Color,
    pub dim: Color,
    pub border: Color,
    pub hover: Color,
    pub accent: Color,
    pub accent_fg: Color,
    pub sel: Color,
    pub sel_fg: Color,
    pub link: Color,
    /// Suspicious links and warnings.
    pub danger: Color,
    pub field: Color,
    pub shade: Color,
}

impl Theme {
    pub const LIGHT: Theme = Theme {
        bg: Color::rgb(0xFF, 0xFF, 0xFF),
        fg: Color::rgb(0x1E, 0x1E, 0x1E),
        chrome: Color::rgb(0xF3, 0xF3, 0xF3),
        chrome_fg: Color::rgb(0x20, 0x20, 0x20),
        dim: Color::rgb(0x6E, 0x6E, 0x6E),
        border: Color::rgb(0xD0, 0xD0, 0xD0),
        hover: Color::rgb(0xE1, 0xE6, 0xEE),
        accent: Color::rgb(0x1F, 0x5F, 0xBF),
        accent_fg: Color::rgb(0xFF, 0xFF, 0xFF),
        sel: Color::rgb(0xB4, 0xD5, 0xFE),
        sel_fg: Color::rgb(0x00, 0x00, 0x00),
        link: Color::rgb(0x0B, 0x57, 0xD0),
        danger: Color::rgb(0xC4, 0x2B, 0x1C),
        field: Color::rgb(0xFF, 0xFF, 0xFF),
        shade: Color::rgba(0, 0, 0, 0x50),
    };
    pub const DARK: Theme = Theme {
        bg: Color::rgb(0x1E, 0x1E, 0x1E),
        fg: Color::rgb(0xDC, 0xDC, 0xDC),
        chrome: Color::rgb(0x2B, 0x2B, 0x2B),
        chrome_fg: Color::rgb(0xE0, 0xE0, 0xE0),
        dim: Color::rgb(0x9A, 0x9A, 0x9A),
        border: Color::rgb(0x45, 0x45, 0x45),
        hover: Color::rgb(0x3A, 0x3F, 0x48),
        accent: Color::rgb(0x3C, 0x7E, 0xE0),
        accent_fg: Color::rgb(0xFF, 0xFF, 0xFF),
        sel: Color::rgb(0x26, 0x4F, 0x78),
        sel_fg: Color::rgb(0xFF, 0xFF, 0xFF),
        link: Color::rgb(0x6C, 0xB0, 0xFF),
        danger: Color::rgb(0xFF, 0x7A, 0x6B),
        field: Color::rgb(0x1E, 0x1E, 0x1E),
        shade: Color::rgba(0, 0, 0, 0x80),
    };
}

/// A borrowed 0x00RRGGBB pixel buffer with a clip rectangle.
pub struct Canvas<'a> {
    px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    clip: [i32; 4],
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], w: u32, h: u32) -> Self {
        let (w, h) = (w as i32, h as i32);
        Self { px, w, h, clip: [0, 0, w, h] }
    }

    pub fn set_clip(&mut self, r: Rect) {
        self.clip = [
            (r.x.floor() as i32).max(0),
            (r.y.floor() as i32).max(0),
            (r.right().ceil() as i32).min(self.w),
            (r.bottom().ceil() as i32).min(self.h),
        ];
    }

    pub fn reset_clip(&mut self) {
        self.clip = [0, 0, self.w, self.h];
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: Color) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        if x < cx0 || y < cy0 || x >= cx1 || y >= cy1 {
            return;
        }
        let a = c.a() as u32;
        if a == 0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        let src = c.0 & 0xFF_FFFF;
        if a == 255 {
            self.px[i] = src;
        } else {
            self.px[i] = mix(self.px[i], src, a);
        }
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        let x0 = (r.x.round() as i32).max(cx0);
        let y0 = (r.y.round() as i32).max(cy0);
        let x1 = (r.right().round() as i32).min(cx1);
        let y1 = (r.bottom().round() as i32).min(cy1);
        if x0 >= x1 || y0 >= y1 || c.a() == 0 {
            return;
        }
        let a = c.a() as u32;
        let src = c.0 & 0xFF_FFFF;
        for y in y0..y1 {
            let row = &mut self.px[(y * self.w + x0) as usize..(y * self.w + x1) as usize];
            if a == 255 {
                row.fill(src);
            } else {
                for p in row {
                    *p = mix(*p, src, a);
                }
            }
        }
    }

    pub fn outline(&mut self, r: Rect, t: f32, c: Color) {
        self.fill(Rect::new(r.x, r.y, r.w, t), c);
        self.fill(Rect::new(r.x, r.bottom() - t, r.w, t), c);
        self.fill(Rect::new(r.x, r.y, t, r.h), c);
        self.fill(Rect::new(r.right() - t, r.y, t, r.h), c);
    }
}

#[inline]
fn mix(dst: u32, src: u32, a: u32) -> u32 {
    let ia = 255 - a;
    let rb = ((src & 0xFF00FF) * a + (dst & 0xFF00FF) * ia + 0x800080) >> 8 & 0xFF00FF;
    let g = ((src & 0x00FF00) * a + (dst & 0x00FF00) * ia + 0x008000) >> 8 & 0x00FF00;
    rb | g
}

/// Glyph images kept before the cache is cleared.
const MAX_GLYPHS: usize = 8192;

/// Shaped UI labels kept per generation of the label cache.
const UI_GEN: usize = 256;

/// Font system, glyph cache, and a small cache of shaped UI labels.
pub struct Gfx {
    pub fs: FontSystem,
    pub cache: SwashCache,
    /// Labels cached in two generations: when `ui` fills up it becomes `ui_old`,
    /// and labels drawn from `ui_old` move back. Labels drawn every frame (menus,
    /// tab names) are never reshaped; ones no longer drawn age out.
    ui: HashMap<String, Buffer>,
    ui_old: HashMap<String, Buffer>,
    ui_metrics: Metrics,
    /// Labels shaped so far (for tests).
    #[cfg(test)]
    shaped: usize,
}

impl Gfx {
    pub fn new() -> Self {
        Self {
            fs: FontSystem::new(),
            cache: SwashCache::new(),
            ui: HashMap::new(),
            ui_old: HashMap::new(),
            ui_metrics: Metrics::new(13.0, 18.0),
            #[cfg(test)]
            shaped: 0,
        }
    }

    /// Drops every cached glyph image. After a zoom or font change the old sizes
    /// are never drawn again, so keeping them would only hold memory.
    pub fn clear_glyphs(&mut self) {
        self.cache = SwashCache::new();
        crate::mem::trim();
    }

    /// Keeps the glyph cache bounded (called before each frame). The limit is
    /// well above a full screen of distinct CJK characters, so normal drawing
    /// never thrashes it; a long session through varied text can't grow it forever.
    pub fn bound_glyphs(&mut self) {
        if self.cache.image_cache.len() > MAX_GLYPHS {
            self.clear_glyphs();
        }
    }

    pub fn set_ui_scale(&mut self, scale: f32) {
        let m = Metrics::new((13.0 * scale).round(), (18.0 * scale).round());
        if m != self.ui_metrics {
            self.ui_metrics = m;
            self.ui.clear();
            self.ui_old.clear();
        }
    }

    pub fn ui_line_h(&self) -> f32 {
        self.ui_metrics.line_height
    }

    /// Lays out a single-line UI label.
    fn shape(&mut self, text: &str) -> Buffer {
        #[cfg(test)]
        {
            self.shaped += 1;
        }
        let mut b = Buffer::new(&mut self.fs, self.ui_metrics);
        b.set_size(None, None);
        b.set_text(text, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        b
    }

    /// The cached layout of a label.
    fn label(&mut self, text: &str) -> &Buffer {
        if !self.ui.contains_key(text) {
            let b = match self.ui_old.remove(text) {
                Some(b) => b,
                None => self.shape(text),
            };
            if self.ui.len() >= UI_GEN {
                self.ui_old = std::mem::take(&mut self.ui);
            }
            self.ui.insert(text.to_owned(), b);
        }
        &self.ui[text]
    }

    pub fn text_w(&mut self, text: &str) -> f32 {
        width(self.label(text))
    }

    /// Like [`Gfx::text_w`], for text that keeps changing (a field being typed
    /// in, the cursor position): measured without filling the label cache.
    pub fn text_w_once(&mut self, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        width(&self.shape(text))
    }

    /// Draws a single-line UI label with its line box top at `y`. Returns its width.
    pub fn text(&mut self, cv: &mut Canvas, text: &str, x: f32, y: f32, color: Color) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.label(text);
        let Gfx { fs, cache, ui, .. } = self;
        draw(fs, cache, cv, &ui[text], x, y, color)
    }

    /// Like [`Gfx::text`], for text that keeps changing: drawn without filling the label cache.
    pub fn text_once(&mut self, cv: &mut Canvas, text: &str, x: f32, y: f32, color: Color) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let b = self.shape(text);
        draw(&mut self.fs, &mut self.cache, cv, &b, x, y, color)
    }

    /// Draws a label vertically centred in `r`, left aligned at `r.x`.
    pub fn text_in(&mut self, cv: &mut Canvas, text: &str, r: Rect, color: Color) -> f32 {
        let y = r.y + ((r.h - self.ui_line_h()) / 2.0).round();
        self.text(cv, text, r.x, y, color)
    }

    /// Like [`Gfx::text_in`], for text that keeps changing: drawn without filling the label cache.
    pub fn text_in_once(&mut self, cv: &mut Canvas, text: &str, r: Rect, color: Color) -> f32 {
        let y = r.y + ((r.h - self.ui_line_h()) / 2.0).round();
        self.text_once(cv, text, r.x, y, color)
    }
}

/// Width of a shaped label.
fn width(b: &Buffer) -> f32 {
    b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

/// Draws a shaped label with its line box top at `y`. Returns its width.
fn draw(fs: &mut FontSystem, cache: &mut SwashCache, cv: &mut Canvas, b: &Buffer, x: f32, y: f32, color: Color) -> f32 {
    let mut w: f32 = 0.0;
    for run in b.layout_runs() {
        w = w.max(run.line_w);
        for g in run.glyphs {
            let pg = g.physical((x, y + run.line_y), 1.0);
            cache.with_pixels(fs, pg.cache_key, color, |gx, gy, c| cv.blend(pg.x + gx, pg.y + gy, c));
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_off_labels_dont_evict_the_rest() {
        let mut g = Gfx::new();
        for i in 0..2000 {
            g.text_w("File"); // drawn every frame, like a menu label
            g.text_w(&format!("Ln {i}, Col 1")); // never drawn again
            assert!(g.ui.len() <= UI_GEN && g.ui_old.len() <= UI_GEN);
        }
        assert_eq!(g.shaped, 2001, "\"File\" was shaped once");
        // Text drawn without the cache stays out of it.
        let before = g.ui.len() + g.ui_old.len();
        g.text_w_once("typed into a field");
        assert_eq!(g.ui.len() + g.ui_old.len(), before);
    }
}
