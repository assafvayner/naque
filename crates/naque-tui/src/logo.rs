//! Pixel-art "naque" logo, ported from the reference `naque.py` splash script.
//!
//! The jaguar wordmark is a fixed pixel grid whose gold coat is re-speckled with
//! dark rosette spots and a few magenta accents from a per-session seed, then
//! rendered with Unicode half-blocks so two vertical pixels share one text cell.
//! The same seed also tints the single-glyph "N" mark shown in the status bar,
//! so the splash and the status mark share one look within a session.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The NAQUE wordmark: 18 rows × 77 columns. `.` is transparent; every other
/// char is a hex color index into the 16-color ANSI palette (`b` gold coat,
/// `8` dark spot, `3` amber spot, `d` magenta accent — `d` is folded back into
/// the coat before speckling so every magenta dot is seed-chosen).
const WORDMARK: [&str; 18] = [
    ".............................................................................",
    ".............................................................................",
    "....b.......b......8b83bb883......bbb888bbb......8.......b......3b8b8b3b8....",
    "...3bb.....bbb....b888bbbbbbb....bbbbbbbbbbb....bbb.....3bb....bbb888bbbbb...",
    "...bbbbb...bbb....bbbbbbbbb3b....b33bb3bbbbb....3bb.....bbb....bbbbbbbbb3....",
    "...bbbbb...bbb....bb8.....b8b....bbb.....bbb....bbb.....bb3....3b3...........",
    "...bbbbbb..bb8....bb8.....d8b....bb3.....bb8....88b.....8bb....8bb...........",
    "...8bbbbb..bb8....bb8.....b8b....bbb.....3b8....8d8.....8bb....8bb...........",
    "...8bbbb8b.3b8....bbb3bb3bbbb....bbb.....bb8....88b.....8bb....8bbbdbbbb.....",
    "...bbb.b88bbbb....bbbbb3bbbbb....b3b.....bbb....bbb.....bbb....bbb888bbbb....",
    "...b3b..bbbbbb....bbb333b3bbb....3bb.....bbb....bbb.....b3b....bbbbbb3bb.....",
    "...bbb..bbbbbb....bbb.....bbb....bbb....bbbb....bbb.....88b....bb3...........",
    "...bbb...bbbbb....b88.....3bb....8b3....bbb8....bbb.....bbb....88b...........",
    "...bbb...bbbbb....b8b.....b8b....8bbb888bbb8....bbb8bbbb88b....d8bbb8b83b....",
    "...bbb.....bbb....b88.....b8b....8bbbbbbbbb8....b8dbbbbbbbb....88bbb888bb8...",
    "....b.......b......b.......8......bbb3bb3bb......b8bbbbbbb......bbbbbbbbb....",
    ".............................................................................",
    ".............................................................................",
];

/// Probability a coat pixel becomes a dark/amber rosette spot.
const SPOT_CHANCE: f64 = 0.25;
/// Fraction of coat pixels recolored to magenta accents.
const MAGENTA_FRACTION: f64 = 0.03;

/// Upper-half block (top pixel only). Lower-half block. Full block.
const UPPER: &str = "\u{2580}";
const LOWER: &str = "\u{2584}";
const FULL: &str = "\u{2588}";

/// SplitMix64 — a tiny deterministic PRNG used to speckle the coat from a seed.
/// Statistical quality is irrelevant here; we only need reproducible, well-spread
/// noise without pulling in an external crate.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float in `[0, 1)` with 53 bits of mantissa.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform index in `0..n` (caller guarantees `n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Map a pixel char to a ratatui color, or `None` for the transparent `.`.
/// Chars are hex color indices into the 16-color ANSI palette.
fn pixel_color(ch: char) -> Option<Color> {
    if ch == '.' {
        return None;
    }
    ch.to_digit(16).map(|idx| ansi16(idx as u8))
}

/// The 16-color ANSI palette by index (0–7 standard, 8–15 bright).
fn ansi16(idx: u8) -> Color {
    match idx {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Re-speckle the gold coat from `rng`: each coat pixel has a `SPOT_CHANCE` of
/// becoming a dark/amber rosette spot, then ~`MAGENTA_FRACTION` of the remaining
/// coat pixels are recolored to magenta accents. Mirrors `speckle` in `naque.py`
/// (hand-placed `d` is folded back into the coat first).
fn speckle(grid: &[&str], rng: &mut SplitMix64) -> Vec<Vec<char>> {
    let mut rows: Vec<Vec<char>> = grid
        .iter()
        .map(|row| row.chars().map(|c| if c == 'd' { 'b' } else { c }).collect())
        .collect();

    let coat: Vec<(usize, usize)> = rows
        .iter()
        .enumerate()
        .flat_map(|(y, row)| row.iter().enumerate().filter_map(move |(x, &c)| (c == 'b').then_some((y, x))))
        .collect();

    for &(y, x) in &coat {
        if rng.next_f64() < SPOT_CHANCE {
            rows[y][x] = if rng.below(2) == 0 { '8' } else { '3' };
        }
    }

    let n_magenta = (coat.len() as f64 * MAGENTA_FRACTION).round() as usize;
    let mut free: Vec<(usize, usize)> = coat.into_iter().filter(|&(y, x)| rows[y][x] == 'b').collect();
    let k = n_magenta.min(free.len());
    // Partial Fisher–Yates: pick `k` distinct coat pixels without replacement.
    for i in 0..k {
        let j = i + rng.below(free.len() - i);
        free.swap(i, j);
        let (y, x) = free[i];
        rows[y][x] = 'd';
    }

    rows
}

/// Nearest-neighbor integer upscale: each pixel becomes a `factor`×`factor`
/// block. `factor <= 1` returns the grid unchanged.
fn upscale(grid: &[Vec<char>], factor: usize) -> Vec<Vec<char>> {
    if factor <= 1 {
        return grid.to_vec();
    }
    let mut out = Vec::with_capacity(grid.len() * factor);
    for row in grid {
        let big: Vec<char> = row.iter().flat_map(|&c| std::iter::repeat_n(c, factor)).collect();
        for _ in 0..factor {
            out.push(big.clone());
        }
    }
    out
}

/// Render a pixel grid to half-block text lines: each text row packs two pixel
/// rows (`▀` upper, `▄` lower, fg+bg for both). A trailing odd row is dropped
/// (matches `render_half`). With `color` off, a both-filled cell becomes a full
/// block so the silhouette survives without foreground/background colors.
fn render_lines(grid: &[Vec<char>], color: bool) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(grid.len() / 2);
    let mut y = 0;
    while y + 1 < grid.len() {
        let top = &grid[y];
        let bot = &grid[y + 1];
        let width = top.len().max(bot.len());
        let mut spans = Vec::with_capacity(width);
        for x in 0..width {
            let t = top.get(x).copied().and_then(pixel_color);
            let b = bot.get(x).copied().and_then(pixel_color);
            let span = match (t, b) {
                (None, None) => Span::raw(" "),
                (Some(t), None) => Span::styled(UPPER, fg(t, color)),
                (None, Some(b)) => Span::styled(LOWER, fg(b, color)),
                (Some(t), Some(b)) if color => Span::styled(UPPER, Style::default().fg(t).bg(b)),
                (Some(_), Some(_)) => Span::raw(FULL),
            };
            spans.push(span);
        }
        out.push(Line::from(spans));
        y += 2;
    }
    out
}

fn fg(c: Color, color: bool) -> Style {
    if color {
        Style::default().fg(c)
    } else {
        Style::default()
    }
}

/// A seed derived from the wall clock — different on essentially every launch.
fn entropy_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A per-session logo: a speckled wordmark grid plus a status-bar "N" mark,
/// both derived from one random seed.
#[derive(Clone)]
pub struct Logo {
    seed: u64,
    /// Speckled wordmark pixel grid (rows of color-index chars).
    wordmark: Vec<Vec<char>>,
    /// Color tinting the status-bar "N" glyph: gold coat, or a seed-chosen accent.
    mark_color: Color,
}

impl Logo {
    /// Build a logo from an explicit seed. Deterministic — used in tests and as
    /// the default app logo before a random one is assigned.
    pub fn new(seed: u64) -> Self {
        let mut rng = SplitMix64::new(seed);
        let wordmark = speckle(&WORDMARK, &mut rng);
        // Draw the mark accent from the same stream so the status "N" shares the
        // session palette: mostly gold, sometimes amber or magenta — never a dark
        // spot, which would be illegible on the status bar.
        let mark_color = {
            let roll = rng.next_f64();
            if roll < 0.15 {
                Color::LightMagenta
            } else if roll < 0.35 {
                Color::Yellow
            } else {
                Color::LightYellow
            }
        };
        Self {
            seed,
            wordmark,
            mark_color,
        }
    }

    /// Build a logo from a fresh random seed (a new look each session).
    pub fn from_entropy() -> Self {
        Self::new(entropy_seed())
    }

    /// The seed this logo was built from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The wordmark as half-block text lines, upscaled 2× when `max_width`
    /// allows (mirrors `naque.py --word`), else natural size.
    pub fn wordmark_lines(&self, max_width: u16, color: bool) -> Vec<Line<'static>> {
        let grid = upscale(&self.wordmark, self.scale_factor(max_width));
        render_lines(&grid, color)
    }

    /// The status-bar "N" mark: a single bold glyph in the seed's accent color.
    pub fn mark_span(&self, color: bool) -> Span<'static> {
        let style = if color {
            Style::default().fg(self.mark_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        Span::styled("N", style)
    }

    fn scale_factor(&self, max_width: u16) -> usize {
        let base = self.wordmark.first().map_or(0, Vec::len);
        if base > 0 && (max_width as usize) >= base * 2 {
            2
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(grid: &[Vec<char>]) -> String {
        grid.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn speckle_is_deterministic_for_a_seed() {
        let a = flatten(&speckle(&WORDMARK, &mut SplitMix64::new(42)));
        let b = flatten(&speckle(&WORDMARK, &mut SplitMix64::new(42)));
        assert_eq!(a, b, "same seed must produce the same speckle");
    }

    #[test]
    fn different_seeds_usually_differ() {
        let a = flatten(&speckle(&WORDMARK, &mut SplitMix64::new(1)));
        let b = flatten(&speckle(&WORDMARK, &mut SplitMix64::new(999_999)));
        assert_ne!(a, b, "distinct seeds should produce distinct speckle");
    }

    #[test]
    fn speckle_preserves_transparency_and_dimensions() {
        let out = speckle(&WORDMARK, &mut SplitMix64::new(7));
        assert_eq!(out.len(), WORDMARK.len());
        for (y, row) in WORDMARK.iter().enumerate() {
            assert_eq!(out[y].len(), row.chars().count(), "row {y} width changed");
            for (x, c) in row.chars().enumerate() {
                if c == '.' {
                    assert_eq!(out[y][x], '.', "transparent pixel at {y},{x} was recolored");
                }
            }
        }
    }

    #[test]
    fn speckle_adds_spots_and_magenta_accents() {
        let out = speckle(&WORDMARK, &mut SplitMix64::new(123));
        let flat: String = flatten(&out);
        assert!(flat.contains('8') || flat.contains('3'), "expected rosette spots");
        let magenta = flat.matches('d').count();
        let coat = WORDMARK
            .iter()
            .flat_map(|r| r.chars())
            .filter(|&c| c == 'b' || c == 'd')
            .count();
        let expected = (coat as f64 * MAGENTA_FRACTION).round() as usize;
        assert_eq!(magenta, expected, "magenta accent count should match the fraction");
    }

    #[test]
    fn upscale_doubles_each_axis() {
        let grid = vec![vec!['a', 'b']];
        let big = upscale(&grid, 2);
        assert_eq!(big.len(), 2);
        assert_eq!(big[0], vec!['a', 'a', 'b', 'b']);
        assert_eq!(big[0], big[1]);
        // factor 1 is a no-op clone.
        assert_eq!(upscale(&grid, 1), grid);
    }

    #[test]
    fn pixel_color_maps_palette() {
        assert_eq!(pixel_color('.'), None);
        assert_eq!(pixel_color('b'), Some(Color::LightYellow));
        assert_eq!(pixel_color('8'), Some(Color::DarkGray));
        assert_eq!(pixel_color('3'), Some(Color::Yellow));
        assert_eq!(pixel_color('d'), Some(Color::LightMagenta));
    }

    #[test]
    fn wordmark_lines_pack_two_rows_per_line() {
        let logo = Logo::new(0);
        // 18 pixel rows → 9 half-block lines at natural size (width too small to upscale).
        let lines = logo.wordmark_lines(80, true);
        assert_eq!(lines.len(), 9);
        assert!(lines.iter().all(|l| l.width() > 0));
    }

    #[test]
    fn wordmark_upscales_only_when_width_allows() {
        let logo = Logo::new(0);
        assert_eq!(logo.scale_factor(80), 1, "narrow terminal: natural size");
        assert_eq!(logo.scale_factor(200), 2, "wide terminal: 2x");
        // Upscaled grid has twice the line count.
        assert_eq!(logo.wordmark_lines(200, true).len(), 18);
    }

    #[test]
    fn no_color_lines_carry_no_foreground() {
        let logo = Logo::new(0);
        let lines = logo.wordmark_lines(80, false);
        for line in &lines {
            for span in &line.spans {
                assert!(span.style.fg.is_none(), "no-color wordmark must not set fg");
                assert!(span.style.bg.is_none(), "no-color wordmark must not set bg");
            }
        }
    }

    #[test]
    fn mark_span_is_bold_n_with_seed_color() {
        let logo = Logo::new(0);
        let colored = logo.mark_span(true);
        assert_eq!(colored.content, "N");
        assert!(colored.style.add_modifier.contains(Modifier::BOLD));
        assert!(colored.style.fg.is_some(), "colored mark should carry an accent color");

        let plain = logo.mark_span(false);
        assert_eq!(plain.content, "N");
        assert!(plain.style.add_modifier.contains(Modifier::BOLD));
        assert!(plain.style.fg.is_none(), "no-color mark must not set fg");
    }

    #[test]
    fn mark_color_is_a_legible_accent() {
        // Across many seeds the mark is always gold/amber/magenta — never a dark spot.
        for seed in 0..200u64 {
            let c = Logo::new(seed).mark_color;
            assert!(
                matches!(c, Color::LightYellow | Color::Yellow | Color::LightMagenta),
                "seed {seed} produced an illegible mark color {c:?}"
            );
        }
    }

    #[test]
    fn from_entropy_builds_a_usable_logo() {
        let logo = Logo::from_entropy();
        assert_eq!(logo.wordmark.len(), WORDMARK.len());
        assert!(!logo.wordmark_lines(80, true).is_empty());
    }
}
