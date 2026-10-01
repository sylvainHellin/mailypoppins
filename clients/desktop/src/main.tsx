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

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LucideProvider strokeWidth={1.75}>
      <App />
    </LucideProvider>
  </React.StrictMode>,
);
