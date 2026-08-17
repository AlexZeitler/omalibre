//! Showing images in the terminal.
//!
//! The reader must work in every terminal the user might pick, and through tmux.
//! That rules out relying on a graphics protocol: Ghostty speaks the Kitty
//! protocol, foot speaks Sixel, alacritty speaks neither, and tmux breaks both
//! because it manages the screen contents itself.
//!
//! What works everywhere is the half block. Each cell shows `▀` with the
//! foreground painted for the upper pixel and the background for the lower one,
//! so one cell carries two pixels. The result is coarse but universal, and it
//! survives scrolling and redrawing like any other text.

pub mod detect;
mod kitty;
mod sixel;

pub use detect::Backend;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};

/// A single cell of a rendered image: two stacked pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub upper: (u8, u8, u8),
    pub lower: (u8, u8, u8),
}

/// A picture ready to be drawn, in whichever form the terminal takes.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// Cells the picture occupies. The layout reserves this many lines either
    /// way, so text flows around the picture the same in every backend.
    cols: u16,
    rows: u16,
    payload: Payload,
}

#[derive(Debug, Clone)]
enum Payload {
    /// Two pixels per cell, drawn as ordinary text.
    HalfBlocks(Vec<Vec<Cell>>),
    /// A ready-made escape sequence, written past the text buffer.
    Escape(String),
}

impl Rendered {
    pub fn height(&self) -> usize {
        self.rows as usize
    }

    pub fn width(&self) -> usize {
        self.cols as usize
    }

    /// The cell rows, for the half block backend.
    pub fn cells(&self) -> Option<&[Vec<Cell>]> {
        match &self.payload {
            Payload::HalfBlocks(rows) => Some(rows),
            Payload::Escape(_) => None,
        }
    }

    /// The escape sequence that places this picture at the cursor.
    pub fn escape(&self) -> Option<&str> {
        match &self.payload {
            Payload::Escape(text) => Some(text),
            Payload::HalfBlocks(_) => None,
        }
    }
}

/// Terminal cells are about twice as tall as they are wide, so a pixel pair per
/// cell keeps an image roughly in proportion.
const CELL_ASPECT: f32 = 2.0;

/// Decodes image bytes and prepares them for the given backend.
///
/// `max_rows` caps the height so a full-page illustration cannot push the text
/// off the screen. `id` distinguishes pictures for backends that address them,
/// which is how a stale picture gets removed before the next draw.
pub fn render(
    bytes: &[u8],
    max_cols: u16,
    max_rows: u16,
    backend: Backend,
    id: u32,
    cell: CellSize,
) -> Result<Rendered> {
    let decoded = image::load_from_memory(bytes).context("cannot decode image")?;
    Ok(match backend {
        Backend::HalfBlocks => fit(&decoded, max_cols, max_rows),
        Backend::Kitty | Backend::Sixel => {
            fit_pixels(&decoded, bytes, max_cols, max_rows, backend, id, cell)
        }
    })
}

/// Size of one terminal cell in pixels. Needed to scale a picture to a whole
/// number of cells, so the reserved lines match what the terminal paints.
#[derive(Debug, Clone, Copy)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

impl Default for CellSize {
    fn default() -> Self {
        // A common default; the real size is read from the terminal when it
        // reports one.
        Self {
            width: 8,
            height: 16,
        }
    }
}

/// Prepares a picture for a pixel protocol, sized to a whole number of cells.
fn fit_pixels(
    decoded: &DynamicImage,
    original: &[u8],
    max_cols: u16,
    max_rows: u16,
    backend: Backend,
    id: u32,
    cell: CellSize,
) -> Rendered {
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::Escape(String::new()),
        };
    }

    // Work out the cell box the picture should fill, keeping its proportions.
    let cell_w = cell.width.max(1) as f32;
    let cell_h = cell.height.max(1) as f32;
    let by_width = max_cols.max(1) as f32;
    let rows_needed = (height as f32 * (by_width * cell_w) / width as f32) / cell_h;
    let (cols, rows) = if rows_needed <= max_rows.max(1) as f32 {
        (by_width, rows_needed.round().max(1.0))
    } else {
        let rows = max_rows.max(1) as f32;
        let cols = (width as f32 * (rows * cell_h) / height as f32) / cell_w;
        (cols.round().max(1.0).min(by_width), rows)
    };

    let escape = match backend {
        // Kitty scales the original itself, which keeps every pixel.
        Backend::Kitty => kitty::encode_png(original, cols as u16, rows as u16, id),
        // Sixel carries no scaling, so the pixels are resized to the cell box.
        Backend::Sixel => {
            let pixel_width = (cols * cell_w) as u32;
            let pixel_height = (rows * cell_h) as u32;
            let scaled = decoded
                .resize_exact(pixel_width.max(1), pixel_height.max(1), FilterType::Triangle)
                .to_rgba8();
            sixel::encode(&scaled)
        }
        Backend::HalfBlocks => String::new(),
    };

    Rendered {
        cols: cols as u16,
        rows: rows as u16,
        payload: Payload::Escape(escape),
    }
}

/// Removes every picture a pixel backend has placed. Called before a redraw,
/// because pictures live outside the text buffer and would otherwise stay put.
pub fn clear_all(backend: Backend) -> Option<String> {
    match backend {
        Backend::Kitty => Some(kitty::delete_all()),
        // Sixel pictures are part of the screen contents and vanish with it.
        Backend::Sixel | Backend::HalfBlocks => None,
    }
}

fn fit(decoded: &DynamicImage, max_cols: u16, max_rows: u16) -> Rendered {
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::HalfBlocks(Vec::new()),
        };
    }

    // Target size in pixels: one column is one pixel wide, one row is two tall.
    let cols = max_cols.max(1) as f32;
    let rows_from_width = (height as f32 / width as f32) * cols / CELL_ASPECT;
    let rows = rows_from_width.round().max(1.0).min(max_rows.max(1) as f32);
    // Recompute the width so a capped height does not stretch the picture.
    let cols = (rows * CELL_ASPECT * width as f32 / height as f32)
        .round()
        .max(1.0)
        .min(cols);

    let pixel_width = cols as u32;
    let pixel_height = (rows * 2.0) as u32;
    let scaled = decoded
        .resize_exact(pixel_width, pixel_height, FilterType::Triangle)
        .to_rgba8();

    let mut out = Vec::with_capacity(rows as usize);
    for row in 0..rows as u32 {
        let mut cells = Vec::with_capacity(pixel_width as usize);
        for column in 0..pixel_width {
            let upper = scaled.get_pixel(column, row * 2);
            let lower_y = (row * 2 + 1).min(pixel_height.saturating_sub(1));
            let lower = scaled.get_pixel(column, lower_y);
            cells.push(Cell {
                upper: flatten(upper.0),
                lower: flatten(lower.0),
            });
        }
        out.push(cells);
    }
    Rendered {
        cols: pixel_width as u16,
        rows: out.len() as u16,
        payload: Payload::HalfBlocks(out),
    }
}

/// Composites a pixel onto white.
///
/// Many book illustrations are line drawings on transparency. Left alone they
/// would come out as black on black in a dark terminal, so transparency is
/// resolved against white, the colour the author assumed.
pub(crate) fn flatten([r, g, b, a]: [u8; 4]) -> (u8, u8, u8) {
    if a == 255 {
        return (r, g, b);
    }
    let alpha = a as f32 / 255.0;
    let over = |channel: u8| -> u8 {
        (channel as f32 * alpha + 255.0 * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
    };
    (over(r), over(g), over(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(width: u32, height: u32, color: [u8; 4]) -> DynamicImage {
        let mut buffer = RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = Rgba(color);
        }
        DynamicImage::ImageRgba8(buffer)
    }

    #[test]
    fn fits_within_the_given_columns() {
        let rendered = fit(&solid(100, 100, [10, 20, 30, 255]), 40, 100);
        assert!(rendered.width() <= 40);
        assert!(rendered.height() >= 1);
    }

    #[test]
    fn keeps_a_square_image_roughly_square() {
        let rendered = fit(&solid(100, 100, [0, 0, 0, 255]), 40, 100);
        // Two pixels per row, so a square picture is half as many rows as
        // columns, give or take rounding.
        let expected = rendered.width() as f32 / CELL_ASPECT;
        assert!(
            (rendered.height() as f32 - expected).abs() <= 1.0,
            "{} rows for {} columns",
            rendered.height(),
            rendered.width()
        );
    }

    #[test]
    fn respects_the_row_cap_without_stretching() {
        let tall = solid(100, 1000, [0, 0, 0, 255]);
        let rendered = fit(&tall, 80, 10);
        assert!(rendered.height() <= 10);
        // A tall picture must stay narrow rather than filling the width.
        assert!(
            rendered.width() < 80,
            "{} columns is too wide for 10 rows",
            rendered.width()
        );
    }

    #[test]
    fn carries_the_colour_through() {
        let rendered = fit(&solid(10, 10, [200, 100, 50, 255]), 10, 10);
        let cell = rendered.cells().unwrap()[0][0];
        assert_eq!(cell.upper, (200, 100, 50));
        assert_eq!(cell.lower, (200, 100, 50));
    }

    #[test]
    fn resolves_transparency_against_white() {
        assert_eq!(flatten([0, 0, 0, 255]), (0, 0, 0));
        assert_eq!(flatten([0, 0, 0, 0]), (255, 255, 255));
        let half = flatten([0, 0, 0, 128]);
        assert!(half.0 > 100 && half.0 < 160, "{half:?}");
    }

    #[test]
    fn an_empty_image_renders_to_nothing() {
        let empty = DynamicImage::ImageRgba8(RgbaImage::new(0, 0));
        assert_eq!(fit(&empty, 40, 20).height(), 0);
    }
}
