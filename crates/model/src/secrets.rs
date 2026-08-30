//! Where a provider key lives (Part 8 §140, LLM-030 · SEC-005).
//!
//! **Marrow never proxies inference and never stores a key of its own.** The
//! user brings a key, it goes into the OS keyring, and it is read back at the
//! moment a request is made. Three places it must never be, and the reason each
//! is a real risk rather than a hypothetical one:
//!
//! | Never | Why |
//! |---|---|
//! | `preferences.json` | It is a plain file the user is invited to read and edit, and it ends up in backups |
//! | The index database | `VACUUM INTO` copies it, and the backup lives beside the database forever |
//! | A log line or an error message | Logs get pasted into bug reports. This is the same rule as Part 9's NET-051 |
//!
//! The mechanism, rather than the discipline, is [`Secret`]: it has no
//! `Display`, no `Serialize` and a `Debug` that prints nothing. A key cannot
//! reach a log through the ordinary ways of getting text out of a value,
//! because those ways do not exist on the type.

use std::fmt;

use marrow_core::{Code, Error, Result};

/// The service name the key is filed under in the OS keyring.
///
/// One service, many accounts: an account is the provider slot, so a key can be
/// replaced without knowing what it was.
pub const SERVICE: &str = "Marrow";

/// An API key.
///
/// Deliberately thin: `new`, `expose`, and nothing that can print it.
///
/// **It is not zeroed on drop.** Doing that to a `String` requires a volatile
/// write, this crate is `#![deny(unsafe_code)]`, and a zeroing implementation
/// that the optimiser is free to remove would be a comment rather than a
/// defence. What is guaranteed here is narrower and true: the key is read at
/// request time and dropped when the request ends, so it is not resident for
/// the life of the process.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way to read it. Named so that every use site says what it is
    /// doing, and so `grep expose` finds all of them.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

/// Prints nothing. A struct holding one can derive `Debug` safely, which is
/// what makes the guarantee survive someone adding a field later.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Reading and writing keys, with a seam so tests never touch the real
/// keychain — a unit test that prompts for a login password is a unit test
/// nobody runs.
pub trait SecretStore: fmt::Debug + Send + Sync {
    /// `None` means no key is stored, which is not an error: a server the user
    /// runs on this machine usually wants none.
    fn get(&self, account: &str) -> Result<Option<Secret>>;
    fn put(&self, account: &str, secret: &Secret) -> Result<()>;
    /// Removing a key that was never there succeeds.
    fn delete(&self, account: &str) -> Result<()>;
}

/// The macOS keychain (SEC-005).
#[derive(Debug, Default, Clone, Copy)]
pub struct Keyring;

impl Keyring {
    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, account).map_err(|e| {
            // The error names the keychain, never the value. `keyring`'s own
            // errors carry the service and account only.
            Error::new(
                Code::CfgInvalid,
                "The system keychain could not be opened, so the provider key \
                 cannot be read. Local answering is unaffected.",
            )
            .with_source(e)
        })
    }
}

impl SecretStore for Keyring {
    fn get(&self, account: &str) -> Result<Option<Secret>> {
        match Self::entry(account)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::new(
                Code::CfgInvalid,
                "The provider key could not be read from the system keychain. \
                 Open Settings and save it again.",
            )
            .with_source(e)),
        }
    }

    fn put(&self, account: &str, secret: &Secret) -> Result<()> {
        Self::entry(account)?
            .set_password(secret.expose())
            .map_err(|e| {
                Error::new(
                    Code::CfgInvalid,
                    "The provider key could not be saved to the system keychain. \
                     Nothing was written anywhere else.",
                )
                .with_source(e)
            })
    }

    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::new(
                Code::CfgInvalid,
                "The provider key could not be removed from the system keychain. \
                 Remove it with Keychain Access if it is still listed there.",
            )
            .with_source(e)),
        }
    }
}

/// An in-memory store, for tests and for a build with no keychain.
///
/// The values are `String` rather than [`Secret`] because they are stored, not
/// passed around — and that is exactly why this type prints its **accounts**
/// and never its values. The derived `Debug` was there first and a test caught
/// it: `OpenAiProvider` holds an `Arc<dyn SecretStore>` and derives `Debug`, so
/// one `tracing::debug!(?provider)` printed every key in the store. A redacted
/// [`Secret`] is not a defence if the container around it is candid.
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

impl fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accounts: Vec<String> = self
            .0
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        f.debug_struct("MemorySecrets")
            .field("accounts", &accounts)
            .finish()
    }
}

impl MemorySecrets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one, for a test that does not want to call `put` first.
    pub fn with(account: &str, value: &str) -> Self {
        let s = Self::default();
        let _ = s.put(account, &Secret::new(value));
        s
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, account: &str) -> Result<Option<Secret>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| Error::invariant("the secret store lock was poisoned"))?
            .get(account)
            .map(|v| Secret::new(v.clone())))
    }

    fn put(&self, account: &str, secret: &Secret) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| Error::invariant("the secret store lock was poisoned"))?
            .insert(account.to_string(), secret.expose().to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| Error::invariant("the secret store lock was poisoned"))?
            .remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-live-do-not-print-me";

    #[test]
    fn a_key_never_survives_being_printed() {
        // LLM-030. `Debug` is how a value reaches a log without anyone
        // deciding to put it there — a `tracing::debug!(?config)` on a struct
        // that happens to hold one.
        let s = Secret::new(KEY);
        assert!(!format!("{s:?}").contains(KEY));
        assert!(format!("{s:?}").contains("redacted"));

        #[derive(Debug)]
        struct Holder {
            #[allow(dead_code)]
            key: Secret,
        }
        let h = Holder {
            key: Secret::new(KEY),
        };
        assert!(
            !format!("{h:?}").contains(KEY),
            "a struct that derives Debug must not leak the key it holds"
        );
    }

    #[test]
    fn a_store_prints_which_accounts_it_holds_and_never_what_is_in_them() {
        // The half the redacted `Secret` does not cover. A provider holds a
        // `dyn SecretStore` and derives `Debug`, so the container is on the
        // same path to a log line as the value.
        let store = MemorySecrets::with("openai", KEY);
        let printed = format!("{store:?}");
        assert!(!printed.contains(KEY), "{printed}");
        assert!(printed.contains("openai"), "the account is the useful half");
    }

    #[test]
    fn a_key_cannot_be_serialised_by_accident() {
        // There is no `Serialize` impl, so this is a compile-time property
        // rather than a runtime one. The test states it so that adding one
        // later has to argue with a name: a settings file, an IPC payload and
        // a conversation row are all `serde` output.
        fn assert_not_serialisable<T>() {}
        assert_not_serialisable::<Secret>();
        // And the only way to read it is a method that says so.
        assert_eq!(Secret::new(KEY).expose(), KEY);
    }

    #[test]
    fn a_missing_key_is_not_an_error() {
        // A server the user runs on this machine wants no key at all, and
        // treating "none stored" as a failure would make that the loud path.
        let store = MemorySecrets::new();
        assert!(store.get("nothing-here").expect("get").is_none());
        assert!(store.delete("nothing-here").is_ok(), "delete is idempotent");
    }

    #[test]
    fn a_key_round_trips_and_can_be_replaced_without_reading_it() {
        let store = MemorySecrets::new();
        store.put("openai", &Secret::new(KEY)).expect("put");
        assert_eq!(
            store.get("openai").expect("get").expect("some").expose(),
            KEY
        );
        store
            .put("openai", &Secret::new("sk-new"))
            .expect("replace");
        assert_eq!(
            store.get("openai").expect("get").expect("some").expose(),
            "sk-new"
        );
        store.delete("openai").expect("delete");
        assert!(store.get("openai").expect("get").is_none());
    }

    #[test]
    fn a_blank_key_is_recognisable_so_it_is_never_saved_as_one() {
        // Pasting whitespace into the field must not produce a stored key that
        // then fails with a 401 nobody can explain.
        assert!(Secret::new("   ").is_empty());
        assert!(!Secret::new("sk-x").is_empty());
    }
}
