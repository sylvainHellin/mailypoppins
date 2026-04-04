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
    #[serde(default)]
    pub attachments: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_status_serde_roundtrip() {
        for status in [
            EmailStatus::Draft,
            EmailStatus::Approved,
            EmailStatus::Sent,
            EmailStatus::Inbox,
            EmailStatus::Archived,
        ] {
            let yaml = serde_yaml::to_string(&status).unwrap();
            let back: EmailStatus = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn test_email_status_display() {
        assert_eq!(EmailStatus::Draft.to_string(), "draft");
        assert_eq!(EmailStatus::Approved.to_string(), "approved");
        assert_eq!(EmailStatus::Sent.to_string(), "sent");
        assert_eq!(EmailStatus::Inbox.to_string(), "inbox");
        assert_eq!(EmailStatus::Archived.to_string(), "archived");
    }

    #[test]
    fn test_email_frontmatter_serde_roundtrip() {
        let fm = EmailFrontmatter {
            to: "alice@example.com".to_string(),
            cc: Some("bob@example.com".to_string()),
            bcc: None,
            subject: "Test Subject".to_string(),
            status: EmailStatus::Draft,
            from: Some("sender@example.com".to_string()),
            reply_to: None,
            attachments: Some(vec!["file.pdf".to_string()]),
            sent_at: None,
            sent_via: None,
            message_id: Some("<test@example.com>".to_string()),
        };
        let yaml = serde_yaml::to_string(&fm).unwrap();
        let back: EmailFrontmatter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.to, fm.to);
        assert_eq!(back.cc, fm.cc);
        assert_eq!(back.subject, fm.subject);
        assert_eq!(back.status, fm.status);
        assert_eq!(back.from, fm.from);
        assert_eq!(back.attachments, fm.attachments);
        assert_eq!(back.message_id, fm.message_id);
    }

    #[test]
    fn test_inbox_frontmatter_deserialize() {
        let yaml = r#"
from: "alice@example.com"
to: "bob@example.com"
cc: "carol@example.com"
subject: "Meeting notes"
date: "Mon, 01 Jan 2024 12:00:00 +0000"
message_id: "<abc123@example.com>"
attachments:
  - "notes.pdf"
"#;
        let fm: InboxFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.from, "alice@example.com");
        assert_eq!(fm.to, "bob@example.com");
        assert_eq!(fm.cc, Some("carol@example.com".to_string()));
        assert_eq!(fm.subject, "Meeting notes");
        assert_eq!(fm.date, Some("Mon, 01 Jan 2024 12:00:00 +0000".to_string()));
        assert_eq!(fm.message_id, Some("<abc123@example.com>".to_string()));
        assert_eq!(fm.attachments, Some(vec!["notes.pdf".to_string()]));
    }

    #[test]
    fn test_inbox_frontmatter_minimal() {
        let yaml = r#"
from: "alice@example.com"
to: "bob@example.com"
subject: "Hi"
"#;
        let fm: InboxFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.from, "alice@example.com");
        assert!(fm.cc.is_none());
        assert!(fm.date.is_none());
        assert!(fm.message_id.is_none());
        assert!(fm.attachments.is_none());
    }
}
