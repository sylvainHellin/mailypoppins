import { StrictMode } from "react";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "@/App";
import { resetMock } from "@/test/tauri-mock";
import { forgetTexts } from "@/app/readerMode";
import { forgetReads } from "@/lib/inflight";
import { setWidth } from "@/test/setup";

/**
 * Render the whole app against the fixture mock, at a window width, inside
 * StrictMode as main.tsx does, so effects and callback refs run attach,
 * detach, attach as in every dev build.
 */
export function renderApp(width = 1400, before?: () => void) {
  resetMock();
  forgetTexts();
  forgetReads();
  before?.();
  setWidth(width);
  const user = userEvent.setup();
  const utils = render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
  return { user, ...utils };
}

/** Wait until the shell has bootstrapped and the inbox list has loaded. */
export async function shellReady() {
  return screen.findByRole("listbox", {}, { timeout: 3000 });
}
