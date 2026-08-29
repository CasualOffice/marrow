//! Rendering.
//!
//! A pure function of data ([LLD §7]): `render(data, out, style)`. `--json` is a
//! second renderer over the identical input, not a parallel code path — which
//! is what keeps the human view and the machine view from drifting.
//!
//! [LLD §7]: ../../../docs/LLD.md

use std::io::{IsTerminal, Write};

/// Output style, resolved once from flags, TTY state and the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    pub width: usize,
}

impl Style {
    /// Colour is on only for a real terminal that has not opted out.
    ///
    /// `NO_COLOR` is honoured because it is the convention, and piping is
    /// detected because a pipe consumer must never receive escape codes
    /// ([UX §10]).
    ///
    /// [UX §10]: ../../../docs/UX.md
    pub fn detect(no_color_flag: bool) -> Self {
        let tty = std::io::stdout().is_terminal();
        let no_color_env = std::env::var_os("NO_COLOR").is_some();
        Self {
            color: tty && !no_color_flag && !no_color_env,
            width: std::env::var("COLUMNS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
        }
    }

    pub fn plain() -> Self {
        Self {
            color: false,
            width: 100,
        }
    }

    fn paint(self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn dim(self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn warn(self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn err(self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn ok(self, s: &str) -> String {
        self.paint("32", s)
    }
}

/// Human-readable byte count.
pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    match n {
        _ if n < K => format!("{n:.0} B"),
        _ if n < K * K => format!("{:.0} KB", n / K),
        _ if n < K * K * K => format!("{:.1} MB", n / (K * K)),
        _ => format!("{:.2} GB", n / (K * K * K)),
    }
}

/// Thousands separators. `9435` → `9,435`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Elapsed time, in the unit a human would use.
///
/// Rolls all the way to hours: a sweep interval rendered as "360 min 0 s" is
/// arithmetic the reader has to do, which is the same complaint that made ages
/// relative rather than dates.
pub fn duration(ms: u128) -> String {
    const S: u128 = 1_000;
    const M: u128 = 60 * S;
    const H: u128 = 60 * M;
    match ms {
        _ if ms < S => format!("{ms} ms"),
        _ if ms < M => format!("{:.1} s", ms as f64 / S as f64),
        _ if ms < H => {
            let (m, s) = (ms / M, (ms % M) / S);
            if s == 0 {
                format!("{m} min")
            } else {
                format!("{m} min {s} s")
            }
        }
        _ => {
            let (h, m) = (ms / H, (ms % H) / M);
            if m == 0 {
                format!("{h} h")
            } else {
                format!("{h} h {m} min")
            }
        }
    }
}

/// Truncate the **middle** of a path — both ends carry meaning, so lopping off
/// either one is worse than losing the middle ([UX §10]).
pub fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max < 6 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// An error, rendered as **what happened · what it means · what to do**.
///
/// The headline is human-readable and the code is second, because the code is
/// for grep and for future-you, not for the reader's first glance ([UX §8]).
pub fn error(e: &marrow_core::Error, out: &mut impl Write, style: Style) -> std::io::Result<()> {
    writeln!(out, "{} {}", style.err("✗"), style.bold(e.message()))?;
    writeln!(out, "  {}", style.dim(e.code().as_str()))?;
    if let Some(ctx) = e.context() {
        writeln!(out, "  {}", style.dim(&elide(ctx, style.width - 2)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(9_435), "9,435");
        assert_eq!(count(34_459), "34,459");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn bytes_scale_to_a_readable_unit() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2 KB");
        assert_eq!(bytes(14_800_000), "14.1 MB");
        assert_eq!(bytes(1_450_000_000), "1.35 GB");
    }

    #[test]
    fn paths_are_elided_in_the_middle() {
        // Both ends carry meaning; the middle is the only safe thing to drop.
        let p = "melp/services/vault/src/auth/token.rs";
        let e = elide(p, 20);
        assert_eq!(e.chars().count(), 20);
        assert!(e.starts_with("melp"), "head preserved: {e}");
        assert!(e.ends_with("token.rs"), "tail preserved: {e}");
        assert!(e.contains('…'));
    }

    #[test]
    fn short_strings_are_left_alone() {
        assert_eq!(elide("short.rs", 40), "short.rs");
    }

    #[test]
    fn plain_style_emits_no_escape_codes() {
        // A pipe consumer must never receive escapes.
        let s = Style::plain();
        for painted in [s.dim("x"), s.bold("x"), s.warn("x"), s.err("x"), s.ok("x")] {
            assert_eq!(painted, "x", "plain style must not colour");
        }
    }

    #[test]
    fn durations_use_the_unit_a_human_would() {
        assert_eq!(duration(8), "8 ms");
        assert_eq!(duration(2_400), "2.4 s");
        assert_eq!(duration(125_000), "2 min 5 s");
        assert_eq!(
            duration(120_000),
            "2 min",
            "a whole number of minutes drops the seconds"
        );
        // The bug this pins: a 6-hour sweep interval rendered as "360 min 0 s".
        assert_eq!(duration(6 * 3_600_000), "6 h");
        assert_eq!(duration(90 * 60_000), "1 h 30 min");
    }
}
