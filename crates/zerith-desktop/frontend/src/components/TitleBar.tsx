import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Bot, Copy, Globe, MessageSquare, Minus, Square, Workflow, X } from "lucide-react";
import { ZerithMark } from "./ui";
import { UpdatePill } from "./Update";
import { LANG_OPTIONS, LANG_SHORT, setLang, tr, useLang, type Lang } from "../lib/i18n";

export type AppView = "chat" | "bots" | "workflows";

/** The window's own title bar: the native one is off (decorations: false) so
 * the bar can sit on the app's black. Empty space drags the window (Tauri's
 * data-tauri-drag-region), double click maximises. */
export function TitleBar({
  title,
  brand = false,
  view,
  onView,
}: {
  title?: string;
  brand?: boolean;
  view?: AppView;
  onView?: (v: AppView) => void;
}) {
  const [max, setMax] = useState(false);
  const lang = useLang();
  useEffect(() => {
    let win: ReturnType<typeof getCurrentWindow> | null = null;
    try {
      win = getCurrentWindow();
    } catch {
      return; // plain browser preview: no window to drive
    }
    const sync = () => void win!.isMaximized().then(setMax).catch(() => undefined);
    sync();
    const off = win.onResized(sync);
    return () => void off.then((u) => u()).catch(() => undefined);
  }, []);
  const act = (fn: (w: ReturnType<typeof getCurrentWindow>) => Promise<void>) => () => {
    try {
      void fn(getCurrentWindow()).catch(() => undefined);
    } catch {
      /* preview */
    }
  };
  return (
    <header className="titlebar" data-tauri-drag-region>
      {/* the sidebar already carries the brand; only the boot screens show it here */}
      {brand ? (
        <span className="titlebar-brand" data-tauri-drag-region>
          <ZerithMark size={16} />
          <span data-tauri-drag-region>Zerith</span>
        </span>
      ) : null}
      {title ? (
        <span className="titlebar-title" data-tauri-drag-region>
          {title}
        </span>
      ) : null}
      {!brand && view && onView ? (
        <nav className="titlebar-tabs" aria-label={tr(lang, "Modo do aplicativo", "App view")}>
          <button
            type="button"
            className={"titlebar-tab" + (view === "chat" ? " on" : "")}
            onClick={() => onView("chat")}
          >
            <MessageSquare size={12} strokeWidth={2.1} />
            <span>{tr(lang, "Chat", "Chat")}</span>
          </button>
          <button
            type="button"
            className={"titlebar-tab" + (view === "bots" ? " on" : "")}
            onClick={() => onView("bots")}
          >
            <Bot size={12} strokeWidth={2.1} />
            <span>Bots</span>
          </button>
          <button
            type="button"
            className={"titlebar-tab" + (view === "workflows" ? " on" : "")}
            onClick={() => onView("workflows")}
          >
            <Workflow size={12} strokeWidth={2.1} />
            <span>{tr(lang, "Automações", "Workflows")}</span>
          </button>
        </nav>
      ) : null}
      <span className="titlebar-fill" data-tauri-drag-region />
      {/* boot screens (brand) have no app yet to update */}
      {brand ? null : <UpdatePill />}
      {brand ? null : (
        <button
          type="button"
          className="pill"
          style={{ height: 22, padding: "0 8px", fontSize: 11, marginRight: 6 }}
          title={tr(
            lang,
            "Trocar idioma (English, Português, Español, Français, Deutsch, 中文)",
            "Switch language (English, Português, Español, Français, Deutsch, 中文)",
          )}
          aria-label={tr(lang, "Idioma", "Language")}
          onClick={() => {
            const idx = LANG_OPTIONS.findIndex(([code]) => code === lang);
            const next = LANG_OPTIONS[(idx + 1) % LANG_OPTIONS.length][0] as Lang;
            setLang(next);
          }}
        >
          <Globe size={12} strokeWidth={2} />
          <span>{LANG_SHORT[lang]}</span>
        </button>
      )}
      <div className="titlebar-ctrls">
        <button
          onClick={act((w) => w.minimize())}
          aria-label={tr(lang, "Minimizar", "Minimize")}
          title={tr(lang, "Minimizar", "Minimize")}
        >
          <Minus size={16} strokeWidth={1.5} />
        </button>
        <button
          onClick={act((w) => w.toggleMaximize())}
          aria-label={
            max
              ? tr(lang, "Restaurar", "Restore")
              : tr(lang, "Maximizar", "Maximize")
          }
          title={
            max
              ? tr(lang, "Restaurar", "Restore")
              : tr(lang, "Maximizar", "Maximize")
          }
        >
          {max ? <Copy size={13} strokeWidth={1.5} /> : <Square size={13} strokeWidth={1.5} />}
        </button>
        <button
          className="close"
          onClick={act((w) => w.close())}
          aria-label={tr(lang, "Fechar", "Close")}
          title={tr(lang, "Fechar", "Close")}
        >
          <X size={17} strokeWidth={1.5} />
        </button>
      </div>
    </header>
  );
}
