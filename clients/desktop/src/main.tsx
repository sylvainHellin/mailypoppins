import React from "react";
import ReactDOM from "react-dom/client";
import { LucideProvider } from "lucide-react";
import App from "./App";
import { applyTheme, DEFAULT_THEME, preloadTheme } from "@/app/theme";
import "./index.css";

// Dark at once, as index.html already paints it; the stored theme replaces it
// when `setting_get` answers, which this read starts before the first render.
applyTheme(DEFAULT_THEME);
void preloadTheme();

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

// A dev build at `#terminal-fixture` shows the embedded editor's pane on an
// echo bridge instead of the app (TerminalPane.fixture.tsx); a release build
// drops the branch and its chunk.
if (import.meta.env.DEV && window.location.hash === "#terminal-fixture") {
  void import("@/components/compose/TerminalPane.fixture").then(({ TerminalFixture }) =>
    root.render(
      <React.StrictMode>
        <TerminalFixture />
      </React.StrictMode>,
    ),
  );
} else {
  root.render(
    <React.StrictMode>
      <LucideProvider strokeWidth={1.75}>
        <App />
      </LucideProvider>
    </React.StrictMode>,
  );
}
