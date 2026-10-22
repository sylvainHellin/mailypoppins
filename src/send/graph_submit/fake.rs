//! An in-memory Graph mailbox behind [`GraphSendApi`], shared by the staged
//! send's tests and the daemon's `outbox discard` tests: every crash window is
//! a future dropped at a chosen request, and nothing here touches the network
//! or a real account.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::GraphSendApi;
use crate::graph::{DraftState, OutgoingAttachment, ServerCopies};
use crate::outbox;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Create,
    Attach,
    Send,
    SendMime,
    /// `permanentDelete`; only [`Fault::Refuse`] applies.
    Discard,
    /// `server_copies` and `draft_state`; only [`Fault::Refuse`] applies, as a
    /// mailbox that cannot be reached.
    Lookup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The process dies waiting, before the server acted.
    HangBefore,
    /// The server acted, then the process died waiting for the answer.
    HangAfter,
    /// An outright refusal, an HTTP 4xx.
    Refuse,
    /// The server acted and the answer timed out.
    TimeoutAfter,
    /// Graph took the delivering request and answered 504: the message is on
    /// its way (a draft in the Outbox, not yet in Sent Items) until
    /// [`FakeGraph::deliver`].
    Gateway,
    /// Before `send` acted, the draft left Drafts: an earlier send of it was
    /// taken late and is on its way. The send answers 404.
    Vanished,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Item {
    pub(crate) message_id: String,
    /// Our `Message-ID` in [`crate::graph::MESSAGE_ID_PROPERTY`], on a JSON
    /// draft.
    pub(crate) property: Option<String>,
    pub(crate) mime: Option<Vec<u8>>,
    pub(crate) attachments: Vec<OutgoingAttachment>,
}

impl Item {
    fn is(&self, message_id: &str) -> bool {
        self.message_id == message_id || self.property.as_deref() == Some(message_id)
    }
}

#[derive(Default)]
pub(crate) struct Server {
    pub(crate) next_id: u32,
    pub(crate) drafts: BTreeMap<String, Item>,
    /// Taken by Graph and not yet filed in Sent Items, with the draft id a
    /// staged send had.
    pub(crate) queued: Vec<(Option<String>, Item)>,
    pub(crate) sent: Vec<Item>,
    pub(crate) faults: Vec<(Step, Fault)>,
    pub(crate) discarded: Vec<String>,
    pub(crate) sent_drafts: Vec<String>,
}

impl Server {
    /// Take one message: filed at once, or queued when Graph answered 504.
    fn take(&mut self, draft: Option<String>, item: Item, queue: bool) {
        if queue {
            self.queued.push((draft, item));
            return;
        }
        if let Some(id) = draft {
            self.sent_drafts.push(id);
        }
        self.sent.push(item);
    }
}

#[derive(Default)]
pub(crate) struct FakeGraph(pub(crate) Mutex<Server>);

fn message_id_of_mime(b64: &str) -> (String, Vec<u8>) {
    use base64::Engine;
    let mime = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("standard base64");
    (crate::send::message_id_of(&mime), mime)
}

impl FakeGraph {
    pub(crate) fn with_fault(step: Step, fault: Fault) -> Self {
        let fake = FakeGraph::default();
        fake.0.lock().unwrap().faults.push((step, fault));
        fake
    }

    fn take_fault(&self, step: Step) -> Option<Fault> {
        let mut server = self.0.lock().unwrap();
        let at = server.faults.iter().position(|(s, _)| *s == step)?;
        Some(server.faults.remove(at).1)
    }

    /// Whether lookups fail: a [`Step::Lookup`] fault stands until removed.
    fn offline(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .faults
            .iter()
            .any(|(s, f)| *s == Step::Lookup && *f == Fault::Refuse)
    }

    /// Take the next fault of `step` only when it is `fault`.
    fn take_this_fault(&self, step: Step, fault: Fault) -> bool {
        let mut server = self.0.lock().unwrap();
        match server.faults.iter().position(|(s, _)| *s == step) {
            Some(at) if server.faults[at].1 == fault => {
                server.faults.remove(at);
                true
            }
            _ => false,
        }
    }

    /// Run one request: the fault decides whether the server acts and what
    /// the client hears. The action is told whether Graph only queues the
    /// message ([`Fault::Gateway`]).
    async fn act<T: Send>(
        &self,
        step: Step,
        action: impl FnOnce(&mut Server, bool) -> Result<T> + Send,
    ) -> Result<T> {
        let fault = self.take_fault(step);
        match fault {
            Some(Fault::HangBefore) => std::future::pending::<()>().await,
            Some(Fault::Refuse) => {
                return Err(graph_status(400));
            }
            _ => {}
        }
        let value = action(&mut self.0.lock().unwrap(), fault == Some(Fault::Gateway))?;
        match fault {
            Some(Fault::HangAfter) => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            Some(Fault::TimeoutAfter) => Err(anyhow!(outbox::AmbiguousSubmission(
                "operation timed out".to_string()
            ))),
            Some(Fault::Gateway) => Err(graph_status(504)),
            _ => Ok(value),
        }
    }

    pub(crate) fn sent(&self) -> Vec<Item> {
        self.0.lock().unwrap().sent.clone()
    }

    /// Graph finishes what it queued: the messages land in Sent Items.
    pub(crate) fn deliver(&self) {
        let mut server = self.0.lock().unwrap();
        for (draft, item) in std::mem::take(&mut server.queued) {
            server.take(draft, item, false);
        }
    }

    pub(crate) fn drafts(&self) -> Vec<String> {
        self.0.lock().unwrap().drafts.keys().cloned().collect()
    }

    /// A draft created out of band, as an earlier attempt left it.
    pub(crate) fn plant_draft(&self, message_id: &str) -> String {
        let mut server = self.0.lock().unwrap();
        server.next_id += 1;
        let id = format!("planted-{}", server.next_id);
        server.drafts.insert(
            id.clone(),
            Item {
                message_id: message_id.to_string(),
                ..Item::default()
            },
        );
        id
    }
}

/// A delivering request answered with `status`, read the way the real client
/// reads it ([`crate::graph::delivering_status_error`]).
pub(crate) fn graph_status(status: u16) -> anyhow::Error {
    crate::graph::delivering_status_error(
        status,
        false,
        true,
        format!("HTTP {status} from the fake"),
    )
}

impl GraphSendApi for FakeGraph {
    fn send_mime(&self, mime_base64: String) -> impl Future<Output = Result<()>> + Send {
        self.act(Step::SendMime, move |server, queue| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            let item = Item {
                message_id,
                mime: Some(mime),
                ..Item::default()
            };
            server.take(None, item, queue);
            Ok(())
        })
    }

    fn create_draft_mime(
        &self,
        mime_base64: String,
    ) -> impl Future<Output = Result<String>> + Send {
        self.act(Step::Create, move |server, _| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id,
                    mime: Some(mime),
                    ..Item::default()
                },
            );
            Ok(id)
        })
    }

    fn create_draft_json(&self, message: &Value) -> impl Future<Output = Result<String>> + Send {
        let property = message["singleValueExtendedProperties"]
            .as_array()
            .and_then(|props| {
                props
                    .iter()
                    .find(|p| p["id"] == crate::graph::MESSAGE_ID_PROPERTY)
            })
            .and_then(|p| p["value"].as_str())
            .map(str::to_string);
        self.act(Step::Create, move |server, _| {
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id: format!("<exchange-{}@fake>", server.next_id),
                    property,
                    ..Item::default()
                },
            );
            Ok(id)
        })
    }

    fn add_attachment(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        let attachment = attachment.clone();
        self.act(Step::Attach, move |server, _| {
            server
                .drafts
                .get_mut(&draft_id)
                .ok_or_else(|| anyhow!("HTTP 404"))?
                .attachments
                .push(attachment);
            Ok(())
        })
    }

    fn send_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        if self.take_this_fault(Step::Send, Fault::Vanished) {
            let mut server = self.0.lock().unwrap();
            if let Some(item) = server.drafts.remove(&draft_id) {
                server.queued.push((Some(draft_id.clone()), item));
            }
        }
        self.act(Step::Send, move |server, queue| {
            let item = server
                .drafts
                .remove(&draft_id)
                .ok_or_else(|| graph_status(404))?;
            server.take(Some(draft_id), item, queue);
            Ok(())
        })
    }

    fn discard_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        let refused = self.take_this_fault(Step::Discard, Fault::Refuse);
        async move {
            if refused {
                return Err(anyhow!("HTTP 503 from the fake: the draft stays"));
            }
            let mut server = self.0.lock().unwrap();
            server.drafts.remove(&draft_id);
            server.discarded.push(draft_id);
            Ok(())
        }
    }

    fn server_copies(
        &self,
        internet_message_id: &str,
    ) -> impl Future<Output = Result<ServerCopies>> + Send {
        let wanted = internet_message_id.to_string();
        let offline = self.offline();
        async move {
            if offline {
                return Err(anyhow!("the fake mailbox cannot be reached"));
            }
            let server = self.0.lock().unwrap();
            Ok(ServerCopies {
                sent: server.sent.iter().any(|i| i.is(&wanted)),
                drafts: server
                    .drafts
                    .iter()
                    .filter(|(_, i)| i.is(&wanted))
                    .map(|(id, _)| id.clone())
                    .collect(),
            })
        }
    }

    fn draft_state(&self, draft_id: &str) -> impl Future<Output = Result<DraftState>> + Send {
        let draft_id = draft_id.to_string();
        let offline = self.offline();
        async move {
            if offline {
                return Err(anyhow!("the fake mailbox cannot be reached"));
            }
            let server = self.0.lock().unwrap();
            Ok(if server.drafts.contains_key(&draft_id) {
                DraftState::InDrafts
            } else if server
                .queued
                .iter()
                .any(|(id, _)| id.as_deref() == Some(&draft_id))
            {
                DraftState::Sending
            } else {
                // Filed in Sent Items under a new id, or deleted.
                DraftState::Gone
            })
        }
    }
}
