// Reactive UI preferences stored in localStorage.
import { useEffect, useState } from "react";

export type ToolsView = "compact" | "detailed";

const TOOLS_VIEW_KEY = "zerith.toolsView";
const TOOLS_VIEW_EVENT = "zerith:toolsView";

function parseToolsView(raw: unknown): ToolsView {
  return raw === "detailed" ? "detailed" : "compact";
}

export function getToolsView(): ToolsView {
  try {
    if (typeof localStorage === "undefined") return "compact";
    return parseToolsView(localStorage.getItem(TOOLS_VIEW_KEY));
  } catch {
    return "compact";
  }
}

export function setToolsView(v: ToolsView): void {
  const clean = parseToolsView(v);
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(TOOLS_VIEW_KEY, clean);
    }
  } catch {
    /* ignore storage errors */
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent<ToolsView>(TOOLS_VIEW_EVENT, { detail: clean }));
  }
}

export function useToolsView(): ToolsView {
  const [view, setViewState] = useState<ToolsView>(getToolsView);
  useEffect(() => {
    if (typeof window === "undefined") return;
    const onCustom = (e: Event) => {
      const detail = (e as CustomEvent<ToolsView>).detail;
      setViewState(detail === "compact" || detail === "detailed" ? detail : getToolsView());
    };
    const onStorage = (e: StorageEvent) => {
      if (e.key === TOOLS_VIEW_KEY) setViewState(getToolsView());
    };
    window.addEventListener(TOOLS_VIEW_EVENT, onCustom);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(TOOLS_VIEW_EVENT, onCustom);
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return view;
}
