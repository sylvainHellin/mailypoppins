use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailStatus {
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
pub struct EmailFrontmatter {
    pub to: String,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub bcc: Option<String>,
    pub subject: String,
    pub status: EmailStatus,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<String>>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub sent_via: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug)]
pub struct EmailDraft {
    pub path: PathBuf,
    pub frontmatter: EmailFrontmatter,
    pub body_markdown: String,
}

#[derive(Debug, Deserialize)]
pub struct InboxFrontmatter {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub cc: Option<String>,
    pub subject: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
}
