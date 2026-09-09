//! The daemon's numeric error table.
//!
//! JSON-RPC reserves `-32768..=-32000` for protocol-level errors and defines
//! five of them, which the daemon also emits: `-32700` parse error, `-32600`
//! invalid request, `-32601` method not found, `-32602` invalid params, and
//! `-32603` internal error. The daemon's own conditions occupy
//! `-32009..=-32000`, which collides with none of them.
//!
//! The numbers and the wire names are pinned by
//! `crates/mp-protocol/fixtures/*.json` and by `tests/daemon_framing.rs`.
//! Changing either needs a protocol-changelog entry in
//! `docs/daemon-protocol.md`.

/// A daemon error condition, with its wire code and its stable wire name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The connection has not completed `initialize`.
    NotInitialized,
    /// The client's data-dir / config-dir pair differs from the daemon's.
    IdentityMismatch,
    /// The declared protocol ranges do not overlap.
    ProtocolIncompatible,
    /// A capability the client declared as required is not offered.
    CapabilityMissing,
    /// A frame breached the byte cap for its direction.
    FrameTooLarge,
    /// The named account is not configured.
    AccountUnknown,
    /// The account exists but its runtime cannot serve the call yet.
    AccountNotReady,
    /// The configuration on disk failed to load or validate.
    ConfigInvalid,
    /// A long-running operation was cancelled before it completed.
    OperationCancelled,
    /// The daemon is shutting down and accepts no new work.
    ShuttingDown,
}

impl ErrorCode {
    /// Every variant, in table order, for exhaustive mapping and for tests.
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::NotInitialized,
        ErrorCode::IdentityMismatch,
        ErrorCode::ProtocolIncompatible,
        ErrorCode::CapabilityMissing,
        ErrorCode::FrameTooLarge,
        ErrorCode::AccountUnknown,
        ErrorCode::AccountNotReady,
        ErrorCode::ConfigInvalid,
        ErrorCode::OperationCancelled,
        ErrorCode::ShuttingDown,
    ];

    /// The JSON-RPC `error.code` this condition serialises as.
    pub fn code(self) -> i32 {
        match self {
            ErrorCode::NotInitialized => -32000,
            ErrorCode::IdentityMismatch => -32001,
            ErrorCode::ProtocolIncompatible => -32002,
            ErrorCode::CapabilityMissing => -32003,
            ErrorCode::FrameTooLarge => -32004,
            ErrorCode::AccountUnknown => -32005,
            ErrorCode::AccountNotReady => -32006,
            ErrorCode::ConfigInvalid => -32007,
            ErrorCode::OperationCancelled => -32008,
            ErrorCode::ShuttingDown => -32009,
        }
    }

    /// The stable snake-case name clients match on and logs print.
    pub fn name(self) -> &'static str {
        match self {
            ErrorCode::NotInitialized => "not_initialized",
            ErrorCode::IdentityMismatch => "identity_mismatch",
            ErrorCode::ProtocolIncompatible => "protocol_incompatible",
            ErrorCode::CapabilityMissing => "capability_missing",
            ErrorCode::FrameTooLarge => "frame_too_large",
            ErrorCode::AccountUnknown => "account_unknown",
            ErrorCode::AccountNotReady => "account_not_ready",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::OperationCancelled => "operation_cancelled",
            ErrorCode::ShuttingDown => "shutting_down",
        }
    }

    /// The variant a wire code names, or `None` for a code outside the table.
    pub fn from_code(code: i32) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.code() == code)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name(), self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn from_code_inverts_code() {
        for variant in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_code(variant.code()), Some(variant));
        }
        assert_eq!(ErrorCode::from_code(-32603), None);
    }

    #[test]
    fn display_names_the_code() {
        assert_eq!(
            ErrorCode::FrameTooLarge.to_string(),
            "frame_too_large (-32004)"
        );
    }
}
