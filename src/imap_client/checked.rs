//! The commands whose tagged answer `async-imap` 0.11 never reads.
//!
//! Its readers for `FETCH`, `STORE`, `EXPUNGE`, `SEARCH`, `LIST`,
//! `CAPABILITY` and `NOOP` (`parse_fetches`, `parse_expunge`, `parse_ids`,
//! `parse_names`, `parse_capabilities`, `parse_noop`) take responses until the
//! command's tagged line and drop that line unread, so a server's `NO` or
//! `BAD` comes back as an empty success: a `\Deleted` STORE the server refused
//! retired the delete, a refused dedup `SEARCH` read as "no copy", a refused
//! `LIST` as a server with no folders. A connection that closes before the
//! tagged line is an empty success there too.
//!
//! [`Checked`] sends each of them over the session's public `run_command` and
//! reads the answer with `read_response`, the path
//! [`super::uidplus::append`] and [`super::search::uid_search_with_literals`]
//! already take, up to the tagged line, whose status it reads (RFC 3501 7.1):
//! `OK` is the command's answer, `NO` and `BAD` are
//! [`async_imap::error::Error::No`] and [`async_imap::error::Error::Bad`] in
//! the words `async-imap` gives every other refusal (`SELECT`, `COPY`), and a
//! connection that ends first is [`async_imap::error::Error::ConnectionLost`].
//!
//! The answer is gathered whole, untagged responses of other kinds dropped
//! except the `EXPUNGE`s a fetch saw ([`Checked::uid_fetch_checked`]), which
//! the draft mirror's claim reads in place of the session's unsolicited
//! channel. The session methods these replace are listed in `clippy.toml`'s
//! `disallowed-methods`, so a new call goes through here.

use std::collections::HashSet;
use std::fmt;

use async_imap::error::{Error, Result};
use async_imap::imap_proto::{
    AttributeValue, Capability, MailboxDatum, MessageSection, Response, SectionPath, Status,
};
use async_imap::types::{Flag, NameAttribute};
use futures::io::{AsyncRead, AsyncWrite};

/// One message of a `FETCH` or `STORE` answer, owned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FetchResponse {
    /// Its sequence number.
    pub message: u32,
    pub uid: Option<u32>,
    pub modseq: Option<u64>,
    /// The flags, as the server spells them.
    pub flags: Vec<String>,
    /// `BODY[HEADER]` or `BODY[HEADER.FIELDS (...)]`, when asked.
    pub header: Option<Vec<u8>>,
    /// `BODY[]`, when asked.
    pub body: Option<Vec<u8>>,
}

impl FetchResponse {
    /// The flags as [`Flag`]s, as `async_imap::types::Fetch::flags` gives them.
    pub fn flags(&self) -> impl Iterator<Item = Flag<'_>> {
        self.flags.iter().map(|flag| Flag::from(flag.as_str()))
    }

    pub fn header(&self) -> Option<&[u8]> {
        self.header.as_deref()
    }

    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    fn of(message: u32, attributes: &[AttributeValue<'_>]) -> FetchResponse {
        let mut out = FetchResponse {
            message,
            ..FetchResponse::default()
        };
        for attribute in attributes {
            match attribute {
                AttributeValue::Uid(uid) => out.uid = Some(*uid),
                AttributeValue::ModSeq(modseq) => out.modseq = Some(*modseq),
                AttributeValue::Flags(flags) => {
                    out.flags = flags.iter().map(|flag| flag.to_string()).collect();
                }
                AttributeValue::BodySection {
                    section: Some(SectionPath::Full(MessageSection::Header)),
                    data: Some(header),
                    ..
                }
                | AttributeValue::Rfc822Header(Some(header)) => {
                    out.header = Some(header.to_vec());
                }
                AttributeValue::BodySection {
                    section: None,
                    data: Some(body),
                    ..
                }
                | AttributeValue::Rfc822(Some(body)) => out.body = Some(body.to_vec()),
                _ => {}
            }
        }
        out
    }
}

/// One mailbox of a `LIST` answer, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedMailbox {
    pub name: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<NameAttribute<'static>>,
}

/// An untagged response of a checked command, owned, as far as a caller
/// reads one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Untagged {
    Fetch(FetchResponse),
    Expunge(u32),
    Search(Vec<u32>),
    List(ListedMailbox),
    Capabilities(Vec<String>),
}

impl Untagged {
    fn of(response: &Response<'_>) -> Option<Untagged> {
        Some(match response {
            Response::Fetch(message, attributes) => {
                Untagged::Fetch(FetchResponse::of(*message, attributes))
            }
            Response::Expunge(message) => Untagged::Expunge(*message),
            Response::MailboxData(MailboxDatum::Search(found)) => Untagged::Search(found.clone()),
            Response::MailboxData(MailboxDatum::List {
                name_attributes,
                delimiter,
                name,
            }) => Untagged::List(ListedMailbox {
                name: name.to_string(),
                delimiter: delimiter.as_ref().map(|d| d.to_string()),
                attributes: name_attributes
                    .iter()
                    .map(|attribute| attribute.clone().into_owned())
                    .collect(),
            }),
            Response::Capabilities(capabilities) => Untagged::Capabilities(
                capabilities
                    .iter()
                    .filter_map(|capability| match capability {
                        Capability::Atom(name) => Some(name.to_string()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => return None,
        })
    }
}

/// Send `command` and gather its untagged answer up to its tagged line,
/// whose `NO` or `BAD` is the error.
pub(crate) async fn answer_of<T>(
    session: &mut async_imap::Session<T>,
    command: &str,
) -> Result<Vec<Untagged>>
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send,
{
    match partial_answer_of(session, command).await? {
        (answer, None) => Ok(answer),
        (_, Some(refusal)) => Err(refusal),
    }
}

/// [`answer_of`], keeping the untagged answer a `NO` came after: a server
/// that cannot send some of the messages a `FETCH` named sends the others
/// and then says `NO` (RFC 2180 4.1.2; Gmail's `NO Some messages could not
/// be FETCHed`). A `BAD` and a connection that ends are errors as before.
pub(crate) async fn partial_answer_of<T>(
    session: &mut async_imap::Session<T>,
    command: &str,
) -> Result<(Vec<Untagged>, Option<Error>)>
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send,
{
    let tag = session.run_command(command).await?;
    let mut answer = Vec::new();
    loop {
        let Some(response) = session.read_response().await? else {
            return Err(Error::ConnectionLost);
        };
        match response.parsed() {
            Response::Done {
                tag: done,
                status,
                code,
                information,
            } if *done == tag => {
                return match status {
                    Status::Ok => Ok((answer, None)),
                    // The server's own words, its code first
                    // ([`super::words::refusal_text`]), which
                    // [`super::words::server_words`] puts before a user.
                    Status::No => Ok((
                        answer,
                        Some(Error::No(super::words::refusal_text(
                            code.as_ref(),
                            information.as_deref(),
                        ))),
                    )),
                    Status::Bad => Err(Error::Bad(super::words::refusal_text(
                        code.as_ref(),
                        information.as_deref(),
                    ))),
                    other => Err(Error::Io(std::io::Error::other(format!(
                        "status: {other:?}, code: {code:?}, information: {information:?}"
                    )))),
                };
            }
            parsed => answer.extend(Untagged::of(parsed)),
        }
    }
}

/// The checked forms of the session commands whose status `async-imap` drops.
pub(crate) trait Checked {
    /// `UID FETCH set query`: the messages, by the order the server sent
    /// them.
    async fn uid_fetch_checked(&mut self, set: &str, query: &str) -> Result<Vec<FetchResponse>>;

    /// `UID FETCH set query`, keeping what the server sent before a `NO`:
    /// the messages it sent, and its refusal of the others.
    async fn uid_fetch_partial(&mut self, set: &str, query: &str) -> Result<PartialFetch>;

    /// `UID STORE set change`: the messages whose flags the server reported.
    async fn uid_store_checked(&mut self, set: &str, change: &str) -> Result<Vec<FetchResponse>>;

    /// `UID EXPUNGE set` (RFC 4315 2.1): the sequence numbers removed, each
    /// as it was when removed (RFC 3501 7.4.1).
    async fn uid_expunge_checked(&mut self, set: &str) -> Result<Vec<u32>>;

    /// `EXPUNGE`: the same.
    async fn expunge_checked(&mut self) -> Result<Vec<u32>>;

    /// `UID SEARCH query`, for a query with no literal.
    async fn uid_search_checked(&mut self, query: &str) -> Result<HashSet<u32>>;

    /// `LIST reference pattern`.
    async fn list_checked(&mut self, reference: &str, pattern: &str) -> Result<Vec<ListedMailbox>>;

    /// `CAPABILITY`: the atom names, `AUTH=` mechanisms left out.
    async fn capabilities_checked(&mut self) -> Result<Vec<String>>;

    /// `NOOP`.
    async fn noop_checked(&mut self) -> Result<()>;
}

impl<T> Checked for async_imap::Session<T>
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send,
{
    async fn uid_fetch_checked(&mut self, set: &str, query: &str) -> Result<Vec<FetchResponse>> {
        Ok(fetches(
            answer_of(self, &format!("UID FETCH {set} {query}")).await?,
        ))
    }

    async fn uid_fetch_partial(&mut self, set: &str, query: &str) -> Result<PartialFetch> {
        let (answer, refused) =
            partial_answer_of(self, &format!("UID FETCH {set} {query}")).await?;
        Ok(PartialFetch {
            messages: fetches(answer),
            refused,
        })
    }

    async fn uid_store_checked(&mut self, set: &str, change: &str) -> Result<Vec<FetchResponse>> {
        Ok(fetches(
            answer_of(self, &format!("UID STORE {set} {change}")).await?,
        ))
    }

    async fn uid_expunge_checked(&mut self, set: &str) -> Result<Vec<u32>> {
        Ok(expunges(
            &answer_of(self, &format!("UID EXPUNGE {set}")).await?,
        ))
    }

    async fn expunge_checked(&mut self) -> Result<Vec<u32>> {
        Ok(expunges(&answer_of(self, "EXPUNGE").await?))
    }

    async fn uid_search_checked(&mut self, query: &str) -> Result<HashSet<u32>> {
        Ok(answer_of(self, &format!("UID SEARCH {query}"))
            .await?
            .into_iter()
            .flat_map(|untagged| match untagged {
                Untagged::Search(found) => found,
                _ => Vec::new(),
            })
            .collect())
    }

    async fn list_checked(&mut self, reference: &str, pattern: &str) -> Result<Vec<ListedMailbox>> {
        Ok(answer_of(
            self,
            &format!("LIST {} {}", quoted(reference), quoted(pattern)),
        )
        .await?
        .into_iter()
        .filter_map(|untagged| match untagged {
            Untagged::List(mailbox) => Some(mailbox),
            _ => None,
        })
        .collect())
    }

    async fn capabilities_checked(&mut self) -> Result<Vec<String>> {
        Ok(answer_of(self, "CAPABILITY")
            .await?
            .into_iter()
            .flat_map(|untagged| match untagged {
                Untagged::Capabilities(names) => names,
                _ => Vec::new(),
            })
            .collect())
    }

    async fn noop_checked(&mut self) -> Result<()> {
        answer_of(self, "NOOP").await.map(drop)
    }
}

/// What a `UID FETCH` answered, a `NO` after its data included.
#[derive(Debug)]
pub(crate) struct PartialFetch {
    /// The messages the server sent.
    pub messages: Vec<FetchResponse>,
    /// Its `NO`, when it refused some or all of the others.
    pub refused: Option<Error>,
}

/// The `FETCH` responses of an answer.
pub(crate) fn fetches(answer: Vec<Untagged>) -> Vec<FetchResponse> {
    answer
        .into_iter()
        .filter_map(|untagged| match untagged {
            Untagged::Fetch(fetch) => Some(fetch),
            _ => None,
        })
        .collect()
}

/// The `EXPUNGE` responses of an answer, in order.
pub(crate) fn expunges(answer: &[Untagged]) -> Vec<u32> {
    answer
        .iter()
        .filter_map(|untagged| match untagged {
            Untagged::Expunge(message) => Some(*message),
            _ => None,
        })
        .collect()
}

/// `text` as an IMAP quoted string (RFC 3501 4.3), as `async-imap` quotes a
/// `LIST`'s reference and pattern.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap_client::scripted::session as scripted;

    /// A connection that ends after part of an answer and before its tagged
    /// line is `ConnectionLost`, never the part taken for the whole.
    #[test]
    fn an_answer_cut_before_its_tagged_line_is_a_lost_connection() {
        futures::executor::block_on(async {
            let (mut session, _) = scripted(&[
                "A0001 OK LOGIN completed\r\n",
                "* 1 FETCH (UID 7 FLAGS (\\Seen))\r\n",
            ])
            .await;
            let lost = session
                .uid_fetch_partial("7", "(UID FLAGS)")
                .await
                .expect_err("the stream ended before the tagged line");
            assert!(matches!(lost, Error::ConnectionLost), "{lost:?}");

            let (mut session, _) = scripted(&["A0001 OK LOGIN completed\r\n"]).await;
            let lost = session.noop_checked().await.expect_err("no answer at all");
            assert!(matches!(lost, Error::ConnectionLost), "{lost:?}");
        });
    }

    /// A tagged line of another command, or an untagged `BYE`, does not end
    /// the answer; only this command's tag does.
    #[test]
    fn only_the_commands_own_tag_ends_its_answer() {
        futures::executor::block_on(async {
            let (mut session, _) = scripted(&[
                "A0001 OK LOGIN completed\r\n",
                "* SEARCH 3 4\r\nA0001 OK stray\r\n* SEARCH 5\r\nA0002 OK SEARCH completed\r\n",
            ])
            .await;
            let found = session.uid_search_checked("ALL").await.expect("the answer");
            assert_eq!(found, HashSet::from([3, 4, 5]));
        });
    }
}
