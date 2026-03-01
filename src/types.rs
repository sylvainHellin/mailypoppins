use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EmailStatus {
    Draft,
    Approved,
    Sent,
    Inbox,
    Archived,
}

impl std::fmt::Display for EmailStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailStatus::Draft => write!(f, "draft"),
            EmailStatus::Approved => write!(f, "approved"),
            EmailStatus::Sent => write!(f, "sent"),
            EmailStatus::Inbox => write!(f, "inbox"),
            EmailStatus::Archived => write!(f, "archived"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct EmailFrontmatter {
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) cc: Option<String>,
    #[serde(default)]
    pub(crate) bcc: Option<String>,
    pub(crate) subject: String,
    pub(crate) status: EmailStatus,
    #[serde(default)]
    pub(crate) from: Option<String>,
    #[serde(default)]
    pub(crate) reply_to: Option<String>,
    #[serde(default)]
    pub(crate) attachments: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) sent_at: Option<String>,
    #[serde(default)]
    pub(crate) sent_via: Option<String>,
    #[serde(default)]
    pub(crate) message_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct EmailDraft {
    pub(crate) path: PathBuf,
    pub(crate) frontmatter: EmailFrontmatter,
    pub(crate) body_markdown: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InboxFrontmatter {
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) cc: Option<String>,
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) date: Option<String>,
    #[serde(default)]
    pub(crate) message_id: Option<String>,
}
