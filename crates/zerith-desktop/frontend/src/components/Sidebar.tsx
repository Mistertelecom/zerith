import { memo, useEffect, useMemo, useRef, useState } from "react";
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
  Plus,
  Search,
  Settings,
  SquarePen,
  Trash2,
  Workflow,
  X,
} from "lucide-react";
import type { AgentView, Session } from "../lib/api";
import { translateText, tr, useLang } from "../lib/i18n";
import { BotEmblem, emblemForAgent } from "./BotEmblem";
import type { AppView } from "./TitleBar";
import { Logo, ZerithMark } from "./ui";
import { useServerTrouble } from "./Servers";

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

function matchesBotSession(title: string, name: string): boolean {
  return (
    title === `@${name}` ||
    title.startsWith(`@${name} · `) ||
    title.startsWith(`@${name} `)
  );
}

function stripBotPrefix(title: string, name: string): string {
  const prefix = `@${name}`;
  if (title === prefix) return "";
  if (title.startsWith(`${prefix} · `)) return title.slice(prefix.length + 3).trim();
  if (title.startsWith(`${prefix} `)) return title.slice(prefix.length + 1).trim();
  return title.trim();
}

export const Sidebar = memo(function Sidebar(props: {
  sessions: Session[];
  selected: string | null;
  query: string;
  onQuery: (q: string) => void;
  onSelect: (id: string) => void;
  onNew: () => void;
  onSettings: () => void;
  onQuotas: () => void;
  onWorkflows?: () => void;
  view?: AppView;
  agents?: AgentView[];
  activeBot?: string | null;
  onSelectBot?: (bot: AgentView) => void;
  onSelectBotSession?: (id: string) => void;
  onCreateBot?: () => void;
  onEditBot?: (bot: AgentView) => void;
  onNewBotChat?: () => void;
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
  const { onNew, onNewBotChat, view } = props;
  const lang = useLang();
  const serverTrouble = useServerTrouble();
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
        (view === "bots" ? (onNewBotChat ?? onNew) : onNew)();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view, onNewBotChat, onNew]);

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

      <button
        className="btn side-new"
        onClick={props.view === "bots" ? (props.onNewBotChat ?? props.onNew) : props.onNew}
      >
        <SquarePen size={16} strokeWidth={1.75} />
        <span>
          {props.view === "bots"
            ? tr(lang, "Nova conversa com bot", "New bot chat")
            : tr(lang, "Nova conversa", "New chat")}
        </span>
        <span className="side-keys" aria-hidden>
          <kbd className="kbd">Ctrl</kbd>
          <kbd className="kbd">N</kbd>
        </span>
      </button>

      <label className="side-search">
        <Search size={14} strokeWidth={2} aria-hidden />
        <input
          value={props.query}
          placeholder={
            props.view === "bots"
              ? tr(lang, "Buscar bots", "Search bots")
              : tr(lang, "Buscar conversas", "Search chats")
          }
          aria-label={
            props.view === "bots"
              ? tr(lang, "Buscar bots", "Search bots")
              : tr(lang, "Buscar conversas", "Search chats")
          }
          onChange={(e) => props.onQuery(e.target.value)}
        />
      </label>

      {props.view === "bots" ? (
        <nav className="side-list" aria-label="Bots">
          <section className="side-group">
            <div className="bot-section-head">
              <h2 className="side-group-label">{tr(lang, "Bots Especialistas", "Specialist Bots")}</h2>
              {props.onCreateBot ? (
                <button
                  type="button"
                  className="bot-add-btn"
                  onClick={props.onCreateBot}
                  title={tr(lang, "Criar novo bot especialista", "Create new specialist bot")}
                >
                  <Plus size={12} strokeWidth={2.4} />
                  <span>{tr(lang, "Novo", "New")}</span>
                </button>
              ) : null}
            </div>
            {(props.agents ?? [])
              .filter((b) => !b.disabled)
              .filter((b) => {
                const q = props.query.trim().toLowerCase();
                return (
                  !q ||
                  b.name.toLowerCase().includes(q) ||
                  b.description.toLowerCase().includes(q)
                );
              })
              .map((bot) => {
                const botSessions = props.sessions.filter((s) =>
                  matchesBotSession(s.title, bot.name),
                );
                const liveStates = botSessions
                  .map((s) => props.status?.(s.id))
                  .filter(Boolean);
                const emblemStatus: "on" | "running" | "error" = liveStates.includes(
                  "waiting",
                )
                  ? "error"
                  : liveStates.includes("busy")
                    ? "running"
                    : "on";
                return (
                  <div
                    key={bot.name}
                    className={
                      "bot-roster-item" + (props.activeBot === bot.name ? " on" : "")
                    }
                  >
                    <button
                      type="button"
                      className={
                        "bot-roster-row" + (props.activeBot === bot.name ? " on" : "")
                      }
                      onClick={() => props.onSelectBot?.(bot)}
                      title={bot.description || bot.name}
                    >
                      <BotEmblem
                        icon={bot.icon || emblemForAgent(bot.name)}
                        size={34}
                        status={emblemStatus}
                      />
                      <div className="bot-roster-info">
                        <span className="bot-roster-name">
                          <span className="bot-roster-label">@{bot.name}</span>
                          {botSessions.length > 0 ? (
                            <span className="bot-roster-count">{botSessions.length}</span>
                          ) : null}
                        </span>
                        <span className="bot-roster-desc">
                          {bot.description || bot.layer}
                        </span>
                      </div>
                    </button>
                    {props.onEditBot ? (
                      <button
                        type="button"
                        className="icon-btn sm bot-roster-edit"
                        onClick={(e) => {
                          e.stopPropagation();
                          props.onEditBot?.(bot);
                        }}
                        title={tr(lang, "Editar bot", "Edit bot")}
                        aria-label={tr(lang, "Editar bot", "Edit bot")}
                      >
                        <Pencil size={12} strokeWidth={2} />
                      </button>
                    ) : null}
                  </div>
                );
              })}
          </section>
          {props.activeBot ? (() => {
            const activeBot = props.activeBot;
            const activeRows = props.sessions.filter((s) =>
              matchesBotSession(s.title, activeBot),
            );
            const midnight = new Date();
            midnight.setHours(0, 0, 0, 0);
            const midnightMs = midnight.getTime();
            return activeRows.length > 0 ? (
              <section className="side-group">
                <h2 className="side-group-label">
                  {tr(lang, `Histórico de @${activeBot}`, `@${activeBot} History`)}
                </h2>
                {activeRows.map((s) => {
                  const ts = s.updated_at < 1e12 ? s.updated_at * 1000 : s.updated_at;
                  const g = group(ts, midnightMs);
                  const cleanTitle =
                    stripBotPrefix(s.title, activeBot) || tr(lang, "Conversa", "Chat");
                  const state = props.status?.(s.id);
                  return (
                    <button
                      key={s.id}
                      type="button"
                      className={"list-row side-row" + (s.id === props.selected ? " on" : "")}
                      onClick={() => props.onSelectBotSession?.(s.id)}
                      title={s.title}
                    >
                      <MessageSquare size={14} strokeWidth={1.75} />
                      <span className="side-row-title">{cleanTitle}</span>
                      {state ? (
                        <span className={"side-row-state " + state}>
                          {state === "waiting" ? "!" : <Loader size={11} strokeWidth={2.5} />}
                        </span>
                      ) : null}
                      <span className="side-row-time">{when(ts, g)}</span>
                    </button>
                  );
                })}
              </section>
            ) : null;
          })() : null}
        </nav>
      ) : (
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
                        if (e.nativeEvent.isComposing || e.keyCode === 229) return;
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
      )}

      {menu ? (
        <div
          ref={menuRef}
          className="popover"
          role="menu"
          style={{
            position: "fixed",
            left: menu.x,
            top: menu.y,
            zIndex: 80,
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
              className="popup-row danger"
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
            {serverTrouble ? (
              <span className="side-state" data-testid="wf-trouble">
                <span
                  className="dot danger"
                  title={tr(lang, "Servidor fora do ar ou workflow remoto com falha", "Server down or remote workflow failing")}
                />
              </span>
            ) : null}
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
});
