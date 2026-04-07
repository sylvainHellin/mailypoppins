use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Collapse consecutive hyphens and trim leading/trailing hyphens.
/// Used by slugify functions across multiple modules.
pub(crate) fn collapse_hyphens(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut prev_hyphen = false;
    for c in input.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    result.trim_matches('-').to_string()
}

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
    #[serde(default)]
    pub read: Option<bool>,
}

/// Frontmatter used when saving fetched emails to disk.
/// Produced via serde_yaml::to_string() to ensure correct quoting of special characters.
#[derive(Debug, Serialize)]
pub struct SaveFrontmatter {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    pub subject: String,
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub status: String,
    pub has_attachments: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<String>>,
    pub read: bool,
    pub fetched_at: String,
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

    #[test]
    fn test_inbox_frontmatter_read_true() {
        let yaml = r#"
from: "alice@example.com"
to: "bob@example.com"
subject: "Hi"
read: true
"#;
        let fm: InboxFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.read, Some(true));
    }

    #[test]
    fn test_inbox_frontmatter_read_false() {
        let yaml = r#"
from: "alice@example.com"
to: "bob@example.com"
subject: "Hi"
read: false
"#;
        let fm: InboxFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.read, Some(false));
    }

    #[test]
    fn test_inbox_frontmatter_read_missing_is_none() {
        let yaml = r#"
from: "alice@example.com"
to: "bob@example.com"
subject: "Hi"
"#;
        let fm: InboxFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.read, None);
    }

    #[test]
    fn test_save_frontmatter_serializes_read() {
        let fm = SaveFrontmatter {
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            cc: None,
            subject: "Test".to_string(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".to_string(),
            message_id: None,
            status: "inbox".to_string(),
            has_attachments: false,
            attachments: None,
            read: false,
            fetched_at: "2024-01-01T12:00:00Z".to_string(),
        };
        let yaml = serde_yaml::to_string(&fm).unwrap();
        assert!(yaml.contains("read: false"));
    }
}
