import { useEffect, useMemo } from "react";
import {
  Brain,
  Gauge,
  Loader,
  MessageSquare,
  Search,
  Settings,
  SquarePen,
} from "lucide-react";
import type { Session } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { Logo, ZerithMark } from "./ui";

const DAY = 86_400_000;
const GROUPS_PT = ["Hoje", "Ontem", "Últimos 7 dias", "Mais antigas"] as const;
const GROUPS_EN = ["Today", "Yesterday", "Last 7 days", "Older"] as const;

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
  memoryPaused: boolean;
  onToggleMemory: () => void;
  /** Live turn of a conversation: `busy` or `waiting` (approval/question). */
  status?: (id: string) => "busy" | "waiting" | null;
}) {
  const { onNew } = props;
  const lang = useLang();

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

  const groups = useMemo(() => {
    const q = props.query.trim().toLowerCase();
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    const labels = lang === "en" ? GROUPS_EN : GROUPS_PT;
    const out = labels.map((label) => ({
      label,
      rows: [] as { s: Session; time: string }[],
    }));
    for (const s of props.sessions) {
      if (q && !s.title.toLowerCase().includes(q)) continue;
      // Milliseconds, like History.tsx: `* 1000` here put every conversation
      // in the year 58852, i.e. always "Hoje".
      const ts = s.updated_at < 1e12 ? s.updated_at * 1000 : s.updated_at;
      const g = group(ts, midnight.getTime());
      out[g].rows.push({ s, time: when(ts, g) });
    }
    return out.filter((g) => g.rows.length > 0);
  }, [props.sessions, props.query, lang]);

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
              {g.rows.map(({ s, time }) => (
                <button
                  key={s.id}
                  className={
                    "list-row side-row" + (s.id === props.selected ? " on" : "")
                  }
                  aria-current={s.id === props.selected ? "page" : undefined}
                  onClick={() => props.onSelect(s.id)}
                  title={s.workspace ? `${s.title}\n${s.workspace}` : s.title}
                >
                  <Logo id={s.provider ?? s.model} size={16} />
                  <span className="side-row-title">{s.title}</span>
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
                        {state === "waiting" ? "!" : <Loader size={11} strokeWidth={2.5} />}
                      </span>
                    );
                  })()}
                  <span className="side-row-time">{time}</span>
                </button>
              ))}
            </section>
          ))
        )}
      </nav>

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
