//! The server drafts mirror (#0143): every draft of an account converges with
//! a copy in the account's server Drafts folder, which is where Outlook, Apple
//! Mail, the Proton app and mp on the other machine see it.
//!
//! The draft file stays the truth. A push renders the file into a message and
//! replaces the server copy with it; a pull rewrites the file from a copy that
//! changed on the server. Which copy belongs to which file is the draft's
//! `server_message_id` frontmatter key, minted at the first push and carried by
//! every copy mp writes; Outlook and the Proton app keep it across their own
//! edits (the probe of 2026-10-20). The volatile state, the copy's UID and the
//! hash of the file at the last convergence, lives in the store's
//! [`draft_sync`](crate::store::draft_sync) table, which a store rebuild drops
//! and the reconcile re-derives.

/// The hash a [`draft_sync`](crate::store::draft_sync) row records of a draft
/// file's bytes: SHA-256, in lowercase hex.
///
/// A file whose hash differs from its row's changed locally since the two
/// last converged. The bytes are hashed whole, frontmatter included, so an
/// approval or a recipient edit is a change as much as a body edit is.
pub fn file_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is the SHA-256 of the bytes, and any change to them moves it.
    #[test]
    fn the_file_hash_is_the_sha256_of_the_bytes() {
        assert_eq!(
            file_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(file_hash(b"status: draft"), file_hash(b"status: approved"));
    }
}
