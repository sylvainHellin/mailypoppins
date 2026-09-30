import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "@/App";
import { resetMock } from "@/test/tauri-mock";
import { setWidth } from "@/test/setup";

/** Render the whole app against the fixture mock, at a window width. */
export function renderApp(width = 1400, before?: () => void) {
  resetMock();
  before?.();
  setWidth(width);
  const user = userEvent.setup();
  const utils = render(<App />);
  return { user, ...utils };
}

/** Wait until the shell has bootstrapped and the inbox list has loaded. */
export async function shellReady() {
  return screen.findByRole("listbox", {}, { timeout: 3000 });
}
