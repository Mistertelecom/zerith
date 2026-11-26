import { memo, useEffect, useRef, useState } from "react";
import { Check, ChevronDown, ExternalLink, Folder, FolderOpen, GitBranch } from "lucide-react";

/** Last segment of a Windows or POSIX path. */
export const folderName = (p: string) =>
  p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;

/** Compute available vertical room above a composer pill and whether its
 * upward popover should right-align (`align-end`) to avoid viewport clipping. */
export function measurePillPopover(
  button: HTMLElement,
  wide = true,
): { room: number; alignEnd: boolean } {
  const rect = button.getBoundingClientRect();
  const pane = button.closest(".convo") ?? document.body;
  const room = Math.max(160, rect.top - pane.getBoundingClientRect().top - 96);
  const clampedW = Math.min(wide ? 420 : 260, window.innerWidth - 32);
  const alignEnd =
    rect.left + 420 > window.innerWidth - 16 &&
    (rect.right - clampedW >= 16 || rect.right > window.innerWidth - rect.left);
  return { room, alignEnd };
}

/** The conversation's folder: recent folders, the OS picker, and Explorer.
 * Same look as the composer's other pills (PillMenu classes). */
export const FolderPill = memo(function FolderPill(props: {
  /** Checked-out git branch of the folder, if it is a repository. */
  branch?: string | null;
  value: string;
  recent: string[];
  onPick: (path: string) => void;
  onBrowse: () => void;
  onReveal: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [room, setRoom] = useState<number | undefined>();
  const [alignEnd, setAlignEnd] = useState(false);
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
        onClick={(e) => {
          if (!open) {
            const layout = measurePillPopover(e.currentTarget, true);
            setRoom(layout.room);
            setAlignEnd(layout.alignEnd);
          }
          setOpen((o) => !o);
        }}
      >
        <Folder size={14} strokeWidth={2} />
        <span className="pill-text">{props.value ? folderName(props.value) : "Pasta"}</span>
        {props.branch ? (
          <span className="pill-branch" title={"Branch: " + props.branch}>
            <GitBranch size={12} strokeWidth={2} aria-hidden />
            {props.branch}
          </span>
        ) : null}
        <ChevronDown size={14} strokeWidth={2} className="pill-chev" />
      </button>
      {open ? (
        <div
          className={"popover pill-list wide" + (alignEnd ? " align-end" : "")}
          role="menu"
          aria-label="Pasta da conversa"
        >
          <div
            className="pill-options"
            style={
              room !== undefined
                ? { maxHeight: `min(360px, 55vh, ${room}px)` }
                : undefined
            }
          >
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
});
