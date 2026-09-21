//! Contact index built from the local message store.
//!
//! Reads each account's message rows, aggregates from/to/cc addresses, filters
//! noise, ranks with a tiered comparator (sent > cc > received) with frecency
//! tiebreaker, and caches results to JSON.
//!
//! Only the two halves that touch the engine are here: `extractor`'s store
//! rebuild and [`hooks`], which observes what a sync or a send just saw.
//! Everything else - the cache, the filter, the ranker, the matcher, the vCard
//! writer and the observation merge - is [`mp_core::contacts`], re-exported
//! whole below (#0126, P5-U10b), so `crate::contacts::load_cache` and every
//! other old path resolves unchanged.

pub use mp_core::contacts::{
    cache, cache_path, contact_to_vcard, filter, load_cache, matcher, observe, rank, save_cache,
    save_rebuilt_cache, search, types, vcard, vcard_file_stem, CacheSave, Contact, ContactIndex,
    ContactSource, ContactTier, MatchResult, ObservedIn,
};

mod extractor;
pub mod hooks;

pub use extractor::build_index_for_account;
