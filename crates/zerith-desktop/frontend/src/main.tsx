import { Component, StrictMode, type ErrorInfo, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
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

class ErrorBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state: { error: string | null } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error("Zerith UI render error:", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div
          style={{
            padding: "32px",
            fontFamily: "system-ui, sans-serif",
            color: "var(--label-primary, #e5e5e5)",
            background: "var(--bg-base, #111)",
            height: "100vh",
            boxSizing: "border-box",
          }}
        >
          <h2 style={{ marginTop: 0 }}>Ocorreu um erro inesperado na interface</h2>
          <p style={{ opacity: 0.8 }}>{this.state.error}</p>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => window.location.reload()}
          >
            Recarregar janela
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
