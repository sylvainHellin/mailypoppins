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

import { useState } from "react";
import { Skeleton } from "@/components/ui/skeleton";

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
