// The active profile at the foot of the sidebar (PERSO-176): the Mac hides
// the title bar, so the foot is where a window on the demo profile says so.

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";

const sidebar = () => screen.getByTestId("sidebar-profile");

describe("the sidebar's profile foot", () => {
  it("names the profile the app was launched on", async () => {
    renderApp(1400, () => {
      mock.profile = "demo";
    });
    await shellReady();
    await waitFor(() => expect(sidebar()).toBeInTheDocument());
    expect(within(sidebar()).getByText("profile demo")).toBeInTheDocument();
    expect(sidebar()).toHaveAttribute("title", "Running on profile demo");
  });

  it("shows nothing on the default profile", async () => {
    renderApp();
    await shellReady();
    await waitFor(() => expect(mock.calls.some((c) => c.cmd === "version_info")).toBe(true));
    expect(screen.queryByTestId("sidebar-profile")).toBeNull();
  });
});
