# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright"]
# ///
"""Regenerate the README's desktop screenshot from the browser preview.

The browser preview serves the desktop frontend on its fixture backend
(clients/desktop/docs/browser-preview.md).
Before the app mounts, STAGE recasts the fixture for the picture: the people
become Mary Poppins characters, so no name can belong to a real person, the
opened message gets a cleaner rendition, and the seeded send hold, outbox row
and account warning that the tests need are cleared.
The fixture files themselves stay as the tests expect them.

    cd clients/desktop && pnpm preview:browser --host <tailnet-ip>
    uv run scripts/readme-screenshot.py --url http://<tailnet-ip>:1430/

The CDP endpoint defaults to BrowserOS neo on the home server.
"""

import argparse
import asyncio
import json
from pathlib import Path

from playwright.async_api import async_playwright

OUT = Path(__file__).resolve().parent.parent / "docs" / "screenshots" / "desktop-app.png"

LEDGER_HTML = """<!doctype html>
<html><head><meta charset="utf-8"><title>Quarterly ledger review</title>
<style>
body{font:15px/1.6 system-ui,-apple-system,"Segoe UI","Noto Sans",Helvetica,Arial,sans-serif;color:#1f2328;background:#fff;margin:28px 32px;max-width:640px}
p{margin:0 0 14px}
table{border-collapse:collapse;margin:4px 0 18px;font-variant-numeric:tabular-nums}
th,td{padding:6px 16px 6px 0;text-align:left;border-bottom:1px solid #e5e7eb}
th{font-weight:600;color:#57606a}
td.num,th.num{text-align:right}
.over{color:#b42318}
a{color:#0969da}
.sig{color:#57606a;font-size:13px;line-height:1.5;margin-top:22px}
</style></head><body>
<p>Hi Mary,</p>
<p>the Q3 ledger is attached. The nursery is the only line over budget this quarter:</p>
<table>
<tr><th>Line</th><th class="num">Budget</th><th class="num">Actual</th></tr>
<tr><td>Household</td><td class="num">4 200</td><td class="num">3 980</td></tr>
<tr><td>Nursery</td><td class="num">1 500</td><td class="num over">1 870</td></tr>
<tr><td>Kites and string</td><td class="num">120</td><td class="num">95</td></tr>
</table>
<p>Could we go through it on Thursday at ten? The figures are also on the <a href="https://bank.example/ledger">shared ledger page</a>.</p>
<p>Best regards,<br>George</p>
<div class="sig">George Banks<br>Dawes, Tomes, Mousley, Grubbs Fidelity Fiduciary Bank<br>17 Cherry Tree Lane, London</div>
</body></html>"""

LEDGER_TEXT = (
    "Hi Mary,\n\nthe Q3 ledger is attached. The nursery is the only line over budget this quarter.\n\n"
    "Could we go through it on Thursday at ten?\n\nBest regards,\nGeorge\n"
)

# Longest keys first, so a full name is replaced before its first name.
NAMES = {
    "Ivana Petrova": "George Banks",
    "Robin Keller": "Bert",
    "Robin Meyer": "Bert",
    "Marta Silva": "Jane Banks",
    "Anna Keller": "Ellen",
    "Chen Wei": "Admiral Boom",
    "ivana@example.com": "george@banks.example",
    "robin@example.com": "bert@cherrytree.example",
    "marta@example.com": "jane@banks.example",
    "anna@example.net": "ellen@cherrytree.example",
    "chen@example.org": "boom@cherrytree.example",
    "me@example.com": "mary@poppins.example",
    "Ivana": "George",
    "Robin": "Bert",
    "Marta": "Jane",
    "Anna": "Ellen",
    "Chen": "Admiral",
}

STAGE = """
(([names, html, text]) => {
  const recast = (value) => {
    let s = JSON.stringify(value);
    for (const [from, to] of Object.entries(names)) s = s.split(from).join(to);
    return JSON.parse(s);
  };
  let backend;
  Object.defineProperty(window, "mpFixture", {
    configurable: true,
    get: () => backend,
    set: (b) => {
      backend = b;
      const fx = b.fixtures;
      for (const key of ["messages", "drafts", "draftBodies", "html", "contacts", "calendar", "bootstrap"]) fx[key] = recast(fx[key]);
      const snap = fx.bootstrap.snapshot;
      snap.holds = [];
      snap.diagnostics = [];
      for (const counts of Object.values(snap.outbox)) counts.queued = 0;
      for (const account of snap.accounts) account.sync_health = { state: "ok" };
      fx.html["1001"] = html;
      fx.messages.work.inbox.find((row) => row.id === 1001).body = text;
      b.resetMock();
      b.mock.editorFixture = true;
    },
  });
})
"""


async def main(url: str, cdp: str) -> None:
    async with async_playwright() as p:
        browser = await p.chromium.connect_over_cdp(cdp)
        ctx = await browser.new_context(
            viewport={"width": 1440, "height": 900}, device_scale_factor=2, color_scheme="dark"
        )
        page = await ctx.new_page()
        await page.add_init_script(
            f"{STAGE}({json.dumps([NAMES, LEDGER_HTML, LEDGER_TEXT])})"
        )
        await page.goto(url)
        await page.wait_for_timeout(2500)
        await page.get_by_text("Quarterly ledger review").first.click()
        await page.keyboard.press("Enter")
        await page.wait_for_timeout(2500)
        await page.screenshot(path=str(OUT))
        await ctx.close()
    print(OUT)


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://100.70.127.88:1430/")
    ap.add_argument("--cdp", default="http://127.0.0.1:3311")
    a = ap.parse_args()
    asyncio.run(main(a.url, a.cdp))
