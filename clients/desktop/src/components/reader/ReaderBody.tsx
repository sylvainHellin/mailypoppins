// ─────────────────────────────────────────────────────────────────────────
// <ReaderBody>: the component boundary U4 replaces.
//
// M1 renders the stored plain text (`message_text`) in a <pre>. U4 swaps the
// inside for the sandboxed `mpmsg://` iframe (`MessageMeta.html_url`,
// sandbox="allow-popups", no allow-scripts; see docs/rust-layer.md "The
// reader"). Keep the props: the iframe needs `htmlUrl`, and the text stays as
// the fallback for a message without markup.
// ─────────────────────────────────────────────────────────────────────────

export type ReaderBodyProps = {
  account: string;
  rowId: number;
  /** `MessageMeta.html_url`, unused until U4. */
  htmlUrl: string;
  text: string | null;
};

export function ReaderBody({ text }: ReaderBodyProps) {
  if (text === null || text.trim() === "") {
    return <p className="px-5 py-4 text-sm text-muted-foreground">This message has no plain-text body.</p>;
  }
  return (
    <pre
      data-slot="reader-body"
      className="px-5 py-4 font-sans text-sm leading-relaxed break-words whitespace-pre-wrap text-foreground"
    >
      {text}
    </pre>
  );
}
