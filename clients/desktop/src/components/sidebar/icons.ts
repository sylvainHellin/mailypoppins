import {
  Archive,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  FileText,
  Folder,
  Inbox,
  Send,
  type LucideIcon,
} from "lucide-react";
import type { MailboxKind } from "@/lib/gui-types";
import type { SyncHealthState } from "@/protocol/types";

export const MAILBOX_ICON: Record<MailboxKind, LucideIcon> = {
  inbox: Inbox,
  drafts: FileText,
  sent: Send,
  archive: Archive,
  extra: Folder,
};

export const HEALTH: Record<SyncHealthState, { icon: LucideIcon; label: string; className: string }> = {
  ok: { icon: CircleCheck, label: "Last sync succeeded", className: "text-link" },
  failed: { icon: CircleAlert, label: "Last sync failed", className: "text-warning" },
  unknown: { icon: CircleDashed, label: "No sync has finished yet", className: "text-muted-foreground" },
};

export function kindOfRole(role: string): MailboxKind {
  return role === "inbox" || role === "drafts" || role === "sent" || role === "archive" ? role : "extra";
}
