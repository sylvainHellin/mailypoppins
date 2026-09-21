//! Contact index: the cache, the filter, the ranker, the matcher and the
//! vCard writer, none of which reads a store (#0126, P5-U10b).
//!
//! Aggregates from/to/cc addresses into a per-account [`ContactIndex`], filters
//! noise, ranks with a tiered comparator (sent > cc > received) with a frecency
//! tiebreaker, and caches the result to JSON. What builds the index out of the
//! account's message rows, and the send/sync hooks that update it in place,
//! stay in the root crate beside the store and re-export this module whole.

pub mod cache;
pub mod extractor;
pub mod filter;
pub mod matcher;
pub mod rank;
pub mod types;
pub mod vcard;

pub use cache::{cache_path, load_cache, save_cache, save_rebuilt_cache, CacheSave};
pub use extractor::{observe, ObservedIn};
pub use matcher::{search, MatchResult};
pub use types::{Contact, ContactIndex, ContactSource, ContactTier};
pub use vcard::{contact_to_vcard, vcard_file_stem};
