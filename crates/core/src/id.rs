//! Typed ULID identifiers.
//!
//! ULID rather than UUID: lexicographically sortable by creation time, so
//! `ORDER BY id` is chronological and index locality is good. Globally unique,
//! which keeps a future multi-device merge possible (SYNC-005) without paying
//! for it now.
//!
//! **Ordering is millisecond-granular, not total.** `Ulid::new()` fills the low
//! 80 bits with randomness, so two IDs minted in the same millisecond sort
//! arbitrarily relative to each other. A monotonic generator exists but needs a
//! shared lock on every mint, which is not worth it: anything that needs strict
//! ordering already has an explicit timestamp column, and `ORDER BY id` is used
//! for locality and rough recency, never as a happens-before relation.
//!
//! Each entity gets its own type. A `FileId` must never be accepted where a
//! `VersionId` is expected — that mistake is silent with bare strings and a
//! compile error here.

use std::fmt;
use std::str::FromStr;

/// Defines a newtype over `Ulid` with the usual conversions.
macro_rules! typed_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(ulid::Ulid);

        impl $name {
            /// Mint a new identifier. See the module note on ordering.
            pub fn new() -> Self {
                Self(ulid::Ulid::new())
            }

            pub fn as_ulid(&self) -> ulid::Ulid {
                self.0
            }

            /// Creation time, milliseconds since the Unix epoch (UTC).
            pub fn timestamp_ms(&self) -> u64 {
                self.0.timestamp_ms()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                ulid::Ulid::from_str(s).map(Self)
            }
        }

        impl From<ulid::Ulid> for $name {
            fn from(u: ulid::Ulid) -> Self {
                Self(u)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

typed_id!(
    /// A workspace: a set of consented roots with its own policy.
    WorkspaceId
);
typed_id!(
    /// One consented directory tree inside a workspace.
    RootId
);
typed_id!(
    /// Stable logical identity of a file. **Survives rename and move** — this is
    /// the whole point of FS-005/FS-006. Never key derived data on a path.
    FileId
);
typed_id!(
    /// One observed state of a file's bytes.
    VersionId
);
typed_id!(
    /// A node in a parsed document's intermediate representation.
    NodeId
);
typed_id!(
    /// A retrieval unit derived from IR nodes.
    ChunkId
);
typed_id!(
    /// A unit of durable background work.
    JobId
);
typed_id!(
    /// One entry in a file's path history (FS-006).
    PathId
);
typed_id!(
    /// One parse attempt against one file version.
    ParseId
);
typed_id!(
    /// One request to the model supervisor (Part 8 §143). Names the scratch
    /// directory, the queue entry and the cancellation token, so a request that
    /// is cancelled mid-flight can be found in all three.
    RequestId
);
typed_id!(
    /// A machine. Carried on canonical rows as `origin_device_id` (SYNC-006)
    /// so a future multi-device merge is possible; unused on one device.
    DeviceId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let id = FileId::new();
        assert_eq!(id, id.to_string().parse::<FileId>().unwrap());
    }

    #[test]
    fn ids_sort_chronologically_to_millisecond_granularity() {
        // Not a total order: IDs minted inside one millisecond sort randomly.
        // What must hold is that a later millisecond always sorts after an
        // earlier one, which is what `ORDER BY id` actually relies on.
        let early = FileId::new();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let late = FileId::new();
        assert!(
            early < late,
            "a later millisecond must sort after an earlier one"
        );
        assert!(early.timestamp_ms() < late.timestamp_ms());
    }

    #[test]
    fn same_millisecond_ids_are_not_ordered() {
        // Documents the limitation so nobody later "fixes" a test that depends
        // on it. If this ever passes deterministically, the generator changed.
        let batch: Vec<FileId> = (0..1000).map(|_| FileId::new()).collect();
        let mut sorted = batch.clone();
        sorted.sort();
        assert_ne!(batch, sorted, "expected within-ms ordering to be random");
    }

    #[test]
    fn ids_are_unique() {
        let ids: std::collections::HashSet<String> =
            (0..10_000).map(|_| FileId::new().to_string()).collect();
        assert_eq!(ids.len(), 10_000);
    }

    #[test]
    fn serde_round_trips_as_a_plain_string() {
        let id = VersionId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""), "must serialize as a bare string");
        assert_eq!(id, serde_json::from_str::<VersionId>(&json).unwrap());
    }
}
