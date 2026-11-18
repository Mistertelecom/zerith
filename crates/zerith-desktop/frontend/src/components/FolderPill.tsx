import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown, ExternalLink, Folder, FolderOpen } from "lucide-react";

/** Last segment of a Windows or POSIX path. */
export const folderName = (p: string) =>
  p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;

/** The conversation's folder: recent folders, the OS picker, and Explorer.
 * Same look as the composer's other pills (PillMenu classes). */
export function FolderPill(props: {
  value: string;
  recent: string[];
  onPick: (path: string) => void;
  onBrowse: () => void;
  onReveal: () => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) =>
      !root.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) =>
      e.key === "Escape" && (e.stopPropagation(), setOpen(false));
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", esc, true);
    };
  }, [open]);
  const list = [props.value, ...props.recent.filter((r) => r !== props.value)].filter(Boolean);
  const close = (f: () => void) => () => (setOpen(false), f());
  return (
    <div className="pill-menu" ref={root}>
      <button
        type="button"
        className={"pill" + (open ? " open" : "")}
        title={props.value || "Pasta da conversa"}
        aria-label={"Pasta da conversa: " + (props.value || "nenhuma")}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <Folder size={14} strokeWidth={2} />
        <span className="pill-text">{props.value ? folderName(props.value) : "Pasta"}</span>
        <ChevronDown size={14} strokeWidth={2} className="pill-chev" />
      </button>
      {open ? (
        <div className="popover pill-list wide" role="menu" aria-label="Pasta da conversa">
          <div className="pill-options">
            {list.map((p) => (
              <button
                key={p}
                type="button"
                role="menuitemradio"
                aria-checked={p === props.value}
                className={"pill-option" + (p === props.value ? " on" : "")}
                title={p}
                onClick={close(() => props.onPick(p))}
              >
                <Folder size={16} strokeWidth={1.75} className="pill-option-icon" />
                <span className="pill-option-text">{folderName(p)}</span>
                <span className="pill-option-who">{p.replace(/[\\/][^\\/]+[\\/]?$/, "")}</span>
                {p === props.value ? <Check size={14} strokeWidth={2} /> : null}
              </button>
            ))}
          </div>
          <button type="button" role="menuitem" className="pill-foot" onClick={close(props.onBrowse)}>
            <FolderOpen size={14} strokeWidth={2} />
            Escolher pasta…
          </button>
          {props.value ? (
            <button type="button" role="menuitem" className="pill-foot" onClick={close(props.onReveal)}>
              <ExternalLink size={14} strokeWidth={2} />
              Abrir no Explorer
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
