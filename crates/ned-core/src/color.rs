//! Theme colours: parsing, diff tinting and the 256-colour fallback
//! (command-language spec, §6.6).

use std::cell::RefCell;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::LazyLock;

use crate::hint::{self, Fix, Hint};

/// An sRGB colour, written `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A colour that isn't written `#rrggbb`.
pub type BadColor = hint::Error<BadColorKind>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("expected a colour `#rrggbb`, found `{0}`")]
pub struct BadColorKind(pub String);

impl Hint for BadColorKind {
    /// As for any invalid config.
    fn exit_code(&self) -> u8 {
        2
    }

    fn fix(&self) -> Option<Fix> {
        Some("write `#` and six hex digits, such as `#61afef`".into())
    }
}

impl FromStr for Rgb {
    type Err = BadColor;

    fn from_str(s: &str) -> Result<Self, BadColor> {
        let hex = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        let channel = |i: usize| hex.map(|h| u8::from_str_radix(&h[i..i + 2], 16).unwrap());
        match (channel(0), channel(2), channel(4)) {
            (Some(r), Some(g), Some(b)) => Ok(Rgb { r, g, b }),
            _ => Err(BadColorKind(s.to_string()).into()),
        }
    }
}

/// `base` tinted by a diff line's colour: each channel multiplied, then, for a
/// dark theme, brought back to `base`'s OKLab lightness.
pub fn tint(base: Rgb, by: Rgb, dark: bool) -> Rgb {
    if !dark {
        let mul = |a: u8, b: u8| ((u32::from(a) * u32::from(b) + 127) / 255) as u8;
        return Rgb {
            r: mul(base.r, by.r),
            g: mul(base.g, by.g),
            b: mul(base.b, by.b),
        };
    }
    let mul = |a: u8, b: u8| decode(a) * decode(b);
    let [_, a, b] = oklab_of([mul(base.r, by.r), mul(base.g, by.g), mul(base.b, by.b)]);
    from_oklab([oklab(base)[0], a, b])
}

/// The xterm-256 index (16-255: the colour cube or grey ramp) nearest `c` in
/// OKLab.
pub fn nearest_256(c: Rgb) -> u8 {
    static PALETTE: LazyLock<Vec<(u8, [f64; 3])>> =
        LazyLock::new(|| (16..=255).map(|i| (i, oklab(xterm(i)))).collect());
    thread_local! {
        // A theme paints with few colours, but every token asks again.
        static NEAREST: RefCell<HashMap<Rgb, u8>> = RefCell::default();
    }
    NEAREST.with_borrow_mut(|nearest| {
        *nearest.entry(c).or_insert_with(|| {
            let lab = oklab(c);
            let dist = |p: &[f64; 3]| (0..3).map(|k| (p[k] - lab[k]).powi(2)).sum::<f64>();
            PALETTE
                .iter()
                .min_by(|(_, x), (_, y)| dist(x).total_cmp(&dist(y)))
                .map(|(i, _)| *i)
                .unwrap()
        })
    })
}

/// The colour of xterm-256 index `i`, for `i` from 16.
fn xterm(i: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        return Rgb { r: v, g: v, b: v };
    }
    let i = usize::from(i - 16);
    Rgb {
        r: LEVELS[i / 36],
        g: LEVELS[i / 6 % 6],
        b: LEVELS[i % 6],
    }
}

/// An sRGB channel's value, from 0 to 1, as written (gamma-encoded).
fn decode(c: u8) -> f64 {
    f64::from(c) / 255.0
}

fn oklab(c: Rgb) -> [f64; 3] {
    oklab_of([decode(c.r), decode(c.g), decode(c.b)])
}

// Björn Ottosson's OKLab, from gamma-encoded sRGB channels.
fn oklab_of(srgb: [f64; 3]) -> [f64; 3] {
    let lin = srgb.map(|c| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let [r, g, b] = lin;
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// The sRGB colour of an OKLab one, clamped to the gamut.
fn from_oklab([lightness, a, b]: [f64; 3]) -> Rgb {
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let lin = [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ];
    let [r, g, b] = lin.map(|c| {
        let c = c.clamp(0.0, 1.0);
        let e = if c <= 0.0031308 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as u8
    });
    Rgb { r, g, b }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hint::Frontend;

    fn rgb(s: &str) -> Rgb {
        s.parse().unwrap()
    }

    fn lightness(c: Rgb) -> f64 {
        oklab(c)[0]
    }

    #[test]
    fn parses_hex_colours() {
        assert_eq!(
            rgb("#98c379"),
            Rgb {
                r: 0x98,
                g: 0xc3,
                b: 0x79
            }
        );
        assert_eq!(rgb("#98C379"), rgb("#98c379"));
        assert_eq!(rgb("#000000"), Rgb { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn rejects_anything_but_six_hex_digits() {
        for bad in [
            "", "#", "98c379", "#98c37", "#98c3790", "#gggggg", "#fff", " #98c379", "#+1+2+3",
        ] {
            assert_eq!(
                bad.parse::<Rgb>(),
                Err(BadColorKind(bad.to_string()).into()),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_bad_colour_says_how_to_write_one() {
        assert_eq!(
            "#fff"
                .parse::<Rgb>()
                .unwrap_err()
                .render(Frontend::Cli, None),
            "error: expected a colour `#rrggbb`, found `#fff`; write `#` and six hex digits, such as `#61afef`"
        );
    }

    #[test]
    fn a_light_theme_multiplies_each_channel() {
        assert_eq!(tint(rgb("#ff8000"), rgb("#80ff80"), false), rgb("#808000"));
        // 192 × 128 / 255 = 96.38
        assert_eq!(tint(rgb("#c0c0c0"), rgb("#808080"), false), rgb("#606060"));
        assert_eq!(tint(rgb("#61afef"), rgb("#000000"), false), rgb("#000000"));
    }

    #[test]
    fn white_leaves_a_colour_alone() {
        for dark in [false, true] {
            for c in ["#61afef", "#c678dd", "#000000", "#ffffff", "#7f7f7f"] {
                assert_eq!(
                    tint(rgb(c), rgb("#ffffff"), dark),
                    rgb(c),
                    "{c}, dark: {dark}"
                );
            }
        }
    }

    #[test]
    fn a_light_theme_never_lightens() {
        for (base, by) in [
            ("#61afef", "#98c379"),
            ("#c678dd", "#e06c75"),
            ("#383a42", "#50a14f"),
        ] {
            let t = tint(rgb(base), rgb(by), false);
            assert!(
                lightness(t) <= lightness(rgb(base)),
                "{base} by {by}: {t:?}"
            );
        }
    }

    #[test]
    fn a_dark_theme_keeps_the_token_lightness() {
        for (base, by) in [
            ("#61afef", "#98c379"),
            ("#c678dd", "#e06c75"),
            ("#abb2bf", "#98c379"),
            ("#ff0000", "#000000"),
        ] {
            let t = tint(rgb(base), rgb(by), true);
            let (want, got) = (lightness(rgb(base)), lightness(t));
            assert!(
                (want - got).abs() < 0.02,
                "{base} by {by}: {t:?}, L {got} not {want}"
            );
        }
    }

    #[test]
    fn a_dark_theme_takes_the_tint_hue() {
        let t = tint(rgb("#aaaaaa"), rgb("#00ff00"), true);
        assert!(t.g > t.r && t.g > t.b, "{t:?}");
        let t = tint(rgb("#aaaaaa"), rgb("#ff0000"), true);
        assert!(t.r > t.g && t.r > t.b, "{t:?}");
    }

    #[test]
    fn a_dark_theme_tinted_by_black_is_a_grey_of_the_same_lightness() {
        let t = tint(rgb("#ff0000"), rgb("#000000"), true);
        assert!(t.r == t.g && t.g == t.b, "{t:?}");
    }

    #[test]
    fn palette_colours_map_to_themselves() {
        assert_eq!(nearest_256(rgb("#000000")), 16);
        assert_eq!(nearest_256(rgb("#ffffff")), 231);
        assert_eq!(nearest_256(rgb("#ff0000")), 196);
        assert_eq!(nearest_256(rgb("#5f87af")), 67);
        assert_eq!(nearest_256(rgb("#080808")), 232);
        assert_eq!(nearest_256(rgb("#808080")), 244);
        assert_eq!(nearest_256(rgb("#eeeeee")), 255);
    }

    #[test]
    fn other_colours_map_to_the_nearest() {
        assert_eq!(nearest_256(rgb("#070707")), 232);
        assert_eq!(nearest_256(rgb("#f00000")), 196);
        assert_eq!(nearest_256(rgb("#5c8ab0")), 67);
        assert_eq!(nearest_256(rgb("#7a7a7a")), 243);
    }
}
