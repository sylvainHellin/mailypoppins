//! Shapes of the `contact.*` family that a client decodes rather than renders:
//! the vCard draft (`contact.vcard_draft`) and the implicit build a
//! `contact.search` reports when the cache guard refused it (the daemon
//! leftovers, 2026-10-22).

use serde::{Deserialize, Serialize};

use crate::draft::DraftCreated;

/// The result of `contact.vcard_draft`: the new draft, addressed to the
/// contact, with the contact's vCard written beside it and attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct VcardDraft {
    /// The draft, as `draft.create` answers it.
    pub draft: DraftCreated,
    /// The absolute path of the `.vcf` under the drafts directory's
    /// `_vcards/`, which the draft's `attachments:` names.
    pub vcf: String,
}

/// What the cache guard (#0067) kept when it refused the index a
/// `contact.search` built because the account had none cached: the search
/// answered from the kept index, and a client says so.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RefusedRebuild {
    /// `refused_empty` or `refused_shrunk`, as `contact.rebuild` settles.
    pub saved: String,
    /// How many contacts the refused build found.
    pub contacts: u64,
    /// How many the kept index holds.
    pub kept: u64,
}
