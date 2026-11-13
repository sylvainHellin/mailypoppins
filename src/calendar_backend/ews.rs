//! The Exchange Web Services calendar backend for on-premises Exchange
//! (PERSO-256, `.agents/plans/calendar.md`, "Backend notes", EWS).
//!
//! TUM's Exchange 2019 accepts NTLM only, so the login is NTLMv2 written in
//! mp ([`ntlm`]).

pub mod ntlm;

#[cfg(test)]
mod tests;
