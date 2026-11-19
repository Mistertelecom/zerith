import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "./styles/index.css";

// The webview opens no window of its own: a link in an answer (or in Ajustes)
// goes to the system browser through `open_url`. One capture-phase listener,
// so every `<a href>` in the app is covered without touching each component.
document.addEventListener(
  "click",
  (e) => {
    const a = (e.target as HTMLElement | null)?.closest?.("a[href]") as HTMLAnchorElement | null;
    if (!a) return;
    const href = a.getAttribute("href") ?? "";
    if (!/^(https?:|mailto:)/i.test(href)) return;
    e.preventDefault();
    void invoke("open_url", { url: href });
  },
  true,
);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
