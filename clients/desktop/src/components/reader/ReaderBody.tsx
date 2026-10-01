// <ReaderBody>: the message body as its own document, the `mpmsg` URL the
// Rust layer serves (clients/desktop/docs/rust-layer.md, "The reader").
//
// The frame is sandboxed with `allow-popups` alone: no scripts, no same
// origin, no forms, no top navigation. `allow-popups` is there so that a
// `target=_blank` link reaches the Rust layer's `on_new_window`, which refuses
// it and logs it; without it the sandbox drops the click before Rust sees it.
// Remote content is blocked by the reader CSP the scheme sends as a header.
// A message without markup comes back as a plain-text document on the same
// URL (`X-Mp-Rendition: text`), so there is one path here.
//
// <ReaderText> is the reader's text mode (docs/reader.md, "Text mode"): the
// stored plain text `message_text` answers, the TUI preview's body, drawn on
// the app's own surface in its theme, with no frame. It flows in the reader's
// scroll container, so the reader's scroll keys move it.

import { Fragment, useEffect, useState } from "react";
import { FileText } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";
import { cachedText, isQuoted, rememberText } from "@/app/readerMode";
import { readerKey } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type GuiError } from "@/lib/gui-types";

/** The exact sandbox the reader frame runs in; a test pins it. */
export const READER_SANDBOX = "allow-popups";

export type ReaderBodyProps = {
  /** `MessageMeta.html_url`, as the Rust layer hands it over. */
  htmlUrl: string;
  /** The message subject, for the frame's accessible name. */
  subject: string | null;
};

export function ReaderBody({ htmlUrl, subject }: ReaderBodyProps) {
  // Keyed on the URL, so a new message starts with the skeleton again.
  return <ReaderFrame key={htmlUrl} htmlUrl={htmlUrl} subject={subject} />;
}

function ReaderFrame({ htmlUrl, subject }: ReaderBodyProps) {
  const [loaded, setLoaded] = useState(false);
  return (
    <div data-slot="reader-body" className="relative min-h-80 flex-1">
      {loaded ? null : (
        <div aria-hidden="true" data-slot="reader-body-loading" className="absolute inset-0 flex flex-col gap-3 p-5">
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="mt-2 h-40 w-full" />
        </div>
      )}
      <iframe
        src={htmlUrl}
        sandbox={READER_SANDBOX}
        referrerPolicy="no-referrer"
        title={`Message body: ${subject ?? "(no subject)"}`}
        onLoad={() => setLoaded(true)}
        className={`absolute inset-0 size-full border-0 bg-reader-canvas ${loaded ? "" : "invisible"}`}
      />
    </div>
  );
}

export type ReaderTextProps = {
  /** The daemon instance the row id belongs to: a restarted daemon may give the id to another message. */
  instance: string;
  account: string;
  rowId: number;
  /** The message's `Message-ID`, which a row id is checked against. */
  messageId: string;
  /** The reader load the headers came from: a reload of the message reads its text again. */
  version: number;
  /** The message subject, for the text's accessible name. */
  subject: string | null;
};

type TextResult = { kind: "text"; body: string | null } | { kind: "error"; error: GuiError };

/**
 * The stored plain text of the open message, read once per daemon instance,
 * message and load and cached; a message change drops an answer still on its way. Line breaks
 * stay as stored, and a line quoted with `>` is muted.
 */
export function ReaderText({ instance, account, rowId, messageId, version, subject }: ReaderTextProps) {
  // A row id is per daemon instance (state.ts, `MessageRef`), so the cache
  // and the shown text are keyed by the instance and the Message-ID too.
  const message = `${instance}/${readerKey(account, rowId)}/${messageId}`;
  const key = `${message}@${version}`;
  const [shown, setShown] = useState<{ message: string; result: TextResult } | null>(() => {
    const hit = cachedText(key);
    return hit ? { message, result: { kind: "text", body: hit.body } } : null;
  });

  useEffect(() => {
    const hit = cachedText(key);
    if (hit) {
      setShown({ message, result: { kind: "text", body: hit.body } });
      return;
    }
    let live = true;
    cmd
      .messageText(account, rowId)
      .then((text) => {
        rememberText(key, { body: text.body });
        if (live) setShown({ message, result: { kind: "text", body: text.body } });
      })
      .catch((e: unknown) => {
        if (live) setShown({ message, result: { kind: "error", error: asGuiError(e) } });
      });
    return () => {
      live = false;
    };
  }, [account, rowId, key, message]);

  // A reload of the same message keeps its text up until the new one lands.
  const result = shown && shown.message === message ? shown.result : null;

  if (!result) {
    return (
      <div data-slot="reader-body" className="relative min-h-80 flex-1">
        <div aria-hidden="true" data-slot="reader-body-loading" className="flex flex-col gap-3 p-5">
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
        </div>
      </div>
    );
  }
  if (result.kind === "error") {
    return (
      <p role="alert" data-slot="reader-body" className="p-5 text-sm text-destructive">
        The text did not load: {result.error.message}
      </p>
    );
  }
  if (result.body === null || result.body.trim() === "") {
    return (
      <div
        role="status"
        data-slot="reader-body"
        className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center text-sm text-muted-foreground"
      >
        <FileText aria-hidden="true" className="size-6" />
        <p>No text body</p>
        <p className="text-xs">t t shows the HTML version.</p>
      </div>
    );
  }
  const lines = result.body.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
  return (
    <div data-slot="reader-body" className="flex-1 bg-background px-5 py-4">
      <pre
        aria-label={`Message text: ${subject ?? "(no subject)"}`}
        data-slot="reader-text"
        className="m-0 font-mono text-sm leading-relaxed break-words whitespace-pre-wrap text-foreground"
      >
        {lines.map((line, i) => (
          <Fragment key={i}>
            {i > 0 ? "\n" : null}
            {isQuoted(line) ? (
              <span data-quoted="true" className="text-muted-foreground">
                {line}
              </span>
            ) : (
              line
            )}
          </Fragment>
        ))}
      </pre>
    </div>
  );
}
