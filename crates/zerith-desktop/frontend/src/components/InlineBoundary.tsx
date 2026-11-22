import { Component, type ErrorInfo, type ReactNode } from "react";

/** Catches a render/lazy-load error of one panel so the rest of the app keeps
 * working; "Tentar de novo" remounts the children. */
export class InlineBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state: { error: string | null } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error("Zerith panel error:", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="callout danger" role="alert" style={{ margin: 16 }}>
        <div>
          <strong>Esta tela falhou ao abrir</strong>
          <p>{this.state.error}</p>
          <button type="button" className="btn btn-sm" onClick={() => this.setState({ error: null })}>
            Tentar de novo
          </button>
        </div>
      </div>
    );
  }
}
