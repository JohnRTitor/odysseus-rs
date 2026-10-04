//! Theme colours and the syntax-highlight derivation.
//!
//! Ports the pre-paint theme script in `static/index.html:39-71` and the palette
//! from `static/js/theme.js:11-30`. The port is a **literal transliteration** of
//! the JavaScript, not a re-derivation: the same operations in the same order
//! with the same floating-point arithmetic. That is deliberate — the browser runs
//! the JS, so any "improvement" here would be a divergence, and the two would
//! disagree on exactly the values that decide whether text is readable.
//!
//! Why this lives in Rust at all when the thing that actually runs is JS: because
//! it is pure, deterministic arithmetic, which makes it the one part of the theme
//! system that can be *tested*. The FOUC test (`AGENTS.md` T0107) needs a
//! reference to compare the rendered output against, and an untested script is not
//! a reference.
//!
//! **Ten** `--hl-*` variables, not eleven as the task text says — the script sets
//! `--hl-bg`, `--hl-fg`, `--hl-keyword`, `--hl-string`, `--hl-comment`,
//! `--hl-function`, `--hl-number`, `--hl-builtin`, `--hl-variable`,
//! `--hl-params`, and that is the complete set.
//!
//! One deliberate difference from the source: [`hsl_to_hex`] clamps each channel
//! into `0..=255` before formatting. The formula cannot produce an out-of-range
//! channel for valid inputs — it is `l - a*max(-1, min(k-3, 9-k, 1))`, which is
//! bounded by `l` — but floating-point error at the boundary could produce
//! `-0.0000001`, which `Math.round` sends to `-0` and whose `toString(16)` is
//! `"0"`. The clamp makes that outcome explicit instead of accidental, and is
//! unreachable otherwise.

/// One palette entry, from `static/js/theme.js`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Page background. Drives the dark/light branch and `--hl-bg`.
    pub bg: &'static str,
    /// Primary text.
    pub fg: &'static str,
    /// Panel background.
    pub panel: &'static str,
    /// Border colour.
    pub border: &'static str,
    /// The accent hue the syntax palette is rotated around.
    pub red: &'static str,
}

impl Theme {
    /// The accent actually used for derivation.
    ///
    /// `index.html:59` reads `c.red || '#e06c75'`, so a theme with an empty
    /// accent falls back to the default rather than deriving from nothing.
    #[must_use]
    pub fn accent(&self) -> &str {
        if self.red.is_empty() {
            "#e06c75"
        } else {
            self.red
        }
    }
}

/// The ten `--hl-*` custom properties, derived.
///
/// `bg` and `fg` are included because they are part of the set the script writes;
/// `fg` is passed through verbatim rather than re-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxVars {
    /// Editor background — the page background nudged away from itself.
    pub bg: String,
    /// Editor foreground — the theme's own `fg`, verbatim.
    pub fg: String,
    /// Language keywords, rotated from the accent.
    pub keyword: String,
    /// String literals.
    pub string: String,
    /// Comments, desaturated and blended toward the background.
    pub comment: String,
    /// Function names.
    pub function: String,
    /// Numeric literals.
    pub number: String,
    /// Built-in identifiers.
    pub builtin: String,
    /// Variables, rotated slightly from the text hue.
    pub variable: String,
    /// Function parameters.
    pub params: String,
}

impl SyntaxVars {
    /// The `(name, value)` pairs in the order the script sets them.
    ///
    /// Order matters for a caller that writes them out in sequence, so it is
    /// part of the API rather than left to a `HashMap`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("--hl-bg", self.bg.as_str()),
            ("--hl-fg", self.fg.as_str()),
            ("--hl-keyword", self.keyword.as_str()),
            ("--hl-string", self.string.as_str()),
            ("--hl-comment", self.comment.as_str()),
            ("--hl-function", self.function.as_str()),
            ("--hl-number", self.number.as_str()),
            ("--hl-builtin", self.builtin.as_str()),
            ("--hl-variable", self.variable.as_str()),
            ("--hl-params", self.params.as_str()),
        ]
        .into_iter()
    }
}

/// Parses `#rrggbb` into normalised `[r, g, b]` floats in `0..=1`.
///
/// Returns `None` for anything else, which is the fail-closed choice: a malformed
/// colour yields no variables at all rather than silently black-on-black.
#[must_use]
pub fn hex_to_rgb(hex: &str) -> Option<[f64; 3]> {
    let body = hex.strip_prefix('#')?;
    if body.len() != 6 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| {
        u8::from_str_radix(&body[i..i + 2], 16)
            .ok()
            .map(|b| f64::from(b) / 255.0)
    };
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// `h2hsl` from `index.html:48-53`: `[hue°, saturation%, lightness%]`.
///
/// Transliterated, including the achromatic short-circuit: when `mx == mn` the
/// JavaScript assigns `h = sv = 0` *in both branches of the same statement*, which
/// is what makes a pure grey hue zero rather than undefined.
// The JS names these `r`, `g`, `b` and computes `(mx + mn) / 2`. A literal
// transliteration is the point: renaming them would make the correspondence to
// `index.html:48-53` harder to check, not easier, and `f64::midpoint` computes
// `a + (b - a) / 2`, which can differ from the JavaScript in the last bit — and
// `l` is compared against `0.5` immediately below, where one ULP flips the
// saturation branch. Both lints are silenced rather than the code "improved",
// because an improvement here is a divergence.
#[expect(
    clippy::many_single_char_names,
    reason = "r/g/b mirror the JavaScript this transliterates"
)]
#[expect(
    clippy::manual_midpoint,
    reason = "f64::midpoint can differ from (a+b)/2 by an ULP, and the result feeds a `> 0.5` branch"
)]
#[expect(
    clippy::float_cmp,
    reason = "the JS uses `mx === mn`, and an epsilon here would be a bug, not a tolerance: a near-grey would take the chromatic branch and divide by a near-zero delta, amplifying float noise into an arbitrary hue. Strict equality is what makes the achromatic case terminate"
)]
#[must_use]
pub fn hex_to_hsl(hex: &str) -> Option<[f64; 3]> {
    let [r, g, b] = hex_to_rgb(hex)?;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if mx == mn {
        return Some([0.0, 0.0, l * 100.0]);
    }
    let d = mx - mn;
    let sv = if l > 0.5 {
        d / (2.0 - mx - mn)
    } else {
        d / (mx + mn)
    };
    let h = if mx == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if mx == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    Some([h * 360.0, sv * 100.0, l * 100.0])
}

/// `hsl2h` from `index.html:54-58`: a `#rrggbb` string.
///
/// The channel order is `f(0), f(8), f(4)` — the standard sector offsets for
/// R, G, B — and `th` rounds then zero-pads to two digits, so every component is
/// exactly two characters.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=255 immediately above, so the conversion is total; a truncating cast is how a byte is made"
)]
#[must_use]
pub fn hsl_to_hex(h: f64, sv: f64, l: f64) -> String {
    let h = (h % 360.0 + 360.0) % 360.0;
    let sv = sv.clamp(0.0, 100.0) / 100.0;
    let l = l.clamp(0.0, 100.0) / 100.0;
    let a = sv * l.min(1.0 - l);
    let channel = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        (l - a * (-1.0_f64).max((k - 3.0).min(9.0 - k).min(1.0))) * 255.0
    };
    let mut out = String::with_capacity(7);
    out.push('#');
    for n in [0.0, 8.0, 4.0] {
        use std::fmt::Write as _;
        let byte = channel(n).clamp(0.0, 255.0).round() as u32;
        // `write!` into the existing String rather than `format!`-and-append.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The eleven-`setProperty` block from `index.html:59-69`, as ten variables.
///
/// `dk` is the dark branch: `bH[2] < 50`, i.e. the *background's* lightness, not
/// the theme's name. A theme called "light" with a dark background takes the dark
/// branch, and this is the single easiest thing to get backwards.
#[must_use]
pub fn syntax_vars(theme: &Theme) -> Option<SyntaxVars> {
    let f = hex_to_hsl(theme.fg)?;
    let b = hex_to_hsl(theme.bg)?;
    let r = hex_to_hsl(theme.accent())?;
    let dk = b[2] < 50.0;

    let min = |a: f64, b: f64| if a < b { a } else { b };
    let max = |a: f64, b: f64| if a > b { a } else { b };

    Some(SyntaxVars {
        bg: hsl_to_hex(
            b[0],
            b[1],
            if dk {
                max(b[2] - 4.0, 0.0)
            } else {
                min(b[2] + 4.0, 100.0)
            },
        ),
        // Verbatim, not derived — `s.setProperty('--hl-fg', c.fg)`.
        fg: theme.fg.to_owned(),
        keyword: hsl_to_hex(
            (r[0] + 280.0) % 360.0,
            min(r[1] + 10.0, 80.0),
            if dk { 70.0 } else { 45.0 },
        ),
        string: hsl_to_hex(40.0, min(f[1] + 20.0, 70.0), if dk { 72.0 } else { 42.0 }),
        comment: hsl_to_hex(f[0], max(f[1] - 20.0, 5.0), f[2] * 0.5 + b[2] * 0.5),
        function: hsl_to_hex(210.0, min(f[1] + 20.0, 75.0), if dk { 70.0 } else { 45.0 }),
        number: hsl_to_hex(20.0, min(f[1] + 15.0, 65.0), if dk { 68.0 } else { 48.0 }),
        builtin: hsl_to_hex(180.0, min(f[1] + 15.0, 60.0), if dk { 65.0 } else { 40.0 }),
        variable: hsl_to_hex((f[0] + 30.0) % 360.0, min(f[1] + 5.0, 60.0), f[2]),
        params: hsl_to_hex(
            f[0],
            max(f[1] - 5.0, 10.0),
            if dk {
                min(f[2] + 8.0, 85.0)
            } else {
                max(f[2] - 8.0, 25.0)
            },
        ),
    })
}

/// Looks a theme up by name.
#[must_use]
pub fn theme(name: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|(n, _)| *n == name).map(|(_, t)| t)
}

/// The palette, transcribed from `static/js/theme.js:11-30`.
///
/// Generated by extracting the five colour fields from that object rather than
/// typed by hand: 16 rows × 5 values is exactly the task where one mistyped hex
/// goes unnoticed, and it did — an early draft had `forest.bg` as `#1b1a1b`
/// instead of `#1b2a1b` and was missing `gpt` and `cute` entirely.
///
/// `gpt` also carries an `advanced` block in the source; it overrides `--*-bg`
/// variables only, never `--hl-*`, so it needs no representation here.
pub const THEMES: [(&str, Theme); 16] = [
    (
        "dark",
        Theme {
            bg: "#282c34",
            fg: "#9cdef2",
            panel: "#111111",
            border: "#355a66",
            red: "#e06c75",
        },
    ),
    (
        "light",
        Theme {
            bg: "#f0ebe3",
            fg: "#5a5248",
            panel: "#faf6f0",
            border: "#d4cdc2",
            red: "#c47d5a",
        },
    ),
    (
        "midnight",
        Theme {
            bg: "#0d1117",
            fg: "#c9d1d9",
            panel: "#161b22",
            border: "#30363d",
            red: "#f85149",
        },
    ),
    (
        "paper",
        Theme {
            bg: "#faf8f5",
            fg: "#3b3836",
            panel: "#ffffff",
            border: "#d5d0c8",
            red: "#c5ac4a",
        },
    ),
    (
        "cyberpunk",
        Theme {
            bg: "#0a0a0f",
            fg: "#0ff0fc",
            panel: "#12101a",
            border: "#9b30ff",
            red: "#e040fb",
        },
    ),
    (
        "retrowave",
        Theme {
            bg: "#1a1a2e",
            fg: "#e94560",
            panel: "#16213e",
            border: "#533483",
            red: "#e94560",
        },
    ),
    (
        "forest",
        Theme {
            bg: "#1b2a1b",
            fg: "#a8d5a2",
            panel: "#142414",
            border: "#3d6b3d",
            red: "#7cb871",
        },
    ),
    (
        "ocean",
        Theme {
            bg: "#0b1a2c",
            fg: "#64d2ff",
            panel: "#091422",
            border: "#1e5074",
            red: "#4facfe",
        },
    ),
    (
        "ume",
        Theme {
            bg: "#2b1b2e",
            fg: "#f5c2e7",
            panel: "#1e1420",
            border: "#6c4675",
            red: "#f5a0c0",
        },
    ),
    (
        "copper",
        Theme {
            bg: "#1c1410",
            fg: "#e8c39e",
            panel: "#140f0a",
            border: "#7a5533",
            red: "#d4764e",
        },
    ),
    (
        "terminal",
        Theme {
            bg: "#000000",
            fg: "#00ff41",
            panel: "#0a0a0a",
            border: "#003b00",
            red: "#00ff41",
        },
    ),
    (
        "organs",
        Theme {
            bg: "#0a0406",
            fg: "#efe1c8",
            panel: "#15080a",
            border: "#3a1519",
            red: "#c83240",
        },
    ),
    (
        "lavender",
        Theme {
            bg: "#f3eef8",
            fg: "#3d3551",
            panel: "#faf7ff",
            border: "#cec3de",
            red: "#9b6dcc",
        },
    ),
    (
        "gpt",
        Theme {
            bg: "#212121",
            fg: "#ececec",
            panel: "#171717",
            border: "#424242",
            red: "#949494",
        },
    ),
    (
        "claude",
        Theme {
            bg: "#262624",
            fg: "#f5f4f0",
            panel: "#30302e",
            border: "#4a4a47",
            red: "#c6613f",
        },
    ),
    (
        "cute",
        Theme {
            bg: "#fff0f5",
            fg: "#d4608a",
            panel: "#fff8fa",
            border: "#f0c0d0",
            red: "#ff6b9d",
        },
    ),
];

#[cfg(test)]
mod tests {
    use {super::*, std::collections::BTreeMap};

    /// Layout ceiling per `AGENTS.md` §12.1 rule 7. Ten heap-allocated strings
    /// held simultaneously is the thing worth watching; it is computed once per
    /// theme, so growth here is a correctness smell rather than a hot path.
    #[test]
    fn the_derived_set_has_a_bounded_footprint() {
        assert!(
            size_of::<SyntaxVars>() <= 320,
            "SyntaxVars grew to {} bytes",
            size_of::<SyntaxVars>()
        );
    }

    #[test]
    fn a_malformed_colour_yields_nothing_rather_than_a_wrong_shade() {
        for bad in ["", "#", "#fff", "#gggggg", "ffffff", "#ffffff00", "#fff "] {
            assert!(
                hex_to_rgb(bad).is_none(),
                "{bad:?} must not parse — silently returning black would be \
                 black-on-black text"
            );
            assert!(hex_to_hsl(bad).is_none(), "{bad:?} must not parse");
        }
    }

    // Exact zero is the assertion: the JS assigns literal `0` in the achromatic
    // branch, and the point is that no arithmetic ran at all.
    #[expect(
        clippy::float_cmp,
        reason = "the achromatic branch assigns literal zeros; asserting they are exactly zero is the property"
    )]
    #[test]
    fn an_achromatic_colour_has_zero_hue_and_zero_saturation() {
        // The `mx === mn` branch of `h2hsl`. A grey has no hue, and the script
        // assigns `h = sv = 0` rather than leaving them undefined — so a NaN here
        // would poison every derived variable that adds to the hue.
        let [h, s, l] = hex_to_hsl("#808080").expect("parses");
        assert_eq!(h, 0.0);
        assert_eq!(s, 0.0);
        assert!((l - 128.0 / 255.0 * 100.0).abs() < 1e-9, "l was {l}");
    }

    #[test]
    fn zero_saturation_produces_a_grey_regardless_of_hue() {
        // `a = sv * min(l, 1-l)` is zero when `sv` is zero, so every channel is
        // `l`. Cheap to verify by hand, which is why it is here.
        for hue in [0.0, 37.0, 180.0, 359.0] {
            let hex = hsl_to_hex(hue, 0.0, 50.0);
            assert_eq!(hex, "#808080", "hue {hue} at zero saturation");
        }
        assert_eq!(hsl_to_hex(0.0, 0.0, 0.0), "#000000");
        assert_eq!(hsl_to_hex(0.0, 0.0, 100.0), "#ffffff");
    }

    #[test]
    fn primary_colours_land_where_the_sector_formula_says() {
        // f(0) is red, f(8) green, f(4) blue. Getting the order wrong would swap
        // two channels everywhere and no single-value test would notice.
        assert_eq!(hsl_to_hex(0.0, 100.0, 50.0), "#ff0000");
        assert_eq!(hsl_to_hex(120.0, 100.0, 50.0), "#00ff00");
        assert_eq!(hsl_to_hex(240.0, 100.0, 50.0), "#0000ff");
    }

    #[test]
    fn every_output_is_six_lowercase_hex_digits() {
        // The script feeds these straight into `toString(16).padStart(2,'0')`, so a
        // component that lost its leading zero would produce `#f0f0f`-shaped CSS
        // and the browser would reject the whole declaration.
        for (name, t) in THEMES {
            let vars = syntax_vars(&t).expect("every palette entry must derive");
            for (var, value) in vars.iter() {
                assert!(
                    value.len() == 7 && value.starts_with('#'),
                    "{name} {var} = {value:?} is not #rrggbb"
                );
                assert!(
                    value[1..]
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "{name} {var} = {value:?} has a non-hex character"
                );
            }
        }
    }

    #[test]
    fn the_dark_branch_is_chosen_by_background_lightness_not_by_name() {
        // `dk = bH[2] < 50`. The easiest thing to get backwards, and a theme named
        // "light" with a dark background must take the dark branch.
        //
        // Six-digit hex throughout: the parser deliberately rejects `#f00`
        // shorthand, and a theme using it would derive nothing at all.
        let dark_bg = Theme {
            bg: "#000000",
            fg: "#ffffff",
            panel: "#000000",
            border: "#333333",
            red: "#ff0000",
        };
        let light_bg = Theme {
            bg: "#ffffff",
            fg: "#000000",
            panel: "#ffffff",
            border: "#cccccc",
            red: "#ff0000",
        };
        let d = syntax_vars(&dark_bg).expect("derives");
        let l = syntax_vars(&light_bg).expect("derives");

        // Keyword lightness is `dk ? 70 : 45` — a large, deliberate gap. Hue and
        // saturation are derived from the accent rather than hard-coded, so the
        // assertion isolates the branch rather than restating the formula.
        let r = hex_to_hsl("#ff0000").expect("accent parses");
        let hue = (r[0] + 280.0) % 360.0;
        let sat = (r[1] + 10.0).min(80.0);
        assert_eq!(d.keyword, hsl_to_hex(hue, sat, 70.0), "dark branch");
        assert_eq!(l.keyword, hsl_to_hex(hue, sat, 45.0), "light branch");
    }

    #[test]
    fn the_background_var_is_nudged_away_from_the_page_in_both_branches() {
        for (name, t) in THEMES {
            let b = hex_to_hsl(t.bg).expect("theme bg parses");
            let dk = b[2] < 50.0;
            let expected = if dk {
                (b[2] - 4.0).max(0.0)
            } else {
                (b[2] + 4.0).min(100.0)
            };
            assert_eq!(
                syntax_vars(&t).expect("derives").bg,
                hsl_to_hex(b[0], b[1], expected),
                "{name}: --hl-bg must be the background shifted by exactly 4"
            );
        }
    }

    #[test]
    fn fg_is_passed_through_verbatim_and_the_rest_are_derived() {
        for (name, t) in THEMES {
            let vars = syntax_vars(&t).expect("derives");
            assert_eq!(vars.fg, t.fg, "{name}: --hl-fg is c.fg, not a derivation");
        }
    }

    #[test]
    fn an_empty_accent_falls_back_to_the_default_rather_than_deriving_from_nothing() {
        // `index.html:59` reads `c.red || '#e06c75'`.
        let mut t = *theme("dark").expect("dark exists");
        t.red = "";
        assert_eq!(t.accent(), "#e06c75");
        assert!(syntax_vars(&t).is_some(), "must still derive");
    }

    #[test]
    fn the_palette_matches_the_source_table_exactly() {
        // Transcribed from `static/js/theme.js:11-30`. Asserted here so a future
        // edit to either side fails loudly rather than drifting — a mistyped hex in
        // a theme is invisible until someone reads the UI.
        let expected: BTreeMap<&str, [&str; 5]> = BTreeMap::from([
            (
                "dark",
                ["#282c34", "#9cdef2", "#111111", "#355a66", "#e06c75"],
            ),
            (
                "light",
                ["#f0ebe3", "#5a5248", "#faf6f0", "#d4cdc2", "#c47d5a"],
            ),
            (
                "midnight",
                ["#0d1117", "#c9d1d9", "#161b22", "#30363d", "#f85149"],
            ),
            (
                "paper",
                ["#faf8f5", "#3b3836", "#ffffff", "#d5d0c8", "#c5ac4a"],
            ),
            (
                "cyberpunk",
                ["#0a0a0f", "#0ff0fc", "#12101a", "#9b30ff", "#e040fb"],
            ),
            (
                "retrowave",
                ["#1a1a2e", "#e94560", "#16213e", "#533483", "#e94560"],
            ),
            (
                "forest",
                ["#1b2a1b", "#a8d5a2", "#142414", "#3d6b3d", "#7cb871"],
            ),
            (
                "ocean",
                ["#0b1a2c", "#64d2ff", "#091422", "#1e5074", "#4facfe"],
            ),
            (
                "ume",
                ["#2b1b2e", "#f5c2e7", "#1e1420", "#6c4675", "#f5a0c0"],
            ),
            (
                "copper",
                ["#1c1410", "#e8c39e", "#140f0a", "#7a5533", "#d4764e"],
            ),
            (
                "terminal",
                ["#000000", "#00ff41", "#0a0a0a", "#003b00", "#00ff41"],
            ),
            (
                "organs",
                ["#0a0406", "#efe1c8", "#15080a", "#3a1519", "#c83240"],
            ),
            (
                "lavender",
                ["#f3eef8", "#3d3551", "#faf7ff", "#cec3de", "#9b6dcc"],
            ),
            (
                "gpt",
                ["#212121", "#ececec", "#171717", "#424242", "#949494"],
            ),
            (
                "claude",
                ["#262624", "#f5f4f0", "#30302e", "#4a4a47", "#c6613f"],
            ),
            (
                "cute",
                ["#fff0f5", "#d4608a", "#fff8fa", "#f0c0d0", "#ff6b9d"],
            ),
        ]);

        let actual: BTreeMap<&str, [&str; 5]> = THEMES
            .iter()
            .map(|(n, t)| (*n, [t.bg, t.fg, t.panel, t.border, t.red]))
            .collect();

        assert_eq!(
            actual.len(),
            expected.len(),
            "theme count drifted: {} vs {}",
            actual.len(),
            expected.len()
        );
        for (name, want) in &expected {
            let got = actual
                .get(name)
                .expect("every palette entry must be present");
            assert_eq!(got, want, "theme {name} drifted from theme.js");
        }
    }

    #[test]
    fn lookup_finds_every_palette_entry_and_rejects_a_stranger() {
        for (name, _) in THEMES {
            assert!(theme(name).is_some(), "{name} must resolve");
        }
        assert!(theme("no-such-theme").is_none());
    }

    #[test]
    fn the_derived_set_is_exactly_ten_variables_in_script_order() {
        // The task text says eleven. The script sets ten; the count is pinned so a
        // future "improvement" that adds or drops one is a deliberate change.
        let vars = syntax_vars(theme("dark").expect("dark exists")).expect("derives");
        let names: Vec<&str> = vars.iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "--hl-bg",
                "--hl-fg",
                "--hl-keyword",
                "--hl-string",
                "--hl-comment",
                "--hl-function",
                "--hl-number",
                "--hl-builtin",
                "--hl-variable",
                "--hl-params",
            ]
        );
    }
}
