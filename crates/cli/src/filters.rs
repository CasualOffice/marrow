//! Search filters, in the words a command line spells them.
//!
//! `marrow-index` has carried `Filters` — workspace, path glob, extension,
//! mtime bounds — since the port was written, and `marrow-query` resolves a
//! workspace *name* onto it. Only MCP ever passed anything; the CLI could
//! narrow a literal scan and not an indexed search, so the surface the author
//! actually types was the one that could not ask a narrower question.
//!
//! Two things are this module's whole job.
//!
//! **Filters go into the query, never onto its results.** The index applies
//! `limit` before anything else, so a filter applied to what came back would
//! discard most of a page of hits and report "no matches" while matching
//! documents sat one row past the cut. Everything here builds a
//! [`SearchFilters`] that travels *with* the request.
//!
//! **What was excluded is part of the answer.** A result set of three is a
//! different finding depending on whether the other forty were absent or merely
//! filtered out, so every applied filter is echoed — in the human view, in
//! `--json`, and in the zero-results screen.

use marrow_core::{Code, Error, Result, Timestamp};
use marrow_query::search::SearchFilters;

/// Milliseconds in a day. The unit every relative bound below is built from.
const DAY_MS: i64 = 86_400_000;

/// The filter flags exactly as they were typed.
///
/// Borrowed rather than owned because these are `&str`s off `clap` that live as
/// long as the command, and because keeping the user's own words is what lets
/// the reports below say `since=7d` instead of `since=1755561600000`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Args<'a> {
    /// File extension, with or without the leading dot.
    pub extension: Option<&'a str>,
    /// Substring of the whole path, case-blind.
    pub path: Option<&'a str>,
    /// Workspace name. A typo is an error, never zero results.
    pub workspace: Option<&'a str>,
    /// Lower mtime bound: `2026-01-31` or `7d`.
    pub since: Option<&'a str>,
    /// Upper mtime bound, same spellings.
    pub until: Option<&'a str>,
}

/// Resolved filters, plus the words that produced them.
///
/// Both halves are needed and neither substitutes for the other. The resolved
/// [`SearchFilters`] is what narrows the query; the typed words are what the
/// reports show, because `since=7d` is a filter a reader can check against
/// their intent and an epoch millisecond count is not.
#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub search: SearchFilters,
    /// `(flag, what the user typed)`, in flag order.
    described: Vec<(&'static str, String)>,
    /// The `--path` substring before it became a glob. Kept because
    /// [`Filters::admits`] tests a path in Rust rather than in SQLite, and
    /// re-deriving a substring from a bracketed GLOB would be a second
    /// implementation of the first one to disagree with it.
    path_substring: Option<String>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.described.is_empty()
    }

    /// The `--path` substring as typed, for the callers that match a path in
    /// Rust or in a `LIKE` rather than through the index's GLOB.
    pub fn path_substring(&self) -> Option<&str> {
        self.path_substring.as_deref()
    }

    /// One line naming every applied filter. Empty when none were.
    pub fn summary(&self) -> String {
        self.described
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The applied filters as JSON, `{}` when there were none.
    ///
    /// The typed word and the resolved bound are both present for the dates.
    /// A script reconstructing the query needs the word; a script checking a
    /// returned `modified_ms` against the bound needs the number, and deriving
    /// one from the other means re-implementing this module.
    pub fn json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (k, v) in &self.described {
            map.insert((*k).to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(t) = self.search.modified_after {
            map.insert("since_ms".into(), t.as_millis().into());
        }
        if let Some(t) = self.search.modified_before {
            map.insert("until_ms".into(), t.as_millis().into());
        }
        serde_json::Value::Object(map)
    }

    /// Does this file pass the filters that only the lexical branch enforced?
    ///
    /// **This is the after-the-fact filtering the rest of this module exists to
    /// avoid, and it is deliberate.** `VectorQuery` carries a workspace and
    /// nothing else, so the semantic branch retrieves without regard to
    /// extension, path or date; a chunk only that branch found reaches the
    /// renderer having passed no filter at all. Showing a `.pdf` under
    /// `--type html` is a visible falsehood about the result set, and losing a
    /// deep semantic candidate to a late filter is an invisible loss of recall.
    /// Between the two, the falsehood is the one that must not ship.
    ///
    /// A hit the lexical branch returned already satisfied all of this in SQL,
    /// so this only ever removes semantic-only candidates. When `VectorQuery`
    /// learns to take `Filters`, this goes away and nothing else changes.
    pub fn admits(&self, path: &str, modified: Timestamp) -> bool {
        if let Some(ext) = &self.search.extension {
            let want = ext.trim_start_matches('.').to_ascii_lowercase();
            let has = path
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .unwrap_or_default();
            if has != want {
                return false;
            }
        }
        if let Some(sub) = &self.path_substring {
            if !path.to_lowercase().contains(&sub.to_lowercase()) {
                return false;
            }
        }
        if let Some(after) = self.search.modified_after {
            if modified.as_millis() < after.as_millis() {
                return false;
            }
        }
        if let Some(before) = self.search.modified_before {
            if modified.as_millis() > before.as_millis() {
                return false;
            }
        }
        true
    }
}

/// Turn typed flags into a filter set, or explain what was unreadable.
///
/// `now` is a parameter rather than a call to [`Timestamp::now`] so that a
/// relative bound is fixed once for the whole command — a `--since 7d` that
/// re-read the clock would put a slightly different question to each branch —
/// and so the tests below can assert on a bound instead of a range.
pub fn resolve(args: Args<'_>, now: Timestamp) -> Result<Filters> {
    let mut f = Filters::default();

    if let Some(raw) = args.extension {
        let ext = raw.trim().trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                "`--type` needs an extension to filter on. Pass one, with or without the dot: \
                 `--type md` or `--type .md`.",
            ));
        }
        f.described.push(("type", ext.clone()));
        f.search.extension = Some(ext);
    }

    if let Some(raw) = args.path {
        let sub = raw.trim();
        if sub.is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                "`--path` needs part of a path to filter on. Pass a substring of one: \
                 `--path crates/index` or `--path .rs`.",
            ));
        }
        f.described.push(("path", sub.to_string()));
        f.path_substring = Some(sub.to_string());
        f.search.path_glob = Some(substring_glob(sub));
    }

    if let Some(raw) = args.workspace {
        let name = raw.trim();
        if name.is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                "`--workspace` needs a name. Run `marrow workspace list` to see the names \
                 this index knows.",
            ));
        }
        // Resolved by `marrow-query` against the workspaces the store holds, so
        // a name that matches nothing is an error there rather than an empty
        // result here. Zero hits for a typo is the worst answer available: it
        // reads as "nothing is indexed" and sends the reader to debug their
        // corpus instead of their command line.
        f.described.push(("workspace", name.to_string()));
        f.search.workspace = Some(name.to_string());
    }

    if let Some(raw) = args.since {
        f.search.modified_after = Some(parse_bound(raw, Edge::Start, now)?);
        f.described.push(("since", raw.trim().to_string()));
    }
    if let Some(raw) = args.until {
        f.search.modified_before = Some(parse_bound(raw, Edge::End, now)?);
        f.described.push(("until", raw.trim().to_string()));
    }

    // Caught here rather than left to return nothing. An empty window is a
    // typed mistake — the two flags the wrong way round — and it is
    // indistinguishable, from the outside, from a corpus with nothing in it.
    if let (Some(a), Some(b)) = (f.search.modified_after, f.search.modified_before) {
        if a.as_millis() > b.as_millis() {
            return Err(Error::new(
                Code::CfgInvalid,
                "`--since` is later than `--until`, so the window they describe is empty and \
                 no file can fall inside it. Swap them, or widen one.",
            ));
        }
    }

    Ok(f)
}

/// Which end of a day a date literal means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Start,
    End,
}

/// A date bound: `2026-01-31`, or `7d` / `3w` / `6m` / `2y` counted back from
/// now.
///
/// Both bounds are inclusive in the index, so a bare date has to mean the
/// *whole* of that day at the upper end. `--until 2026-01-31` resolving to
/// midnight would silently exclude everything edited on the 31st, which is the
/// day the reader named and the one they most expect to see.
///
/// UTC throughout, because that is what the `modified_ms` column holds. A
/// local-time reading would put the boundary in a different place than the
/// stored value it is compared against, and the difference would show up only
/// for files edited near midnight — the least debuggable failure available.
fn parse_bound(raw: &str, edge: Edge, now: Timestamp) -> Result<Timestamp> {
    let s = raw.trim();
    if let Some(days) = relative_days(s) {
        return Ok(Timestamp::from_millis(
            now.as_millis().saturating_sub(days.saturating_mul(DAY_MS)),
        ));
    }
    if let Some(day) = civil_date(s) {
        let start = day.saturating_mul(DAY_MS);
        return Ok(Timestamp::from_millis(match edge {
            Edge::Start => start,
            Edge::End => start + DAY_MS - 1,
        }));
    }
    Err(Error::new(
        Code::CfgInvalid,
        format!(
            "`{s}` is not a date this understands, so the filter could not be applied. Write a \
             calendar date as `2026-01-31`, or a span counted back from now as `7d`, `3w`, \
             `6m` or `2y`."
        ),
    ))
}

/// `7d`, `3w`, `6m`, `2y` as a number of days back from now.
///
/// Months and years are 30 and 365 days. They are approximations and they are
/// the right ones here: this flag answers "roughly the last half-year", and a
/// calendar-exact month boundary would move the cut by at most a day while
/// costing a date library. Anyone who needs the exact boundary writes the date.
fn relative_days(s: &str) -> Option<i64> {
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = digits.parse().ok()?;
    let per = match unit {
        "d" => 1,
        "w" => 7,
        "m" => 30,
        "y" => 365,
        _ => return None,
    };
    Some(n.saturating_mul(per))
}

/// `YYYY-MM-DD` as days since the epoch, or `None` if it is not one.
fn civil_date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // Rejected rather than normalised. `2026-02-30` is a typo, and rolling it
    // forward to 1 March would answer a question nobody asked without saying so.
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's
/// `days_from_civil`).
///
/// Written out rather than depended on. This is the only calendar arithmetic in
/// the CLI, it is exact, and pulling a date library into a binary that has
/// never needed one to convert two flags would be the larger change.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A case-blind GLOB that matches any path containing `sub`.
///
/// Two things have to happen and neither is optional.
///
/// GLOB is case-sensitive, and the `--literal` scan's `--path` has always been
/// case-blind. One flag with two meanings depending on a sibling flag is a
/// worse surface than either meaning, and `--path desktop` finding nothing on a
/// disk full of `/Desktop/` is the kind of empty result that reads as a fact
/// about the corpus.
///
/// GLOB also has metacharacters, and this flag is documented as a substring.
/// `[x]` is SQLite's literal-character bracket, so `*` becomes `[*]` and the
/// pattern means what the reader typed.
fn substring_glob(sub: &str) -> String {
    let mut out = String::with_capacity(sub.len() * 4 + 2);
    out.push('*');
    for c in sub.chars() {
        match c {
            '*' | '?' | '[' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            c if c.is_ascii_alphabetic() => {
                out.push('[');
                out.push(c.to_ascii_lowercase());
                out.push(c.to_ascii_uppercase());
                out.push(']');
            }
            c => out.push(c),
        }
    }
    out.push('*');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp::from_millis(1_800_000_000_000);

    fn args<'a>() -> Args<'a> {
        Args::default()
    }

    #[test]
    fn no_flags_is_an_empty_filter_set() {
        let f = resolve(args(), NOW).expect("no flags resolve");
        assert!(f.is_empty());
        assert_eq!(f.summary(), "");
        assert_eq!(f.json(), serde_json::json!({}));
    }

    #[test]
    fn an_extension_is_normalised_with_or_without_its_dot() {
        for typed in [".MD", "md", " md "] {
            let f = resolve(
                Args {
                    extension: Some(typed),
                    ..args()
                },
                NOW,
            )
            .expect("an extension resolves");
            assert_eq!(f.search.extension.as_deref(), Some("md"), "{typed}");
        }
    }

    #[test]
    fn a_path_substring_becomes_a_case_blind_glob() {
        // The whole point: `--path desktop` must find `/Users/x/Desktop/a.md`,
        // because the literal scan's `--path` has always matched case-blind and
        // two meanings for one flag is worse than either.
        assert_eq!(substring_glob("ab"), "*[aA][bB]*");
        assert_eq!(substring_glob("a/b"), "*[aA]/[bB]*");
    }

    #[test]
    fn glob_metacharacters_in_a_substring_stay_literal() {
        // Documented as a substring, so `*` is an asterisk and not "anything".
        assert_eq!(substring_glob("*"), "*[*]*");
        assert_eq!(substring_glob("a?"), "*[aA][?]*");
        assert_eq!(substring_glob("["), "*[[]*");
    }

    #[test]
    fn a_calendar_date_lower_bound_is_midnight_utc() {
        let f = resolve(
            Args {
                since: Some("2020-01-01"),
                ..args()
            },
            NOW,
        )
        .expect("a date resolves");
        assert_eq!(
            f.search.modified_after.map(Timestamp::as_millis),
            Some(1_577_836_800_000)
        );
    }

    #[test]
    fn a_calendar_date_upper_bound_covers_the_whole_of_that_day() {
        // `--until 2020-01-01` must include a file saved at 18:00 on the 1st.
        // Midnight would exclude the day the reader named.
        let f = resolve(
            Args {
                until: Some("2020-01-01"),
                ..args()
            },
            NOW,
        )
        .expect("a date resolves");
        assert_eq!(
            f.search.modified_before.map(Timestamp::as_millis),
            Some(1_577_836_800_000 + DAY_MS - 1)
        );
    }

    #[test]
    fn a_relative_span_counts_back_from_the_clock_it_was_handed() {
        let f = resolve(
            Args {
                since: Some("7d"),
                ..args()
            },
            NOW,
        )
        .expect("a span resolves");
        assert_eq!(
            f.search.modified_after.map(Timestamp::as_millis),
            Some(NOW.as_millis() - 7 * DAY_MS)
        );
        for (typed, days) in [("3w", 21), ("6m", 180), ("2y", 730)] {
            let f = resolve(
                Args {
                    since: Some(typed),
                    ..args()
                },
                NOW,
            )
            .expect("a span resolves");
            assert_eq!(
                f.search.modified_after.map(Timestamp::as_millis),
                Some(NOW.as_millis() - days * DAY_MS),
                "{typed}"
            );
        }
    }

    #[test]
    fn an_unreadable_date_names_the_spellings_that_work() {
        let e = resolve(
            Args {
                since: Some("last tuesday"),
                ..args()
            },
            NOW,
        )
        .expect_err("an unparseable bound is an error");
        assert_eq!(e.code(), Code::CfgInvalid);
        let msg = e.to_string();
        assert!(msg.contains("2026-01-31"), "{msg}");
        assert!(msg.contains("7d"), "{msg}");
    }

    #[test]
    fn an_impossible_calendar_date_is_refused_rather_than_rolled_forward() {
        // 2026 is not a leap year, so the 29th does not exist. Normalising it
        // to 1 March would answer a question nobody asked.
        assert_eq!(civil_date("2026-02-29"), None);
        assert!(civil_date("2024-02-29").is_some(), "2024 is a leap year");
        assert_eq!(civil_date("2026-13-01"), None);
        assert_eq!(civil_date("2026-01-32"), None);
    }

    #[test]
    fn a_backwards_window_is_an_error_not_an_empty_result() {
        let e = resolve(
            Args {
                since: Some("2026-06-01"),
                until: Some("2026-01-01"),
                ..args()
            },
            NOW,
        )
        .expect_err("an empty window is refused");
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.to_string().contains("empty"), "{e}");
    }

    #[test]
    fn every_applied_filter_is_reported_in_both_renderings() {
        let f = resolve(
            Args {
                extension: Some("html"),
                path: Some("docs"),
                workspace: Some("system"),
                since: Some("7d"),
                until: None,
            },
            NOW,
        )
        .expect("filters resolve");
        assert_eq!(
            f.summary(),
            "type=html · path=docs · workspace=system · since=7d"
        );
        let j = f.json();
        assert_eq!(j["type"], "html");
        assert_eq!(j["path"], "docs");
        assert_eq!(j["workspace"], "system");
        // The word the user typed and the bound it resolved to. A script
        // checking a returned `modified_ms` needs the number; a script
        // reproducing the command needs the word.
        assert_eq!(j["since"], "7d");
        assert_eq!(j["since_ms"], NOW.as_millis() - 7 * DAY_MS);
    }

    #[test]
    fn admits_rejects_what_only_the_semantic_branch_could_have_returned() {
        let f = resolve(
            Args {
                extension: Some("html"),
                path: Some("docs"),
                since: Some("2020-01-01"),
                ..args()
            },
            NOW,
        )
        .expect("filters resolve");
        let recent = Timestamp::from_millis(1_600_000_000_000);
        assert!(f.admits("/x/docs/a.html", recent));
        assert!(!f.admits("/x/docs/a.pdf", recent), "wrong extension");
        assert!(!f.admits("/x/notes/a.html", recent), "wrong path");
        assert!(
            !f.admits("/x/docs/a.html", Timestamp::from_millis(0)),
            "older than the window"
        );
    }

    #[test]
    fn admits_is_case_blind_about_the_path_the_way_the_glob_is() {
        let f = resolve(
            Args {
                path: Some("desktop"),
                ..args()
            },
            NOW,
        )
        .expect("filters resolve");
        assert!(f.admits("/Users/x/Desktop/a.md", NOW));
    }

    #[test]
    fn an_empty_flag_value_says_what_to_pass_instead() {
        for a in [
            Args {
                extension: Some("  "),
                ..args()
            },
            Args {
                path: Some(""),
                ..args()
            },
            Args {
                workspace: Some(" "),
                ..args()
            },
        ] {
            let e = resolve(a, NOW).expect_err("an empty value is refused");
            assert_eq!(e.code(), Code::CfgInvalid);
        }
    }
}
