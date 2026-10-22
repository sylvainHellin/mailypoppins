# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright"]
# ///
"""Regenerate the README's desktop screenshot from the browser preview.

The browser preview serves the desktop frontend on its fixture backend, so the
screenshot shows fictional mail only (clients/desktop/docs/browser-preview.md).

    cd clients/desktop && pnpm preview:browser --host <tailnet-ip>
    uv run scripts/readme-screenshot.py --url http://<tailnet-ip>:1430/

The CDP endpoint defaults to BrowserOS neo on the home server.
"""

import argparse
import asyncio
from pathlib import Path

from playwright.async_api import async_playwright

OUT = Path(__file__).resolve().parent.parent / "docs" / "screenshots" / "desktop-app.png"


async def main(url: str, cdp: str) -> None:
    async with async_playwright() as p:
        browser = await p.chromium.connect_over_cdp(cdp)
        ctx = await browser.new_context(
            viewport={"width": 1440, "height": 900}, device_scale_factor=2, color_scheme="dark"
        )
        page = await ctx.new_page()
        await page.goto(url)
        await page.wait_for_timeout(2500)
        # The fixture starts with an armed send hold and an account warning banner.
        await page.get_by_role("button", name="Cancel").first.click(timeout=5000)
        await page.locator("button:has(svg.lucide-x)").first.click(timeout=5000)
        await page.get_by_text("Quarterly ledger review").first.click()
        await page.keyboard.press("Enter")
        # Wait out the "Send cancelled" toast.
        await page.wait_for_timeout(9000)
        await page.screenshot(path=str(OUT))
        await ctx.close()
    print(OUT)


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://100.70.127.88:1430/")
    ap.add_argument("--cdp", default="http://127.0.0.1:3311")
    a = ap.parse_args()
    asyncio.run(main(a.url, a.cdp))
