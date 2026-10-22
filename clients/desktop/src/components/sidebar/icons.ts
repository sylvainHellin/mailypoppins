import {
  Archive,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  FileText,
  Folder,
  Inbox,
  OctagonAlert,
  Send,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import type { MailboxKind } from "@/lib/gui-types";
import type { SyncHealthState } from "@/protocol/types";

export const MAILBOX_ICON: Record<MailboxKind, LucideIcon> = {
  inbox: Inbox,
  drafts: FileText,
  sent: Send,
  archive: Archive,
  trash: Trash2,
  junk: OctagonAlert,
  extra: Folder,
};

export const HEALTH: Record<SyncHealthState, { icon: LucideIcon; label: string; className: string }> = {
  ok: { icon: CircleCheck, label: "Last sync succeeded", className: "text-link" },
  failed: { icon: CircleAlert, label: "Last sync failed", className: "text-warning" },
  unknown: { icon: CircleDashed, label: "No sync has finished yet", className: "text-muted-foreground" },
};

/** A sidebar row's kind from its wire role; the trash and junk roles come with ticket 0141. */
export function kindOfRole(role: string): MailboxKind {
  switch (role) {
    case "inbox":
    case "drafts":
    case "sent":
    case "archive":
    case "trash":
    case "junk":
      return role;
    default:
      return "extra";
  }
}
