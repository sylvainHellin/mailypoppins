use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tier describes which "kind" of observation boosted this contact.
/// Lexicographic comparison on (tier1_count, tier2_count, tier3_count)
/// gives the notmuch-addrlookup-c tiered ranking for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactTier {
    /// Directly sent-to (most trusted) — present in a sent/ message's `to:`.
    SentTo,
    /// CC'd on sent mail.
    SentCc,
    /// From/To/Cc of received mail.
    Received,
}

/// Where this contact came from. For Tier A this is always "local" (scanned
/// from local mailstore). Reserved for future vCard/CardDAV tiers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactSource {
    #[default]
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Lowercased email address (the index key).
    pub address: String,
    /// Most-recently-observed non-empty display name. Empty string if never seen.
    pub display_name: String,
    /// Count of sent/ messages where this address was in `to:`.
    pub sent_to: u32,
    /// Count of sent/ messages where this address was in `cc:` or `bcc:`.
    pub sent_cc: u32,
    /// Count of inbox/archive messages that mentioned this address
    /// (from/to/cc — any field).
    pub received: u32,
    /// RFC-3339 timestamp of the first message this address appeared in.
    pub first_seen: String,
    /// RFC-3339 timestamp of the most recent message this address appeared in.
    /// Also the timestamp the display_name was picked from.
    pub last_seen: String,
    #[serde(default)]
    pub source: ContactSource,
}

impl Contact {
    /// Lexicographic tier tuple used by the ranker.
    pub fn tier_tuple(&self) -> (u32, u32, u32) {
        (self.sent_to, self.sent_cc, self.received)
    }
}

/// The full contact index for a single account.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ContactIndex {
    /// Account name this index belongs to (matches `AccountConfig::name`).
    pub account: String,
    /// Address -> Contact. Address is lowercased.
    pub contacts: HashMap<String, Contact>,
    /// RFC-3339 timestamp of the most recent full rebuild.
    pub built_at: String,
}
