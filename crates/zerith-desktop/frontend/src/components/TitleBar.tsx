import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";
import { ZerithMark } from "./ui";
import { UpdatePill } from "./Update";

/** The window's own title bar: the native one is off (decorations: false) so
 * the bar can sit on the app's black. Empty space drags the window (Tauri's
 * data-tauri-drag-region), double click maximises. */
export function TitleBar({ title, brand = false }: { title?: string; brand?: boolean }) {
  const [max, setMax] = useState(false);
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
      <span className="titlebar-fill" data-tauri-drag-region />
      {/* boot screens (brand) have no app yet to update */}
      {brand ? null : <UpdatePill />}
      <div className="titlebar-ctrls">
        <button onClick={act((w) => w.minimize())} aria-label="Minimizar" title="Minimizar">
          <Minus size={16} strokeWidth={1.5} />
        </button>
        <button onClick={act((w) => w.toggleMaximize())} aria-label={max ? "Restaurar" : "Maximizar"} title={max ? "Restaurar" : "Maximizar"}>
          {max ? <Copy size={13} strokeWidth={1.5} /> : <Square size={13} strokeWidth={1.5} />}
        </button>
        <button className="close" onClick={act((w) => w.close())} aria-label="Fechar" title="Fechar">
          <X size={17} strokeWidth={1.5} />
        </button>
      </div>
    </header>
  );
}
