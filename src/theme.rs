use egui::Color32;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    pub fn from_str_loose(value: &str) -> Self {
        if value.eq_ignore_ascii_case("dark") {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub paper: Color32,
    pub side: Color32,
    pub surface: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub line: Color32,
    pub accent: Color32,
    pub accent_ink: Color32,
    pub code_bg: Color32,
    pub selection: Color32,
}

impl Palette {
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self {
                paper: Color32::from_rgb(0xfb, 0xfa, 0xf6),
                side: Color32::from_rgb(0xef, 0xee, 0xe6),
                surface: Color32::from_rgb(0xff, 0xfe, 0xfa),
                ink: Color32::from_rgb(0x23, 0x2b, 0x26),
                muted: Color32::from_rgb(0x59, 0x64, 0x5e),
                line: Color32::from_rgb(0xdb, 0xde, 0xd3),
                accent: Color32::from_rgb(0x28, 0x58, 0x47),
                accent_ink: Color32::WHITE,
                code_bg: Color32::from_rgb(0xf5, 0xf3, 0xec),
                selection: Color32::from_rgba_unmultiplied(0x34, 0x7e, 0xcc, 0x58),
            },
            ThemeMode::Dark => Self {
                paper: Color32::from_rgb(0x20, 0x26, 0x22),
                side: Color32::from_rgb(0x24, 0x2e, 0x28),
                surface: Color32::from_rgb(0x29, 0x34, 0x2d),
                ink: Color32::from_rgb(0xf0, 0xf3, 0xeb),
                muted: Color32::from_rgb(0xbd, 0xc9, 0xbd),
                line: Color32::from_rgb(0x46, 0x56, 0x4b),
                accent: Color32::from_rgb(0xa9, 0xd7, 0xba),
                accent_ink: Color32::from_rgb(0x15, 0x27, 0x1c),
                code_bg: Color32::from_rgb(0x18, 0x1d, 0x1a),
                selection: Color32::from_rgba_unmultiplied(0x5a, 0xa8, 0xe8, 0x58),
            },
        }
    }
}

const DARK_BG_L: f64 = 0.245;
const DARK_INK_L: f64 = 0.945;
const NEUTRAL_C_LOW: f64 = 0.065;
const NEUTRAL_C_HIGH: f64 = 0.130;
const LUT_SIZE: usize = 65;

fn srgb_to_linear_u8(c: u8) -> f64 {
    let x = f64::from(c) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb_u8(c: f64) -> u8 {
    let clamped = c.clamp(0.0, 1.0);
    let srgb = if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

fn linear_rgb_to_oklab(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

fn oklab_to_linear_rgb(l: f64, a: f64, b: f64) -> (f64, f64, f64) {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;
    (
        4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
        -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
        -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3,
    )
}

fn max_gamut_chroma(l: f64, cos_h: f64, sin_h: f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = 0.45;
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        let (r, g, b) = oklab_to_linear_rgb(l, mid * cos_h, mid * sin_h);
        if (-1e-4..=1.0001).contains(&r)
            && (-1e-4..=1.0001).contains(&g)
            && (-1e-4..=1.0001).contains(&b)
        {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

fn adapt_pixel_rgb(r: u8, g: u8, b: u8) -> [u8; 3] {
    let (l, a, b_ch) = linear_rgb_to_oklab(
        srgb_to_linear_u8(r),
        srgb_to_linear_u8(g),
        srgb_to_linear_u8(b),
    );
    let c = a.hypot(b_ch);
    let mut neut_l = DARK_INK_L - l * (DARK_INK_L - DARK_BG_L);
    if l > 0.72 {
        let t = (l - 0.72) / 0.28;
        neut_l += t * t * (0.045 * (1.0 - t) - 0.01 * t);
    }
    if c <= 1e-4 {
        let lr = oklab_to_linear_rgb(neut_l, 0.0, 0.0);
        return [
            linear_to_srgb_u8(lr.0),
            linear_to_srgb_u8(lr.1),
            linear_to_srgb_u8(lr.2),
        ];
    }
    let cos_h = a / c;
    let sin_h = b_ch / c;
    let inv_target_l = if l < 0.70 {
        (0.84 - 0.26 * (l / 0.70)).max(0.58)
    } else {
        DARK_BG_L + (1.0 - (l - 0.70) / 0.30) * (0.58 - DARK_BG_L)
    };
    let (target_l, target_c) = if c <= NEUTRAL_C_LOW {
        (neut_l, c * 0.25)
    } else if c < NEUTRAL_C_HIGH {
        let w = (c - NEUTRAL_C_LOW) / (NEUTRAL_C_HIGH - NEUTRAL_C_LOW);
        let smooth = w * w * (3.0 - 2.0 * w);
        (
            neut_l + smooth * (inv_target_l - neut_l),
            c * (0.25 + 0.75 * smooth),
        )
    } else {
        (inv_target_l, c)
    };
    let final_c = target_c.min(max_gamut_chroma(target_l, cos_h, sin_h) * 0.98);
    let lr = oklab_to_linear_rgb(target_l, final_c * cos_h, final_c * sin_h);
    [
        linear_to_srgb_u8(lr.0),
        linear_to_srgb_u8(lr.1),
        linear_to_srgb_u8(lr.2),
    ]
}

fn smart_lut() -> &'static Vec<u8> {
    static LUT: OnceLock<Vec<u8>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = vec![0u8; LUT_SIZE * LUT_SIZE * LUT_SIZE * 3];
        let step = 255.0 / (LUT_SIZE - 1) as f64;
        for ri in 0..LUT_SIZE {
            let r = (ri as f64 * step).round() as u8;
            for gi in 0..LUT_SIZE {
                let g = (gi as f64 * step).round() as u8;
                for bi in 0..LUT_SIZE {
                    let b = (bi as f64 * step).round() as u8;
                    let rgb = adapt_pixel_rgb(r, g, b);
                    let idx = ((ri * LUT_SIZE + gi) * LUT_SIZE + bi) * 3;
                    lut[idx] = rgb[0];
                    lut[idx + 1] = rgb[1];
                    lut[idx + 2] = rgb[2];
                }
            }
        }
        lut
    })
}

pub fn smart_invert_rgba(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    preserve_rects: &[[f32; 4]],
) {
    let lut = smart_lut();
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return;
    }
    let page_area = (w * h) as f32;
    let has_full_page = preserve_rects
        .iter()
        .any(|[_, _, rw, rh]| *rw * *rh >= page_area * 0.75);
    let rects: Vec<[usize; 4]> = if has_full_page {
        Vec::new()
    } else {
        preserve_rects
            .iter()
            .filter(|[_, _, rw, rh]| *rw >= 8.0 && *rh >= 8.0)
            .filter_map(|[x, y, rw, rh]| {
                let x0 = (x.floor().max(0.0) as usize).min(w);
                let y0 = (y.floor().max(0.0) as usize).min(h);
                let x1 = ((x + rw).ceil().max(0.0) as usize).min(w);
                let y1 = ((y + rh).ceil().max(0.0) as usize).min(h);
                (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
            })
            .collect()
    };

    const STRIDE_R: usize = LUT_SIZE * LUT_SIZE * 3;
    const STRIDE_G: usize = LUT_SIZE * 3;
    const WHITE_LI: usize = 64 * STRIDE_R + 64 * STRIDE_G + 64 * 3;
    const BLACK_LI: usize = 0;
    let white_rgb = [lut[WHITE_LI], lut[WHITE_LI + 1], lut[WHITE_LI + 2]];
    let black_rgb = [lut[BLACK_LI], lut[BLACK_LI + 1], lut[BLACK_LI + 2]];

    let row_bytes = w * 4;
    for y in 0..h {
        let mut row_ranges: [(usize, usize); 16] = [(0, 0); 16];
        let mut range_count = 0usize;
        for &[x0, y0, x1, y1] in &rects {
            if y >= y0 && y < y1 && range_count < row_ranges.len() {
                row_ranges[range_count] = (x0, x1);
                range_count += 1;
            }
        }
        let row_slice = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
        let mut spans: [(usize, usize); 17] = [(0, 0); 17];
        let mut span_count = 0usize;
        if range_count == 0 {
            spans[0] = (0, w);
            span_count = 1;
        } else {
            let active = &mut row_ranges[..range_count];
            active.sort_unstable_by_key(|&(x0, _)| x0);
            let mut cursor = 0usize;
            for &(x0, x1) in active.iter() {
                if x0 > cursor {
                    spans[span_count] = (cursor, x0);
                    span_count += 1;
                }
                cursor = cursor.max(x1);
            }
            if cursor < w {
                spans[span_count] = (cursor, w);
                span_count += 1;
            }
        }

        for &(sx0, sx1) in &spans[..span_count] {
            for px in row_slice[sx0 * 4..sx1 * 4].as_chunks_mut::<4>().0 {
                if px[3] == 0 {
                    continue;
                }
                if px[0] == 255 && px[1] == 255 && px[2] == 255 {
                    px[0] = white_rgb[0];
                    px[1] = white_rgb[1];
                    px[2] = white_rgb[2];
                    continue;
                }
                if px[0] == 0 && px[1] == 0 && px[2] == 0 {
                    px[0] = black_rgb[0];
                    px[1] = black_rgb[1];
                    px[2] = black_rgb[2];
                    continue;
                }
                let r = (usize::from(px[0]) + 2) >> 2;
                let g = (usize::from(px[1]) + 2) >> 2;
                let b = (usize::from(px[2]) + 2) >> 2;
                let li = r * STRIDE_R + g * STRIDE_G + b * 3;
                px[0] = lut[li];
                px[1] = lut[li + 1];
                px[2] = lut[li + 2];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_invert_darkens_paper_brightens_ink_and_preserves_images() {
        // 40x20 pixel buffer: white paper, black ink, and a preserved photo rect at [4, 4, 12, 10]
        let width = 40;
        let height = 20;
        let mut rgba = vec![255u8; (width * height * 4) as usize];
        // Black ink pixel at (0, 0)
        rgba[0] = 0;
        rgba[1] = 0;
        rgba[2] = 0;
        // Blue image pixel inside preserved rect at (6, 6)
        let photo_idx = (6 * width as usize + 6) * 4;
        rgba[photo_idx] = 20;
        rgba[photo_idx + 1] = 120;
        rgba[photo_idx + 2] = 230;

        smart_invert_rgba(&mut rgba, width, height, &[[4.0, 4.0, 12.0, 10.0]]);

        // Black ink turned light
        assert!(rgba[0] > 210 && rgba[1] > 210 && rgba[2] > 210);
        // White paper at (39, 19) turned dark gray
        let paper_idx = (19 * width as usize + 39) * 4;
        assert!(rgba[paper_idx] < 55 && rgba[paper_idx + 1] < 55 && rgba[paper_idx + 2] < 55);
        // Photo pixel inside preserve rect remained untouched
        assert_eq!(&rgba[photo_idx..photo_idx + 4], &[20, 120, 230, 255]);
    }

    #[test]
    fn smart_invert_handles_overlapping_preserve_rects() {
        let width = 40;
        let height = 20;
        let mut rgba = vec![255u8; (width * height * 4) as usize];
        let p1 = (5 * width as usize + 6) * 4;
        rgba[p1] = 10;
        rgba[p1 + 1] = 100;
        rgba[p1 + 2] = 200;
        let p_overlap = (5 * width as usize + 12) * 4;
        rgba[p_overlap] = 50;
        rgba[p_overlap + 1] = 180;
        rgba[p_overlap + 2] = 90;
        let p2 = (5 * width as usize + 18) * 4;
        rgba[p2] = 200;
        rgba[p2 + 1] = 80;
        rgba[p2 + 2] = 30;

        smart_invert_rgba(
            &mut rgba,
            width,
            height,
            &[[4.0, 2.0, 10.0, 10.0], [10.0, 3.0, 10.0, 9.0]],
        );

        assert_eq!(&rgba[p1..p1 + 4], &[10, 100, 200, 255]);
        assert_eq!(&rgba[p_overlap..p_overlap + 4], &[50, 180, 90, 255]);
        assert_eq!(&rgba[p2..p2 + 4], &[200, 80, 30, 255]);
        let outside = (5 * width as usize + 28) * 4;
        assert!(rgba[outside] < 55 && rgba[outside + 1] < 55 && rgba[outside + 2] < 55);
    }
}
