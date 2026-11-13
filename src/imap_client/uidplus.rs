//! UIDPLUS (RFC 4315) on the client's own writes (PERSO-202): the UID an
//! `APPEND` gave the new message, read off the `APPENDUID` response code of
//! the tagged `OK`.
//!
//! `async-imap` 0.11 drops that response code: its `append` checks the tagged
//! answer and throws the code away. So the command is sent here by hand over
//! the session's public `run_command` and `read_response`, the path
//! [`super::search::uid_search_with_literals`] already takes for a literal,
//! and a server without UIDPLUS, which answers a plain `OK`, comes back as
//! `None` for the caller's own fallback.
//!
//! Untagged answers that arrive meanwhile (an `EXISTS` for the new message)
//! are dropped rather than queued for the session's unsolicited channel: no
//! borrower of a pooled session reads that channel for anything but the
//! expunges [`crate::draft_mirror`] drains before it acts.

use std::fmt;

use anyhow::{anyhow, bail, Result};
use async_imap::imap_proto::{Response, ResponseCode, Status, UidSetMember};
use futures::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// The UID the server gave a message this client wrote, under the
/// UIDVALIDITY it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NewUid {
    pub uidvalidity: u32,
    pub uid: u32,
}

/// The server's own `NO` or `BAD` to an `APPEND`, before the literal or
/// after it, with its words: the error [`append`] answers when the server
/// refused, as against one that broke the connection or never answered.
#[derive(Debug)]
pub(crate) struct AppendRefused(pub String);

impl fmt::Display for AppendRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AppendRefused {}

/// `APPEND` `content` to `mailbox` with `flags` (a parenthesised list), and
/// the `APPENDUID` the server answered, `None` from a server without UIDPLUS.
///
/// Success is the tagged `OK`, as it is for `async-imap`'s own `append`: a
/// `NO` or `BAD`, before the literal or after it, is an [`AppendRefused`]
/// carrying the server's words. A server that refuses the command before asking for the
/// literal never receives it.
pub(crate) async fn append<T>(
    session: &mut async_imap::Session<T>,
    mailbox: &str,
    flags: &str,
    content: &[u8],
) -> Result<Option<NewUid>>
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send,
{
    let tag = session
        .run_command(format!(
            "APPEND {} {flags} {{{}}}",
            quoted(mailbox)?,
            content.len()
        ))
        .await?;
    let mut sent = false;
    loop {
        let Some(response) = session.read_response().await? else {
            bail!("the connection closed during the APPEND to '{mailbox}'");
        };
        match response.parsed() {
            Response::Continue { .. } if !sent => {
                let stream = session.get_mut();
                stream.write_all(content).await?;
                stream.write_all(b"\r\n").await?;
                stream.flush().await?;
                sent = true;
            }
            Response::Done {
                tag: done,
                status,
                code,
                information,
            } if *done == tag => {
                if *status != Status::Ok {
                    return Err(anyhow::Error::new(AppendRefused(format!(
                        "the server refused the APPEND to '{mailbox}': {}",
                        information.as_deref().unwrap_or("no reason given")
                    ))));
                }
                if !sent {
                    bail!("the server ended the APPEND to '{mailbox}' before it took the message");
                }
                return Ok(appended_uid(code.as_ref()));
            }
            _ => {}
        }
    }
}

/// The one UID an `APPENDUID` names, `None` for any other code or for a set
/// of several (MULTIAPPEND, which this client never sends).
fn appended_uid(code: Option<&ResponseCode<'_>>) -> Option<NewUid> {
    let Some(ResponseCode::AppendUid(uidvalidity, uids)) = code else {
        return None;
    };
    match uids.as_slice() {
        [UidSetMember::Uid(uid)] => Some(NewUid {
            uidvalidity: *uidvalidity,
            uid: *uid,
        }),
        [UidSetMember::UidRange(range)] if range.start() == range.end() => Some(NewUid {
            uidvalidity: *uidvalidity,
            uid: *range.start(),
        }),
        _ => None,
    }
}

/// `name` as an IMAP quoted string (RFC 3501 section 4.3), which escapes `\`
/// and `"` and cannot carry a line break.
fn quoted(name: &str) -> Result<String> {
    if name.contains(['\r', '\n']) {
        return Err(anyhow!("a mailbox name cannot carry a line break"));
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::super::scripted;
    use super::*;

    /// The literal goes out only after the server's continuation, and the
    /// tagged `OK [APPENDUID 1 309]` is the new message's UID.
    #[tokio::test]
    async fn an_append_answers_the_uid_its_appenduid_names() {
        let (mut session, turns) = scripted::session(&[
            "A0001 OK logged in\r\n",
            "+ ready for literal\r\n",
            "* 18 EXISTS\r\nA0002 OK [APPENDUID 1 309] Append completed.\r\n",
        ])
        .await;

        let new = append(
            &mut session,
            "Sent \"Items\"",
            "(\\Seen)",
            b"Subject: hi\r\n\r\nbody",
        )
        .await
        .unwrap();

        assert_eq!(
            new,
            Some(NewUid {
                uidvalidity: 1,
                uid: 309
            })
        );
        assert_eq!(
            scripted::after_login(&turns),
            vec![
                "A0002 APPEND \"Sent \\\"Items\\\"\" (\\Seen) {19}\r\n".to_string(),
                "Subject: hi\r\n\r\nbody\r\n".to_string(),
            ]
        );
    }

    /// A server without UIDPLUS acknowledges with a plain `OK`: the append
    /// succeeded and names no UID, which the caller finds its own way.
    #[tokio::test]
    async fn an_append_without_appenduid_succeeds_with_no_uid() {
        let (mut session, _) = scripted::session(&[
            "A0001 OK logged in\r\n",
            "+ go\r\n",
            "A0002 OK APPEND completed\r\n",
        ])
        .await;

        let new = append(&mut session, "Sent", "(\\Seen)", b"x")
            .await
            .unwrap();

        assert_eq!(new, None);
    }

    /// A refusal before the continuation is an error, and the message is
    /// never sent: there is no literal for the server to misread as a command.
    #[tokio::test]
    async fn a_refused_append_never_sends_the_message() {
        let (mut session, turns) = scripted::session(&[
            "A0001 OK logged in\r\n",
            "A0002 NO [TRYCREATE] Mailbox doesn't exist: Sent\r\n",
        ])
        .await;

        let error = append(&mut session, "Sent", "(\\Seen)", b"secret body")
            .await
            .unwrap_err();
        assert!(
            error.is::<AppendRefused>(),
            "the server's own answer: {error:#}"
        );
        let error = error.to_string();

        assert!(error.contains("Mailbox doesn't exist"), "{error}");
        assert_eq!(
            scripted::after_login(&turns),
            vec!["A0002 APPEND \"Sent\" (\\Seen) {11}\r\n".to_string()]
        );
    }

    /// Only a single UID is an answer; a mailbox name that would break the
    /// command line is refused before anything is sent.
    #[test]
    fn only_one_uid_is_an_appended_uid_and_a_line_break_is_no_mailbox() {
        let one = ResponseCode::AppendUid(4, vec![UidSetMember::UidRange(9..=9)]);
        assert_eq!(
            appended_uid(Some(&one)),
            Some(NewUid {
                uidvalidity: 4,
                uid: 9
            })
        );
        let several = ResponseCode::AppendUid(4, vec![UidSetMember::UidRange(9..=10)]);
        assert_eq!(appended_uid(Some(&several)), None);
        assert_eq!(appended_uid(Some(&ResponseCode::ReadOnly)), None);
        assert_eq!(appended_uid(None), None);
        assert!(quoted("Sent\r\nA1 LOGOUT").is_err());
    }
}
