import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { StoreProvider } from "@/app/store";
import type { MessageMeta } from "@/lib/gui-types";
import { ReaderHeader } from "@/components/reader/ReaderHeader";

afterEach(cleanup);

function meta(from: string, to: string, cc: string | null): MessageMeta {
  return {
    row_id: 1,
    html_url: "mpmsg://localhost/work/1",
    selector: "mp://work/inbox/1",
    account: "work",
    mailbox: "INBOX",
    message_id: "<1@x.example>",
    from,
    to,
    cc,
    subject: "Angebot",
    date: null,
    flags: [],
    invite: false,
    attachments: [],
  };
}

/** The value the header shows beside `name`. */
function field(name: string): string | null {
  return screen.getByText(name, { selector: "dt" }).nextElementSibling?.textContent ?? null;
}

describe("the reader header", () => {
  it("shows a quoted display name unquoted and unescaped, and a name that repeats the address not at all", () => {
    render(
      <StoreProvider>
        <ReaderHeader
          meta={meta(
            '"Doe, \\"JJ\\" Jane" <jane@x.example>',
            '"robin@y.example" <robin@y.example>, "Roe, Rob" <rob@y.example>',
            '"c@z.example" <c@z.example>',
          )}
        />
      </StoreProvider>,
    );
    expect(field("From")).toBe('Doe, "JJ" Jane <jane@x.example>');
    expect(field("To")).toBe("robin@y.example, Roe, Rob <rob@y.example>");
    expect(field("Cc")).toBe("c@z.example");
  });
});
