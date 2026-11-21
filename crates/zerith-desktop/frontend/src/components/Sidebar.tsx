import { useEffect, useMemo, useRef, useState } from "react";
import {
  Archive,
  Brain,
  Check,
  Gauge,
  Loader,
  MessageSquare,
  Pencil,
  Pin,
  PinOff,
  Search,
  Settings,
  SquarePen,
  Trash2,
  Workflow,
  X,
} from "lucide-react";
import type { Session } from "../lib/api";
import { translateText, tr, useLang } from "../lib/i18n";
import { Logo, ZerithMark } from "./ui";

const DAY = 86_400_000;
const GROUPS_PT = ["Hoje", "Ontem", "Últimos 7 dias", "Mais antigas"] as const;

/** Which date group a session falls in, counted from local midnight. */
function group(ms: number, today: number): number {
  if (ms >= today) return 0;
  if (ms >= today - DAY) return 1;
  if (ms >= today - 7 * DAY) return 2;
  return 3;
}

function when(ms: number, g: number): string {
  const d = new Date(ms);
  if (g <= 1)
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
  if (g === 2)
    return d
      .toLocaleDateString(undefined, { weekday: "short" })
      .replace(".", "");
  return d.toLocaleDateString(undefined, { day: "2-digit", month: "2-digit" });
}

export function Sidebar(props: {
  sessions: Session[];
  selected: string | null;
  query: string;
  onQuery: (q: string) => void;
  onSelect: (id: string) => void;
  onNew: () => void;
  onSettings: () => void;
  onQuotas: () => void;
  onWorkflows?: () => void;
  memoryPaused: boolean;
  onToggleMemory: () => void;
  pinned?: string[];
  onTogglePin?: (id: string, pinned: boolean) => void;
  onRename?: (id: string, title: string) => void;
  onArchive?: (id: string) => void;
  onDelete?: (id: string) => void;
  /** Live turn of a conversation: `busy` or `waiting` (approval/question). */
  status?: (id: string) => "busy" | "waiting" | null;
}) {
  const { onNew } = props;
  const lang = useLang();
  const [menu, setMenu] = useState<{
    id: string;
    title: string;
    x: number;
    y: number;
  } | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; text: string } | null>(
    null,
  );
  const menuRef = useRef<HTMLDivElement>(null);

  // Close context menu on outside click or Escape
  useEffect(() => {
    if (!menu) return;
    const onDown = (e: MouseEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenu(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(null);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [menu]);

  // Ctrl/⌘ N opens a new conversation, as the button's key hint promises.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.shiftKey &&
        !e.altKey &&
        e.key.toLowerCase() === "n"
      ) {
        e.preventDefault();
        onNew();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onNew]);

  const pinnedSet = useMemo(() => new Set(props.pinned ?? []), [props.pinned]);

  const groups = useMemo(() => {
    const q = props.query.trim().toLowerCase();
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    const pinnedGroup = {
      label: translateText("Fixadas", lang),
      rows: [] as { s: Session; time: string; isPinned: boolean }[],
    };
    const labels = GROUPS_PT.map((l) => translateText(l, lang));
    const out = labels.map((label) => ({
      label,
      rows: [] as { s: Session; time: string; isPinned: boolean }[],
    }));
    for (const s of props.sessions) {
      if (q && !s.title.toLowerCase().includes(q)) continue;
      const ts = s.updated_at < 1e12 ? s.updated_at * 1000 : s.updated_at;
      const g = group(ts, midnight.getTime());
      const isPinned = pinnedSet.has(s.id);
      if (isPinned) {
        pinnedGroup.rows.push({ s, time: when(ts, g), isPinned: true });
      } else {
        out[g].rows.push({ s, time: when(ts, g), isPinned: false });
      }
    }
    if (props.pinned?.length) {
      pinnedGroup.rows.sort(
        (a, b) =>
          props.pinned!.indexOf(a.s.id) - props.pinned!.indexOf(b.s.id),
      );
    }
    return [pinnedGroup, ...out].filter((g) => g.rows.length > 0);
  }, [props.sessions, props.query, props.pinned, pinnedSet, lang]);

  const commitRename = () => {
    if (!renaming) return;
    const clean = renaming.text.trim();
    if (clean && props.onRename) {
      props.onRename(renaming.id, clean);
    }
    setRenaming(null);
  };

  return (
    <aside className="sidebar">
      <div className="side-brand">
        <ZerithMark size={22} />
        <span>Zerith</span>
      </div>

      <button className="btn side-new" onClick={props.onNew}>
        <SquarePen size={16} strokeWidth={1.75} />
        <span>{tr(lang, "Nova conversa", "New chat")}</span>
        <span className="side-keys" aria-hidden>
          <kbd className="kbd">Ctrl</kbd>
          <kbd className="kbd">N</kbd>
        </span>
      </button>

      <label className="side-search">
        <Search size={14} strokeWidth={2} aria-hidden />
        <input
          value={props.query}
          placeholder={tr(lang, "Buscar conversas", "Search chats")}
          aria-label={tr(lang, "Buscar conversas", "Search chats")}
          onChange={(e) => props.onQuery(e.target.value)}
        />
      </label>

      <nav className="side-list" aria-label={tr(lang, "Conversas", "Chats")}>
        {groups.length === 0 ? (
          <div className="side-empty">
            <MessageSquare size={18} strokeWidth={1.75} aria-hidden />
            <span>
              {props.query.trim()
                ? tr(lang, "Nada encontrado", "Nothing found")
                : tr(lang, "Nenhuma conversa ainda", "No chats yet")}
            </span>
          </div>
        ) : (
          groups.map((g) => (
            <section key={g.label} className="side-group">
              <h2 className="side-group-label">{g.label}</h2>
              {g.rows.map(({ s, time, isPinned }) =>
                renaming?.id === s.id ? (
                  <div
                    key={s.id}
                    className="list-row side-row on"
                    style={{ gap: 4, paddingRight: 6 }}
                  >
                    <Logo id={s.provider ?? s.model} size={16} />
                    <input
                      className="input"
                      style={{ height: 24, fontSize: 12, padding: "0 6px", flex: 1 }}
                      value={renaming.text}
                      autoFocus
                      onChange={(e) =>
                        setRenaming({ id: s.id, text: e.target.value })
                      }
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          commitRename();
                        } else if (e.key === "Escape") {
                          e.preventDefault();
                          setRenaming(null);
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="icon-btn sm"
                      onClick={commitRename}
                      title={translateText("Salvar", lang)}
                    >
                      <Check size={13} strokeWidth={2.25} />
                    </button>
                    <button
                      type="button"
                      className="icon-btn sm"
                      onClick={() => setRenaming(null)}
                      title={translateText("Cancelar", lang)}
                    >
                      <X size={13} strokeWidth={2.25} />
                    </button>
                  </div>
                ) : (
                  <button
                    key={s.id}
                    className={
                      "list-row side-row" + (s.id === props.selected ? " on" : "")
                    }
                    aria-current={s.id === props.selected ? "page" : undefined}
                    onClick={() => props.onSelect(s.id)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setMenu({
                        id: s.id,
                        title: s.title,
                        x: Math.min(e.clientX, window.innerWidth - 190),
                        y: Math.min(e.clientY, window.innerHeight - 170),
                      });
                    }}
                    title={
                      (s.workspace ? `${s.title}\n${s.workspace}` : s.title) +
                      ` (${tr(lang, "Clique direito para opções", "Right-click for options")})`
                    }
                  >
                    <Logo id={s.provider ?? s.model} size={16} />
                    <span className="side-row-title">{s.title}</span>
                    {isPinned ? (
                      <Pin
                        size={11}
                        strokeWidth={2}
                        style={{ opacity: 0.7, flexShrink: 0 }}
                      />
                    ) : null}
                    {(() => {
                      const state = props.status?.(s.id);
                      if (!state) return null;
                      const label =
                        state === "waiting"
                          ? tr(lang, "Esperando você", "Waiting for you")
                          : tr(lang, "Respondendo", "Responding");
                      return (
                        <span
                          className={"side-row-state " + state}
                          title={label}
                          aria-label={label}
                        >
                          {state === "waiting" ? (
                            "!"
                          ) : (
                            <Loader size={11} strokeWidth={2.5} />
                          )}
                        </span>
                      );
                    })()}
                    <span className="side-row-time">{time}</span>
                  </button>
                ),
              )}
            </section>
          ))
        )}
      </nav>

      {menu ? (
        <div
          ref={menuRef}
          className="popover"
          role="menu"
          style={{
            position: "fixed",
            left: menu.x,
            top: menu.y,
            zIndex: 9999,
            minWidth: 175,
            padding: 4,
            display: "flex",
            flexDirection: "column",
            gap: 2,
          }}
        >
          <button
            type="button"
            className="popup-row"
            onClick={() => {
              setRenaming({ id: menu.id, text: menu.title });
              setMenu(null);
            }}
          >
            <span className="popup-icon">
              <Pencil size={14} strokeWidth={2} />
            </span>
            <span>{translateText("Renomear", lang)}</span>
          </button>
          {props.onTogglePin ? (
            <button
              type="button"
              className="popup-row"
              onClick={() => {
                props.onTogglePin!(menu.id, !pinnedSet.has(menu.id));
                setMenu(null);
              }}
            >
              <span className="popup-icon">
                {pinnedSet.has(menu.id) ? (
                  <PinOff size={14} strokeWidth={2} />
                ) : (
                  <Pin size={14} strokeWidth={2} />
                )}
              </span>
              <span>
                {pinnedSet.has(menu.id)
                  ? translateText("Desafixar", lang)
                  : translateText("Fixar", lang)}
              </span>
            </button>
          ) : null}
          {props.onArchive ? (
            <button
              type="button"
              className="popup-row"
              onClick={() => {
                props.onArchive!(menu.id);
                setMenu(null);
              }}
            >
              <span className="popup-icon">
                <Archive size={14} strokeWidth={2} />
              </span>
              <span>{translateText("Arquivar", lang)}</span>
            </button>
          ) : null}
          {props.onDelete ? (
            <button
              type="button"
              className="popup-row"
              style={{ color: "var(--danger, #ff5c5c)" }}
              onClick={() => {
                props.onDelete!(menu.id);
                setMenu(null);
              }}
            >
              <span className="popup-icon">
                <Trash2 size={14} strokeWidth={2} />
              </span>
              <span>{translateText("Excluir", lang)}</span>
            </button>
          ) : null}
        </div>
      ) : null}

      <div className="side-foot">
        <button
          className="list-row side-foot-row"
          onClick={props.onToggleMemory}
          title={
            props.memoryPaused
              ? tr(lang, "Retomar memória", "Resume memory")
              : tr(lang, "Pausar memória", "Pause memory")
          }
        >
          <Brain size={16} strokeWidth={1.75} />
          <span className="side-row-title">{tr(lang, "Memória", "Memory")}</span>
          <span className="side-state">
            <span className={"dot" + (props.memoryPaused ? " warn" : " ok")} />
            {props.memoryPaused
              ? tr(lang, "pausada", "paused")
              : tr(lang, "ativa", "active")}
          </span>
        </button>
        {props.onWorkflows ? (
          <button className="list-row side-foot-row" onClick={props.onWorkflows}>
            <Workflow size={16} strokeWidth={1.75} />
            <span className="side-row-title">{tr(lang, "Automações", "Workflows")}</span>
          </button>
        ) : null}
        <button className="list-row side-foot-row" onClick={props.onQuotas}>
          <Gauge size={16} strokeWidth={1.75} />
          <span className="side-row-title">{tr(lang, "Cotas", "Quotas")}</span>
        </button>
        <button className="list-row side-foot-row" onClick={props.onSettings}>
          <Settings size={16} strokeWidth={1.75} />
          <span className="side-row-title">{tr(lang, "Ajustes", "Settings")}</span>
        </button>
      </div>
    </aside>
  );
}
