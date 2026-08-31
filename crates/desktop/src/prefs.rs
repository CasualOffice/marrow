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
use marrow_model::openai::Endpoint;

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
    /// Which installed local model answers questions.
    ///
    /// `None` means "whatever fits", which is what this always did and never
    /// said: `local_generator` picked the largest installed model that fitted
    /// the memory free at that instant. That is a reasonable default and a
    /// terrible only-option — the choice moved with the machine's load, nothing
    /// reported which model had been chosen until the answer's footer, and
    /// there was no way to pin one.
    ///
    /// An id that is not installed is ignored rather than an error: models are
    /// deleted from the Models page, and a stale preference must not be able to
    /// stop questions being answered.
    pub generator_model_id: Option<String>,
    /// The one remote endpoint, when the user has configured one.
    ///
    /// **The key is not here and cannot be** (LLM-030): [`RemoteProvider`]
    /// holds an [`Endpoint`], which names a keyring account rather than a
    /// secret. This file is a plain file the user is invited to read and edit,
    /// and it ends up in backups.
    pub remote_provider: Option<RemoteProvider>,
}

/// A configured OpenAI-compatible endpoint, as the Settings page holds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProvider {
    /// Whether it answers questions. Configured and off is a real state: the
    /// user sets it up, then decides per week whether to use it, and clearing
    /// the endpoint to turn it off would mean re-typing the key.
    pub enabled: bool,
    /// What to call it on screen. The user's word, because "the endpoint at
    /// api.openai.com" is not what they think of it as.
    pub label: String,
    pub endpoint: Endpoint,
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

/// Pin a local model, or return to choosing automatically.
pub fn set_generator_model(data_dir: &Path, model_id: Option<String>) -> std::io::Result<()> {
    let mut prefs = load(data_dir);
    prefs.generator_model_id = model_id;
    write(data_dir, &prefs)
}

/// Record — or clear — the remote provider.
///
/// Same read-modify-write as the profile, and the same rule about what is not
/// in it: the key never passes through here. It goes to the OS keyring from
/// the command that received it, and this file learns only which account holds
/// it.
pub fn set_remote_provider(
    data_dir: &Path,
    provider: Option<RemoteProvider>,
) -> std::io::Result<()> {
    let mut prefs = load(data_dir);
    prefs.remote_provider = provider;
    write(data_dir, &prefs)
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
    fn a_remote_provider_survives_a_relaunch_and_its_key_never_lands_in_the_file() {
        // LLM-030, as a property of the bytes on disk rather than of anyone's
        // intentions. This file is plain text the user is invited to edit, and
        // it is copied into every backup of the data directory.
        let dir = tempfile::tempdir().expect("temp dir");
        let provider = RemoteProvider {
            enabled: true,
            label: "OpenAI".into(),
            endpoint: Endpoint::new("https://api.openai.com/v1", "gpt-4o-mini"),
        };
        set_remote_provider(dir.path(), Some(provider.clone())).expect("write");

        let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).expect("read");
        assert!(
            text.contains("api.openai.com"),
            "the endpoint is not a secret"
        );
        assert!(
            text.contains("keyAccount"),
            "the file names where the key is, not what it is"
        );
        for shape in ["sk-", "apiKey", "\"key\"", "secret", "token"] {
            assert!(!text.contains(shape), "{shape} appeared in {text}");
        }
        assert_eq!(load(dir.path()).remote_provider, Some(provider));

        set_remote_provider(dir.path(), None).expect("clear");
        assert_eq!(load(dir.path()).remote_provider, None);
    }

    #[test]
    fn configuring_a_provider_does_not_disturb_the_ai_preference() {
        // Read-modify-write, tested rather than assumed: the two settings are
        // written by different pages and one must not erase the other.
        let dir = tempfile::tempdir().expect("temp dir");
        set_ai_profile(dir.path(), Profile::Efficient);
        set_remote_provider(
            dir.path(),
            Some(RemoteProvider {
                enabled: false,
                label: "LM Studio".into(),
                endpoint: Endpoint::new("http://localhost:1234/v1", "qwen"),
            }),
        )
        .expect("write");
        let p = load(dir.path());
        assert_eq!(p.ai_profile, Some(Profile::Efficient));
        assert!(p.remote_provider.is_some());
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
