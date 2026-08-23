//! Encoding pictures as Sixel, the format foot and xterm understand.
//!
//! Sixel writes an image in bands six pixel rows tall. Within a band, one
//! character carries six vertically stacked pixels of one colour: bit 0 is the
//! top row, bit 5 the bottom, and the value is offset by 63 into printable
//! ASCII. A band is written once per colour that appears in it, returning to the
//! band's start with `$` in between, and `-` moves on to the next band.
//!
//! Colours come from a fixed palette rather than a quantiser. A 6·6·6 colour
//! cube plus a grey ramp covers book illustrations well, keeps the encoder
//! simple, and costs no dependency. Sixel palettes hold 256 entries, so the cube
//! has to stay within that budget.

use image::RgbaImage;

/// Levels per channel in the colour cube.
const LEVELS: usize = 6;
/// Extra grey steps, which line drawings and screenshots rely on.
const GREYS: usize = 24;

/// Builds the palette: a colour cube followed by a grey ramp.
fn palette() -> Vec<(u8, u8, u8)> {
    let mut colors = Vec::with_capacity(LEVELS * LEVELS * LEVELS + GREYS);
    for r in 0..LEVELS {
        for g in 0..LEVELS {
            for b in 0..LEVELS {
                colors.push((step(r), step(g), step(b)));
            }
        }
    }
    for grey in 0..GREYS {
        let value = (grey * 255 / (GREYS - 1)) as u8;
        colors.push((value, value, value));
    }
    colors
}

fn step(index: usize) -> u8 {
    (index * 255 / (LEVELS - 1)) as u8
}

/// Squared distance between two colours.
fn distance((ar, ag, ab): (u8, u8, u8), (br, bg, bb): (u8, u8, u8)) -> u32 {
    (ar as i32 - br as i32).pow(2) as u32
        + (ag as i32 - bg as i32).pow(2) as u32
        + (ab as i32 - bb as i32).pow(2) as u32
}

/// The cube level closest to one channel value.
///
/// The levels sit at 0, 51, 102, 153, 204 and 255, so the nearest one is the
/// value divided by the spacing and rounded. No value lands exactly between two
/// levels, because the spacing is odd.
fn level_of(value: u8) -> usize {
    ((value as u32 * (LEVELS as u32 - 1) + 127) / 255) as usize
}

/// Nearest palette entry for a pixel, by squared distance.
///
/// The cube is a regular grid, so its nearest point follows from rounding each
/// channel on its own: on a rectangular grid that minimises the euclidean
/// distance, and no search is needed. Only the grey ramp is scanned, and it
/// holds a tenth of the palette. Searching all entries cost a book of rendered
/// formulas about sixteen seconds a chapter.
///
/// Ties go to the lower index, as a scan over the whole palette would give.
fn nearest(palette: &[(u8, u8, u8)], pixel: (u8, u8, u8)) -> usize {
    let (r, g, b) = pixel;
    let cube = level_of(r) * LEVELS * LEVELS + level_of(g) * LEVELS + level_of(b);
    let mut best = cube;
    let mut best_distance = distance(palette[cube], pixel);

    let greys = LEVELS * LEVELS * LEVELS;
    for index in greys..palette.len() {
        let candidate = distance(palette[index], pixel);
        if candidate < best_distance {
            best_distance = candidate;
            best = index;
        }
    }
    best
}

/// Encodes an image as a Sixel escape sequence.
///
/// The image must already be scaled to its final pixel size, because Sixel
/// carries no scaling of its own.
pub fn encode(image: &RgbaImage) -> String {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return String::new();
    }
    let palette = palette();

    // Map every pixel to a palette index once, so the per-colour passes below
    // only compare integers.
    let mut indexed = vec![0u16; (width * height) as usize];
    let mut used = vec![false; palette.len()];
    for (x, y, pixel) in image.enumerate_pixels() {
        let index = nearest(&palette, super::flatten(pixel.0));
        indexed[(y * width + x) as usize] = index as u16;
        used[index] = true;
    }

    let mut out = String::with_capacity((width * height / 4) as usize + 1024);
    // P q introduces the data; the raster attributes give the aspect ratio 1:1
    // and the pixel size, which lets a terminal reserve the right area.
    out.push_str("\x1bP0;1;0q\"1;1;");
    out.push_str(&format!("{width};{height}"));

    for (index, &(r, g, b)) in palette.iter().enumerate() {
        if !used[index] {
            continue;
        }
        // Sixel colour components are percentages.
        out.push_str(&format!(
            "#{};2;{};{};{}",
            index,
            percent(r),
            percent(g),
            percent(b)
        ));
    }

    let bands = height.div_ceil(6);
    for band in 0..bands {
        let mut first_colour = true;
        for colour in 0..palette.len() {
            if !used[colour] {
                continue;
            }
            // Collect this colour's pixels across the band.
            let mut run: Vec<u8> = Vec::new();
            let mut any = false;
            for x in 0..width {
                let mut bits = 0u8;
                for row in 0..6u32 {
                    let y = band * 6 + row;
                    if y >= height {
                        break;
                    }
                    if indexed[(y * width + x) as usize] as usize == colour {
                        bits |= 1 << row;
                    }
                }
                if bits != 0 {
                    any = true;
                }
                run.push(bits);
            }
            if !any {
                continue;
            }
            if !first_colour {
                // Return to the start of the band for the next colour.
                out.push('$');
            }
            first_colour = false;
            out.push_str(&format!("#{colour}"));
            write_run(&mut out, &run);
        }
        if band + 1 < bands {
            out.push('-');
        }
    }

    out.push_str("\x1b\\");
    out
}

/// Writes one colour's band, compressing repeats with the `!` run operator.
fn write_run(out: &mut String, run: &[u8]) {
    let mut index = 0;
    while index < run.len() {
        let bits = run[index];
        let mut length = 1;
        while index + length < run.len() && run[index + length] == bits {
            length += 1;
        }
        let glyph = (bits + 63) as char;
        // The operator only pays off from four repeats.
        if length >= 4 {
            out.push_str(&format!("!{length}{glyph}"));
        } else {
            for _ in 0..length {
                out.push(glyph);
            }
        }
        index += length;
    }
}

fn percent(value: u8) -> u8 {
    ((value as u16 * 100) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        let mut buffer = RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = Rgba(color);
        }
        buffer
    }

    #[test]
    fn the_palette_fits_sixel_limits() {
        assert!(palette().len() <= 256, "{} entries", palette().len());
    }

    #[test]
    fn an_encoded_image_is_well_formed() {
        let escape = encode(&solid(4, 6, [255, 0, 0, 255]));
        assert!(escape.starts_with("\x1bP"), "missing introducer");
        assert!(escape.contains("q\"1;1;4;6"), "missing raster attributes");
        assert!(escape.ends_with("\x1b\\"), "missing terminator");
        assert!(escape.contains("#"), "no colour defined");
    }

    #[test]
    fn a_full_band_sets_every_bit() {
        // Six rows of one colour means all six bits, so 63 + 63 = '~'.
        let escape = encode(&solid(1, 6, [0, 0, 0, 255]));
        assert!(escape.contains('~'), "{escape:?}");
    }

    #[test]
    fn bands_are_separated() {
        // Twelve rows are two bands.
        let escape = encode(&solid(2, 12, [0, 0, 0, 255]));
        assert_eq!(escape.matches('-').count(), 1, "expected one band break");
    }

    #[test]
    fn repeats_are_compressed() {
        let escape = encode(&solid(20, 6, [0, 0, 0, 255]));
        assert!(escape.contains("!20"), "expected a run of 20: {escape:?}");
    }

    #[test]
    fn short_runs_stay_literal() {
        let mut buffer = solid(3, 6, [0, 0, 0, 255]);
        // Three identical columns are below the threshold for `!`.
        for pixel in buffer.pixels_mut() {
            *pixel = Rgba([0, 0, 0, 255]);
        }
        let escape = encode(&buffer);
        assert!(!escape.contains('!'), "{escape:?}");
    }

    #[test]
    fn an_empty_image_encodes_to_nothing() {
        assert!(encode(&RgbaImage::new(0, 0)).is_empty());
    }

    /// What `nearest` replaced: a scan over every entry. Kept so the fast path
    /// can be held against it.
    fn nearest_by_scan(palette: &[(u8, u8, u8)], pixel: (u8, u8, u8)) -> usize {
        let mut best = 0;
        let mut best_distance = u32::MAX;
        for (index, &entry) in palette.iter().enumerate() {
            let candidate = distance(entry, pixel);
            if candidate < best_distance {
                best_distance = candidate;
                best = index;
            }
        }
        best
    }

    #[test]
    fn the_fast_lookup_agrees_with_a_full_scan() {
        let palette = palette();
        // Every eleventh value per channel, which lands on and between the cube
        // levels and covers both ends. All 16.7 million were checked once by
        // hand and agreed; this keeps the cheap part of that in the suite.
        let values: Vec<u8> = (0..=255u8).step_by(11).chain([255]).collect();
        for &r in &values {
            for &g in &values {
                for &b in &values {
                    assert_eq!(
                        nearest(&palette, (r, g, b)),
                        nearest_by_scan(&palette, (r, g, b)),
                        "rgb({r},{g},{b})"
                    );
                }
            }
        }
    }

    #[test]
    fn cube_levels_map_to_themselves() {
        let palette = palette();
        for level in 0..LEVELS {
            let value = step(level);
            let grey = (value, value, value);
            // A cube corner is its own nearest entry, unless the grey ramp holds
            // the very same colour at a lower index, which it never does.
            assert_eq!(palette[nearest(&palette, grey)], grey, "level {level}");
        }
    }

    #[test]
    fn two_colours_share_a_band() {
        let mut buffer = RgbaImage::new(2, 6);
        for y in 0..6 {
            buffer.put_pixel(0, y, Rgba([255, 0, 0, 255]));
            buffer.put_pixel(1, y, Rgba([0, 0, 255, 255]));
        }
        let escape = encode(&buffer);
        // A second colour in the same band needs the carriage return.
        assert!(escape.contains('$'), "{escape:?}");
    }

    #[test]
    fn percentages_span_the_range() {
        assert_eq!(percent(0), 0);
        assert_eq!(percent(255), 100);
    }
}
