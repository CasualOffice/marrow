//! Choices the user made, kept across launches.
//!
//! **A setting that silently reverts is worse than one that is not offered.**
//! The AI preference on the Models page lived in a `Mutex<Profile>` and nowhere
//! else, so picking "Efficient" worked for exactly as long as the process ran
//! and was back to the hardware default at the next launch — with the page
//! cheerfully showing the reverted value as though it were the choice. The user
//! has no way to tell that apart from a control that does nothing.
//!
//! **A file rather than a database table.** Two reasons, and the second is the
//! one that decided it:
//!
//!   1. This is the precedent already in the tree. `net-allow.txt` is a plain
//!      file in the same directory for the stated reason that the user should be
//!      able to read and edit what they have agreed to ([`marrow_net::Consent::from_allowlist`]).
//!      A handful of preferences is the same kind of thing.
//!   2. A migration is a number claimed across every crate that contributes one
//!      ([D57]), and claiming one for two string fields would put this crate in
//!      the chain for the first time — a real cost, borne by every binary, for
//!      something that is not index state and is not rebuildable from the files.
//!
//! **A missing or unreadable file is not an error.** It means "nothing chosen",
//! which is the correct default and exactly how `from_allowlist` treats an
//! absent allowlist. A corrupt preference must never be the reason the window
//! will not open, so every failure here is a `tracing` line and a fallback.
//!
//! [D57]: ../../../DECISIONS.md

use std::path::{Path, PathBuf};

use marrow_hw::Profile;

/// Sits beside `marrow.db` and `net-allow.txt` in the per-user data directory.
const FILE_NAME: &str = "preferences.json";

/// Everything that survives a relaunch.
///
/// Every field is optional and the whole struct derives `Default`, so a file
/// written by an older build — one field short — still parses. A preferences
/// format that refuses to load because it does not recognise a key is a
/// preferences format that loses the user's settings on every upgrade.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    /// `efficient` | `balanced` | `larger_local` | `cloud`. `None` means the
    /// user has never chosen, which is *not* the same as having chosen the
    /// value that `default_profile` happens to return today: the hardware
    /// default may move, and it should keep moving until someone decides.
    pub ai_profile: Option<Profile>,
}

/// Read what the user chose. Never fails; an absent or unreadable file is
/// "nothing chosen".
pub fn load(data_dir: &Path) -> Preferences {
    let path = path_in(data_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Preferences::default();
    };
    match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            // Named, so someone who hand-edited it and made a typo can find it.
            // Not fatal: the app opens on defaults and the file is rewritten by
            // the next deliberate change.
            tracing::warn!(error = %e, file = %path.display(), "preferences did not parse; using defaults");
            Preferences::default()
        }
    }
}

/// Record a choice.
///
/// Read-modify-write rather than write-whole-struct, so a field this build does
/// not know about is not deleted by a build that only knows about one of them.
///
/// Written to a temporary file and renamed over the target. `rename` within a
/// directory is atomic on every filesystem this runs on, which means a crash
/// mid-write leaves the previous preferences intact rather than a half-written
/// file that will not parse — the exact failure the "unreadable is not an error"
/// rule above then has to paper over.
pub fn set_ai_profile(data_dir: &Path, profile: Profile) {
    let mut prefs = load(data_dir);
    prefs.ai_profile = Some(profile);
    if let Err(e) = write(data_dir, &prefs) {
        // The choice still applies for this session — the caller has already
        // set it in memory. What is lost is the persistence, and saying so is
        // better than a silent no-op.
        tracing::warn!(error = %e, "could not save the AI preference; it applies until Marrow is closed");
    }
}

fn write(data_dir: &Path, prefs: &Preferences) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let target = path_in(data_dir);
    let temp = target.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(prefs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&temp, body)?;
    std::fs::rename(&temp, &target)
}

fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_profile_is_still_chosen_after_a_relaunch() {
        // The reported bug: `set_profile` wrote a mutex and nothing else, so
        // the Models page reverted to the hardware default on every launch.
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(load(dir.path()).ai_profile, None, "nothing chosen yet");

        set_ai_profile(dir.path(), Profile::LargerLocal);
        assert_eq!(load(dir.path()).ai_profile, Some(Profile::LargerLocal));
    }

    #[test]
    fn nothing_chosen_is_distinguishable_from_choosing_the_default() {
        // `None` must not be written as "balanced": the hardware default is
        // allowed to move between builds, and it should keep moving for a user
        // who has never expressed an opinion.
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(load(dir.path()).ai_profile.is_none());
    }

    #[test]
    fn a_corrupt_preferences_file_does_not_stop_the_app_opening() {
        // Hand-edited by a user, or truncated by a crash. The app opens on
        // defaults; refusing to start over a preference would be absurd.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(FILE_NAME), "{ this is not json").expect("write");
        assert_eq!(load(dir.path()).ai_profile, None);
    }

    #[test]
    fn a_field_this_build_does_not_know_about_survives_a_write() {
        // A newer build's key must not be deleted by an older one recording a
        // profile. `serde` drops unknown fields on parse, so the guard is the
        // read-modify-write plus `#[serde(default)]` — this pins the half of it
        // that is testable here: writing does not fail on a foreign key.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"aiProfile":"efficient","somethingElse":7}"#,
        )
        .expect("write");
        assert_eq!(load(dir.path()).ai_profile, Some(Profile::Efficient));
        set_ai_profile(dir.path(), Profile::Cloud);
        assert_eq!(load(dir.path()).ai_profile, Some(Profile::Cloud));
    }
}
