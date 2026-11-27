import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  Bot,
  CheckCircle2,
  CircleDashed,
  Download,
  Gauge,
  Import,
  ListChecks,
  Loader,
  PanelRight,
  Pencil,
  Play,
  Route,
  Search,
  Sparkles,
  Workflow as WorkflowIcon,
} from "lucide-react";
import {
  api,
  onBotWorkflowReport,
  onTurnDone,
  onTurnEvent,
  onTurnWake,
  onTurnLearned,
  onWorkflowsChanged,
  workflowsApi,
  type Account,
  type AgentView,
  type Boot,
  type Routing,
  type Session,
  type SessionUsageTotals,
  type Workflow,
} from "./lib/api";
import {
  applyEvent,
  attachUserMessageId,
  borrowIds,
  emptyTurn,
  finishTurn,
  toBlocks,
  tokenTotal,
  type Block,
  type TurnState,
} from "./lib/turn";
import { Sidebar } from "./components/Sidebar";
import { Transcript } from "./components/Transcript";
import { ApprovalCard, QuestionCard } from "./components/Cards";
import type { SettingsTab } from "./components/Settings";
import { Composer, type CommandName } from "./components/Composer";
import { Logo, ZerithMark } from "./components/ui";
import { catalogEntry, NOT_CHAT } from "./lib/catalog";
import { titleOf } from "./lib/title";
import { TitleBar, type AppView } from "./components/TitleBar";
import { BotEmblem, emblemForAgent } from "./components/BotEmblem";
import { BotModal } from "./components/BotModal";
import { SessionCost } from "./components/SessionCost";
import { AppVersion } from "./components/Update";
import { FolderPill } from "./components/FolderPill";
import { BackgroundJobs } from "./components/BackgroundJobs";
import { ActiveTerminals } from "./components/ActiveTerminals";
import { translateText, tr, useDomTranslator, useLang } from "./lib/i18n";
import { InlineBoundary } from "./components/InlineBoundary";

const Settings = lazy(() =>
  import("./components/Settings").then((m) => ({ default: m.Settings })),
);
const WorkflowsStudio = lazy(() =>
  import("./components/Workflows").then((m) => ({ default: m.WorkflowsStudio })),
);

const MODEL_KEY = "zerith.model";
const EXECUTOR_MODEL_KEY = "zerith.executorModel";
const ORCHESTRATOR_KEY = "zerith.orchestrator";
const EFFORT_KEY = "zerith.effort";
const MAX_CACHED_TURNS = 8;
// Stored rows read per page; "carregar anteriores" asks for one more page.
const HISTORY_PAGE = 400;

/** Bypass asks nothing at all: stepping into it takes an explicit yes. */
const confirmBypass = (lang: Parameters<typeof tr>[0]) =>
  window.confirm(
    tr(
      lang,
      "Ativar Bypass? O assistente vai rodar ferramentas sem pedir aprovação.",
      "Enable Bypass? The assistant will run tools without asking.",
    ),
  );

function estimateContextWindow(model: string): number {
  const m = model.toLowerCase();
  if (/gemini|hydrogen|argon/.test(m)) return 1_000_000;
  if (/claude|opus|sonnet|haiku|gpt-5|o3|o4/.test(m)) return 200_000;
  return 128_000;
}

// Models the user took out of the chat menu (`account:model` values).
const HIDDEN_KEY = "zerith.hiddenModels";

// Same four as Claude Code: `Bypass` is the one that asks nothing at all.
const MODES = [
  ["default", "Padrão"],
  ["acceptEdits", "Aceitar edições"],
  ["plan", "Plano"],
  ["bypass", "Bypass"],
] as const;

const num = (n: number) => n.toLocaleString("pt-BR");

export default function App() {
  const [boot, setBoot] = useState<Boot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
  // Primeiro uso: sem conversas e com um Hermes instalado, o início oferece
  // a migração (o plano abre em Ajustes › Histórico).
  const [hermesOffer, setHermesOffer] = useState(false);
  useEffect(() => {
    api
      .migrationOffer()
      .then(setHermesOffer)
      .catch(() => setHermesOffer(false));
  }, [sessions.length === 0]);
  const [pinnedSessions, setPinnedSessions] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Block[]>([]);
  // One live turn per conversation, keyed by session id: a chat that is not
  // open keeps its stream, its Stop button and its approval card. `turn` below
  // is the open one.
  const lang = useLang();
  useDomTranslator(lang);
  const [turns, setTurns] = useState<Record<string, TurnState>>({});
  const turnsRef = useRef<Record<string, TurnState>>(turns);
  turnsRef.current = turns;
  const sessionsRef = useRef<Session[]>(sessions);
  sessionsRef.current = sessions;
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;
  const sendingSessionsRef = useRef<Set<string>>(new Set());
  const sessionEpochRef = useRef<Record<string, number>>({});
  const bumpSessionEpoch = useCallback((id: string) => {
    const next = (sessionEpochRef.current[id] ?? 0) + 1;
    sessionEpochRef.current[id] = next;
    return next;
  }, []);
  const [sessionUsage, setSessionUsage] = useState<SessionUsageTotals | null>(
    null,
  );
  const [pendingUsage, setPendingUsage] = useState<Record<string, boolean>>({});
  const turnTouchedRef = useRef<Map<string, number>>(new Map());
  const pruneTurns = useCallback(
    (all: Record<string, TurnState>, keepId: string): Record<string, TurnState> => {
      turnTouchedRef.current.set(keepId, Date.now());
      const inactive = Object.entries(all).filter(
        ([k, v]) =>
          k !== selectedRef.current &&
          k !== keepId &&
          !v.busy &&
          v.approvals.length === 0 &&
          !v.question,
      );
      if (inactive.length <= MAX_CACHED_TURNS) return all;
      inactive.sort(
        (a, b) =>
          (turnTouchedRef.current.get(a[0]) ?? 0) -
          (turnTouchedRef.current.get(b[0]) ?? 0),
      );
      const next = { ...all };
      const drop = inactive.length - MAX_CACHED_TURNS;
      for (let i = 0; i < drop; i++) {
        delete next[inactive[i][0]];
        turnTouchedRef.current.delete(inactive[i][0]);
      }
      return next;
    },
    [],
  );
  const updateTurn = useCallback(
    (id: string, fn: (prev: TurnState) => TurnState) =>
      setTurns((all) =>
        pruneTurns({ ...all, [id]: fn(all[id] ?? emptyTurn) }, id),
      ),
    [pruneTurns],
  );
  const clearTurnBlocksIfIdle = useCallback((id: string) => {
    if (turnsRef.current[id]?.busy) return;
    setTurns((prev) => {
      const cur = prev[id];
      if (!cur || cur.busy || cur.blocks.length === 0) return prev;
      const kept = cur.blocks.filter(
        (b) => b.kind === "note" && (Boolean(b.learned?.length) || !!b.btw),
      );
      if (kept.length === cur.blocks.length) return prev;
      return { ...prev, [id]: { ...cur, blocks: kept } };
    });
  }, []);
  const turn: TurnState = (selected && turns[selected]) || emptyTurn;
  const setTurn = useCallback(
    (next: TurnState | ((prev: TurnState) => TurnState)) => {
      const id = selectedRef.current;
      if (!id) return;
      updateTurn(id, (prev) => (typeof next === "function" ? next(prev) : next));
    },
    [updateTurn],
  );
  // One draft per conversation ("__new__" for the one not created yet).
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const draft = drafts[selected ?? "__new__"] ?? "";
  const setDraft = useCallback(
    (v: string, key: string = selectedRef.current ?? "__new__") =>
      setDrafts((d) => (d[key] === v ? d : { ...d, [key]: v })),
    [],
  );
  const [query, setQuery] = useState("");
  // The last pick survives a restart; settings.toml's routing.main_model is the fallback.
  const [model, setModel] = useState(
    () => localStorage.getItem(MODEL_KEY) ?? "",
  );
  const [executorModel, setExecutorModel] = useState(
    () => localStorage.getItem(EXECUTOR_MODEL_KEY) ?? "",
  );
  const [orchestrator, setOrchestrator] = useState(
    () => localStorage.getItem(ORCHESTRATOR_KEY) === "1",
  );
  const toggleOrchestrator = useCallback(() => {
    setOrchestrator((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(ORCHESTRATOR_KEY, next ? "1" : "0");
      } catch {
        // ignore storage errors
      }
      return next;
    });
  }, []);
  const [accounts, setAccounts] = useState<Account[]>([]);
  // Live lists, for the accounts that publish one (Antigravity), by account id.
  const [liveModels, setLiveModels] = useState<
    Record<string, { id: string; name: string }[]>
  >({});
  const [hidden, setHidden] = useState<string[]>(() => {
    try {
      const parsed = JSON.parse(localStorage.getItem(HIDDEN_KEY) ?? "[]");
      return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
    } catch {
      return [];
    }
  });
  const toggleHidden = useCallback((v: string) => {
    setHidden((prev) => {
      const next = prev.includes(v)
        ? prev.filter((x) => x !== v)
        : [...prev, v];
      localStorage.setItem(HIDDEN_KEY, JSON.stringify(next));
      return next;
    });
  }, []);
  // Delegation profiles (combos with a subagent model), offered as models.
  const [profiles, setProfiles] = useState<string[]>([]);
  const loadProfiles = (r: Routing) =>
    setProfiles(
      (r.combos ?? [])
        .filter((c) => c.subagent_model != null && c.models.length)
        .map((c) => c.name),
    );
  const [mode, setMode] = useState<string>("default");
  const [effort, setEffortState] = useState<string>(
    () => localStorage.getItem(EFFORT_KEY) || "off",
  );
  const setEffort = useCallback((v: string) => {
    setEffortState(v);
    try {
      localStorage.setItem(EFFORT_KEY, v);
    } catch {
      // ignore storage errors
    }
  }, []);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [memoryPaused, setMemoryPaused] = useState(false);
  const [settings, setSettings] = useState<SettingsTab | null>(null);
  const [activeView, setActiveView] = useState<AppView>("chat");
  const [agents, setAgents] = useState<AgentView[]>([]);
  const [activeAgent, setActiveAgent] = useState<string | null>(null);
  const [botModal, setBotModal] = useState<{ open: boolean; editing: AgentView | null }>({
    open: false,
    editing: null,
  });
  const [workflowsList, setWorkflowsList] = useState<Workflow[]>([]);
  const [runningWfId, setRunningWfId] = useState<string | null>(null);
  const [status, setStatus] = useState("");
  // A status line is transient; an error wins over it and clears it.
  useEffect(() => {
    if (!status) return;
    const t = setTimeout(() => setStatus(""), 4000);
    return () => clearTimeout(t);
  }, [status]);
  useEffect(() => {
    if (error) setStatus("");
  }, [error]);
  // Sessions whose Stop the user pressed: their end is "Interrompido", not a failure.
  const cancelRequestedRef = useRef<Set<string>>(new Set());
  // User message row ids delivered directly on `done` events (skips history refetch on turn:done).
  const deliveredUserMsgIdRef = useRef<Map<string, number>>(new Map());
  const workflowsDirtyRef = useRef(false);
  // History of the open conversation: loading, failed (with retry), and how
  // many rows were asked for (`more`: the store may hold older ones).
  const [hist, setHist] = useState<{
    sid: string;
    state: "loading" | "ok" | "error";
    error?: string;
    limit: number;
    more: boolean;
  } | null>(null);
  const histLimitRef = useRef(HISTORY_PAGE);
  const loadSeqRef = useRef(0);
  const [inspector, setInspector] = useState(true);
  // Folders used before (newest first, from the store) and the one picked for
  // a conversation that does not exist yet.
  const [recent, setRecent] = useState<string[]>([]);
  const [newFolder, setNewFolder] = useState<string | null>(null);
  const current = sessions.find((s) => s.id === selected) ?? null;
  // The open conversation's folder; a new one starts in the last used.
  const folder = (selected ? current?.workspace : newFolder) ?? recent[0] ?? "";
  const [branch, setBranch] = useState<string | null>(null);
  // Re-read when the folder changes and after each answer (the agent may switch branches).
  useEffect(() => {
    if (!folder) return setBranch(null);
    let live = true;
    void api
      .gitBranch(folder)
      .then((b) => live && setBranch(b))
      .catch(() => live && setBranch(null));
    return () => {
      live = false;
    };
  }, [folder, turn.busy]);
  // A change applies to the next turn, even mid-conversation.
  const pickFolder = useCallback(
    (path: string) => {
      setRecent((r) => [path, ...r.filter((x) => x !== path)]);
      if (!selected) return setNewFolder(path);
      api
        .setSessionWorkspace(selected, path)
        .then(() =>
          setSessions((list) =>
            list.map((s) => (s.id === selected ? { ...s, workspace: path } : s)),
          ),
        )
        .catch((e) => setStatus(`Não consegui trocar a pasta: ${e}`));
    },
    [selected],
  );
  const draftRef = useRef<HTMLTextAreaElement>(null);
  const creatingSessionRef = useRef(false);
  const fetchFiles = useCallback(
    () => api.workspaceFiles(folder || undefined),
    [folder],
  );

  // First frame: one call decides everything the window needs.
  useEffect(() => {
    api
      .boot()
      .then((b) => {
        setBoot(b);
        setSessions(b.sessions);
        setMemoryPaused(b.memory_paused);
        void api
          .pinnedSessions()
          .then(setPinnedSessions)
          .catch(() => undefined);
        void api
          .recentWorkspaces()
          .then(setRecent)
          .catch(() => undefined);
        void api
          .routing()
          .then((r) => (loadProfiles(r), r))
          .then((r) => {
            setModel(
              (prev) =>
                prev || r.main_model || b.providers[0]?.default_model || "",
            );
            setExecutorModel(
              (prev) =>
                prev ||
                r.subagent_model ||
                r.main_model ||
                b.providers[0]?.default_model ||
                "",
            );
          })
          .catch(() => {
            setModel((prev) => prev || b.providers[0]?.default_model || "");
            setExecutorModel(
              (prev) => prev || b.providers[0]?.default_model || "",
            );
          });
        void api
          .accounts()
          .then(setAccounts)
          .catch(() => undefined);
        void api
          .agentsList()
          .then(setAgents)
          .catch(() => undefined);
        void workflowsApi
          .list()
          .then(setWorkflowsList)
          .catch(() => undefined);
        document.documentElement.dataset.theme =
          b.theme === "light"
            ? "light"
            : b.theme === "dark"
              ? "dark"
              : window.matchMedia("(prefers-color-scheme: light)").matches
                ? "light"
                : "dark";
        document.documentElement.dataset.motion = b.reduce_motion
          ? "reduced"
          : "full";
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (model) localStorage.setItem(MODEL_KEY, model);
  }, [model]);

  useEffect(() => {
    if (executorModel) localStorage.setItem(EXECUTOR_MODEL_KEY, executorModel);
  }, [executorModel]);

  // The saved model left its account's live list: move to the account's
  // first model instead of failing every turn with a 404.
  useEffect(() => {
    const cut = model.indexOf(":");
    const live = cut > 0 ? liveModels[model.slice(0, cut)] : undefined;
    const id = model.slice(cut + 1);
    if (!live?.length || live.some((m) => m.id === id)) return;
    const next = live.find((m) => !NOT_CHAT.test(m.id));
    if (!next) return;
    setModel(model.slice(0, cut + 1) + next.id);
    setStatus(`${id} não é mais oferecido pela conta; usando ${next.id}`);
  }, [model, liveModels]);

  // OAuth accounts that publish their models (Antigravity's Gemini 4 Hydrogen
  // is not in any static catalogue); a failure keeps the catalogue list.
  const fetchedLiveModels = useRef<Set<string>>(new Set());
  useEffect(() => {
    for (const a of accounts) {
      if (a.enabled && a.auth === "oauth" && !fetchedLiveModels.current.has(a.id)) {
        fetchedLiveModels.current.add(a.id);
        api
          .accountModels(a.id)
          .then(
            (list) =>
              list.length && setLiveModels((m) => ({ ...m, [a.id]: list })),
          )
          .catch(() => {
            fetchedLiveModels.current.delete(a.id);
          });
      }
    }
  }, [accounts]);

  const fresh = useRef<string | null>(null);
  // Reads the stored rows of `sid`. While its turn still runs, the rows of
  // that turn (after the last prompt) are left out: the live blocks draw them.
  const loadHistory = useCallback(
    (sid: string, limit: number, tries = 3): void => {
      const seq = ++loadSeqRef.current;
      const epoch = bumpSessionEpoch(sid);
      histLimitRef.current = limit;
      setHist((h) => ({
        sid,
        state: "loading",
        limit,
        more: h?.sid === sid ? h.more : false,
      }));
      api
        .sessionMessages(sid, limit)
        .then((list) => {
          if (seq !== loadSeqRef.current || selectedRef.current !== sid) return;
          // A send or a turn end raced this read: read again.
          if (epoch !== sessionEpochRef.current[sid]) {
            if (tries > 1) loadHistory(sid, limit, tries - 1);
            return;
          }
          const busy = turnsRef.current[sid]?.busy;
          let rows = list;
          if (busy) {
            let k = list.length - 1;
            while (k >= 0 && list[k].role !== "user") k--;
            rows = list.slice(0, k + 1);
          }
          const blocks = toBlocks(rows);
          setMessages((prev) => {
            // A prompt sent while this read ran may not be stored yet: keep it.
            const sent = prev[prev.length - 1];
            return busy && sent?.kind === "user" && sent.id == null &&
              !rows.some((m) => m.role === "user" && m.content === sent.text)
              ? [...blocks, sent]
              : blocks;
          });
          if (!busy) clearTurnBlocksIfIdle(sid);
          setHist({ sid, state: "ok", limit, more: list.length >= limit });
        })
        .catch((e) => {
          if (seq !== loadSeqRef.current || selectedRef.current !== sid) return;
          setMessages([]);
          setHist({ sid, state: "error", error: String(e), limit, more: false });
        });
    },
    [bumpSessionEpoch, clearTurnBlocksIfIdle],
  );
  // Transcript of the conversation that is open.
  useEffect(() => {
    if (!selected) {
      setMessages([]);
      setTurn(emptyTurn);
      setHist(null);
      return;
    }
    // A conversation `send` just created already holds its first message and
    // turn; reading the (still empty) store here would wipe both.
    if (fresh.current === selected) {
      fresh.current = null;
      setHist(null);
      return;
    }
    // Never show the previous chat's rows under this one while it loads.
    setMessages([]);
    loadHistory(selected, HISTORY_PAGE);
  }, [selected, setTurn, loadHistory]);

  useEffect(() => {
    let cancelled = false;
    let u1: (() => void) | undefined;
    let u2: (() => void) | undefined;
    void onWorkflowsChanged(() => {
      if (cancelled) return;
      void workflowsApi
        .list()
        .then((list) => {
          if (!cancelled) setWorkflowsList(list);
        })
        .catch(() => undefined);
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        u1 = fn;
      }
    });
    void onBotWorkflowReport((report) => {
      if (cancelled) return;
      setStatus(`Workflow "${report.workflowName}" concluído (${report.status})`);
      void api
        .boot()
        .then((b) => {
          if (!cancelled) setSessions(b.sessions);
        })
        .catch(() => undefined);
      if (selectedRef.current === report.sessionId) {
        loadHistory(report.sessionId, histLimitRef.current);
      }
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        u2 = fn;
      }
    });
    return () => {
      cancelled = true;
      u1?.();
      u2?.();
    };
  }, [loadHistory]);

  useEffect(() => {
    if (!selected) {
      setSessionUsage(null);
      return;
    }
    turnTouchedRef.current.set(selected, Date.now());
    setSessionUsage(null);
    let live = true;
    api
      .sessionUsage(selected)
      .then((u) => live && setSessionUsage(u))
      .catch(() => live && setSessionUsage(null));
    return () => {
      live = false;
    };
  }, [selected]);

  const pendingDeltasRef = useRef<Map<string, { text: string; reasoning: string }>>(new Map());
  const rafRef = useRef<number | null>(null);

  const flushDeltas = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    const pending = pendingDeltasRef.current;
    if (pending.size === 0) return;
    const snapshot = Array.from(pending.entries());
    pending.clear();
    setTurns((all) => {
      let next = { ...all };
      let lastId = "";
      for (const [sid, buf] of snapshot) {
        let s = next[sid] ?? emptyTurn;
        if (buf.reasoning) s = applyEvent(s, { type: "reasoning", text: buf.reasoning });
        if (buf.text) s = applyEvent(s, { type: "delta", text: buf.text });
        next[sid] = s;
        turnTouchedRef.current.set(sid, Date.now());
        lastId = sid;
      }
      return lastId ? pruneTurns(next, lastId) : next;
    });
  }, [pruneTurns]);

  const cleanupSessionState = useCallback(
    (id: string) => {
      void api.cancelTurn(id).catch(() => undefined);
      pendingDeltasRef.current.delete(id);
      turnTouchedRef.current.delete(id);
      deliveredUserMsgIdRef.current.delete(id);
      bumpSessionEpoch(id);
      setTurns((prev) => {
        if (!(id in prev)) return prev;
        const next = { ...prev };
        delete next[id];
        return next;
      });
    },
    [bumpSessionEpoch],
  );

  // Turn events, for every conversation at once. The state of a chat that is
  // not open is kept here, so switching away and back shows the same stream,
  // the Stop button and any approval that arrived while away.
  useEffect(() => {
    const ev = onTurnEvent((id, event) => {
      if (event.type === "delta" || event.type === "reasoning") {
        const cur = pendingDeltasRef.current.get(id) ?? { text: "", reasoning: "" };
        if (event.type === "delta") cur.text += event.text;
        else cur.reasoning += event.text;
        pendingDeltasRef.current.set(id, cur);
        if (rafRef.current === null) {
          rafRef.current = requestAnimationFrame(() => {
            rafRef.current = null;
            flushDeltas();
          });
        }
        return;
      }
      flushDeltas();
      // An approved plan switches mode in the backend; keep the picker in step
      // so the next turn is not sent back into plan mode.
      if (event.type === "modeChanged") {
        if (id === selectedRef.current) setMode(event.mode);
        return;
      }
      if (event.type === "route") {
        setSessions((list) =>
          list.map((s) =>
            s.id === id ? { ...s, provider: event.provider, model: event.model } : s,
          ),
        );
      }
      if (event.type === "done" && event.userMessageId != null) {
        deliveredUserMsgIdRef.current.set(id, event.userMessageId);
        if (id === selectedRef.current) {
          const userMsgId = event.userMessageId;
          setMessages((prev) => attachUserMessageId(prev, userMsgId));
        }
      }
      updateTurn(id, (prev) => applyEvent(prev, event));
    });
    const done = onTurnDone((id) => {
      flushDeltas();
      const epoch = bumpSessionEpoch(id);
      const isSelected = id === selectedRef.current;
      if (isSelected) {
        setPendingUsage((m) => ({ ...m, [id]: true }));
      }
      const cancelled = cancelRequestedRef.current.delete(id);
      const deliveredUserId = deliveredUserMsgIdRef.current.get(id);
      deliveredUserMsgIdRef.current.delete(id);
      updateTurn(id, (prev) => finishTurn(prev, cancelled));
      // The open conversation gets its stored rows back (with their ids, which
      // "Editar" needs); a closed one rereads them when it is opened.
      if (!isSelected) return;
      if (deliveredUserId != null) {
        setMessages((prev) => attachUserMessageId(prev, deliveredUserId));
      } else {
        void api
          .sessionMessages(id)
          .then((list) => {
            if (id !== selectedRef.current) return;
            if (
              epoch !== sessionEpochRef.current[id] ||
              turnsRef.current[id]?.busy
            ) {
              return;
            }
            setMessages((prev) => {
              const stored = [...(turnsRef.current[id]?.blocks ?? [])]
                .reverse()
                .find((b) => b.kind === "assistant")?.stored;
              return borrowIds(prev, list, stored);
            });
          })
          .catch(() => undefined);
      }
      void api
        .sessionUsage(id)
        .then((u) => {
          if (id === selectedRef.current) setSessionUsage(u);
        })
        .catch(() => undefined)
        .finally(() => {
          setPendingUsage((m) => {
            if (!m[id]) return m;
            const n = { ...m };
            delete n[id];
            return n;
          });
        });
    });
    const learned = onTurnLearned((id, items) => {
      if (!items.length) return;
      if (!sessionsRef.current.some((s) => s.id === id)) return;
      const text =
        "Aprendi: " +
        items
          .map((i) => (i.kind === "skill" ? `skill ${i.name}` : i.text))
          .join(" · ");
      const noteBlock: Block = { kind: "note", text, learned: items };
      if (turnsRef.current[id]?.busy) {
        if (id === selectedRef.current) {
          setMessages((prev) => {
            let insertAt = prev.length;
            while (
              insertAt > 0 &&
              prev[insertAt - 1].kind === "user" &&
              (prev[insertAt - 1] as Extract<Block, { kind: "user" }>).id ==
                null
            ) {
              insertAt--;
            }
            return [...prev.slice(0, insertAt), noteBlock, ...prev.slice(insertAt)];
          });
        }
        return;
      }
      updateTurn(id, (prev) =>
        prev.busy ? prev : { ...prev, blocks: [...prev.blocks, noteBlock] },
      );
    });
    // A monitor woke the conversation: show the notice and mark it busy, the
    // same way a sent message does, so the stream that follows has a home.
    const wake = onTurnWake((id, text) => {
      flushDeltas();
      deliveredUserMsgIdRef.current.delete(id);
      bumpSessionEpoch(id);
      if (id === selectedRef.current) {
        const prevBlocks = turnsRef.current[id]?.blocks ?? [];
        const done = prevBlocks.map((b) =>
          b.kind === "assistant" ? { ...b, streaming: false } : b,
        );
        // "[Monitor job-3: deploy]\nDEPLOY OK..." -> "Monitor (deploy): DEPLOY OK..."
        const lines = text
          .split("\n")
          .filter((l) => l.trim() && !l.startsWith("[aviso automático"));
        const head = /^\[Monitor [^:]+: (.+)\]/.exec(lines[0] ?? "")?.[1] ?? "monitor";
        const body = lines.find((l) => !l.startsWith("[Monitor ")) ?? lines[0] ?? "";
        setMessages((prev) => [
          ...prev,
          ...done,
          { kind: "note", text: `Monitor (${head}): ${body}` },
        ]);
      }
      const busyTurn: TurnState = { ...emptyTurn, busy: true };
      turnsRef.current = { ...turnsRef.current, [id]: busyTurn };
      updateTurn(id, () => busyTurn);
    });
    return () => {
      flushDeltas();
      void ev.then((u) => u());
      void done.then((u) => u());
      void learned.then((u) => u());
      void wake.then((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [updateTurn, flushDeltas, bumpSessionEpoch]);

  const send = useCallback(async (overridePrompt?: string) => {
    const typed = (overridePrompt ?? draft).trim();
    // `/goal <objetivo>`: o objetivo vira o prompt e o backend roda o laço.
    const goalMatch = /^\/goal(?:\s+([\s\S]*))?$/.exec(typed);
    const prompt = goalMatch ? (goalMatch[1] ?? "").trim() : typed;
    if (goalMatch && !prompt) {
      setStatus("Uso: /goal <objetivo>");
      return;
    }
    // /btw <pergunta>: à parte, mesmo com o turno rodando. O cartão vive só
    // na tela: não é mensagem da conversa nem volta ao modelo.
    const btwMatch = /^\/btw(?:\s+([\s\S]*))?$/.exec(typed);
    if (btwMatch) {
      const question = (btwMatch[1] ?? "").trim();
      const sid = selectedRef.current;
      if (!question) return setStatus("Uso: /btw <pergunta>");
      if (!sid) return setStatus("/btw precisa de uma conversa aberta");
      if (!model) return setError("Escolha um modelo antes de perguntar");
      if (!overridePrompt) setDraft("");
      const btwId = Date.now();
      // Nasce nos blocos do turno; um novo envio pode tê-lo levado para a
      // conversa, então a resposta troca o cartão onde ele estiver.
      const mine = (x: Block) => x.kind === "note" && x.btw?.id === btwId;
      const swap = (list: Block[], b: Block) =>
        list.some(mine) ? list.map((x) => (mine(x) ? b : x)) : list;
      const put = (b: Block, first = false) => {
        updateTurn(sid, (prev) => ({
          ...prev,
          blocks: first ? [...prev.blocks, b] : swap(prev.blocks, b),
        }));
        if (!first && selectedRef.current === sid) setMessages((prev) => swap(prev, b));
      };
      const card = (extra: Partial<NonNullable<Extract<Block, { kind: "note" }>["btw"]>>): Block => ({
        kind: "note",
        text: question,
        btw: { id: btwId, question, ...extra },
      });
      put(card({}), true);
      api
        .btwAsk(sid, model, question, activeAgent)
        .then((a) => put(card({ answer: a.text || "(sem resposta)", model: a.model })))
        .catch((e) => put(card({ error: String(e) })));
      return;
    }
    if (!prompt) return;
    const curSelected = selectedRef.current;
    const sendKey = curSelected ?? "__new__";
    if (sendingSessionsRef.current.has(sendKey)) return;
    sendingSessionsRef.current.add(sendKey);
    let extraSendKey: string | null = null;
    let createdLock = false;
    let markedBusy = false;
    let id = curSelected;
    try {
      // Intercept /workflows and /run <id|name> slash shortcuts cleanly without an LLM round-trip.
      if (prompt === "/workflows") {
        if (!overridePrompt) setDraft("");
        const sid = curSelected;
        const wfs = workflowsList.length
          ? workflowsList
          : await workflowsApi.list().catch(() => []);
        if (sid === null) {
          setStatus(
            wfs.length
              ? `Workflows (${wfs.length}): ${wfs.map((w) => w.name).join(", ")}`
              : "Nenhum workflow cadastrado.",
          );
          return;
        }
        if (selectedRef.current !== sid) return;
        setMessages((prev) => [
          ...prev,
          {
            kind: "note",
            text: wfs.length
              ? "Workflows disponíveis (/run <nome>):\n" +
                wfs.map((w) => `• ${w.name} (${w.id})`).join("\n")
              : "Nenhum workflow cadastrado.",
          },
        ]);
        return;
      }
      if (prompt.startsWith("/run ")) {
        const q = prompt.slice(5).trim().toLowerCase();
        if (!q) return;
        if (!overridePrompt) setDraft("");
        const sid = curSelected;
        const wfs = workflowsList.length
          ? workflowsList
          : await workflowsApi.list().catch(() => []);
        const match =
          wfs.find((w) => w.id.toLowerCase() === q) ??
          wfs.find((w) => w.name.toLowerCase().includes(q));
        if (!match) {
          setStatus(`Workflow não encontrado para "/run ${q}"`);
          return;
        }
        setRunningWfId(match.id);
        setStatus(`Executando "${match.name}"…`);
        try {
          const run = await workflowsApi.runNow(match.id);
          if (sid === null) {
            const summary = run.summary ? ` — ${run.summary.slice(0, 120)}` : "";
            setStatus(`"${match.name}" finalizado (${run.status})${summary}`);
            return;
          }
          setStatus(`"${match.name}" finalizado (${run.status})`);
          if (selectedRef.current !== sid) return;
          setMessages((prev) => [
            ...prev,
            {
              kind: "note",
              text: `⚡ ${match.name} (${run.status}):\n${run.summary || "Concluído sem saída textual."}`,
            },
          ]);
        } catch (e) {
          setStatus(`Erro ao executar "${match.name}": ${e}`);
        } finally {
          setRunningWfId(null);
        }
        return;
      }
      // Enter while the answer streams: the backend would refuse the second
      // turn and the transcript would split. Keep the draft and say why.
      if (id && (turnsRef.current[id]?.busy ?? false)) {
        setStatus("Espere a resposta terminar (ou pare) para enviar outra");
        return;
      }
      if (!model) {
        setError("Escolha um modelo antes de enviar");
        return;
      }
      if (!id) {
        if (creatingSessionRef.current) return;
        creatingSessionRef.current = true;
        createdLock = true;
      }
      setError(null);
      if (!overridePrompt) setDraft("");
      if (!id) {
        const sessionTitle = activeAgent
          ? `@${activeAgent} · ${titleOf(prompt)}`
          : titleOf(prompt);
        const created = await api.createSession(sessionTitle, folder);
        sessionsRef.current = [created, ...sessionsRef.current];
        setSessions((prev) => [created, ...prev]);
        id = created.id;
        if (sendingSessionsRef.current.has(id)) return;
        sendingSessionsRef.current.add(id);
        extraSendKey = id;
        fresh.current = id;
        selectedRef.current = id;
        setSelected(id);
        setMessages([]);
        // The composer remounts for the new conversation: keep the caret there.
        requestAnimationFrame(() => draftRef.current?.focus());
        if (activeAgent) {
          try {
            const raw = localStorage.getItem("zerith.botSessions");
            const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
            map[activeAgent] = created.id;
            localStorage.setItem("zerith.botSessions", JSON.stringify(map));
          } catch {
            // ignore storage errors
          }
        }
      }
      // The previous answer lives only in `turn` until the session is reloaded:
      // keep it in the transcript before the new turn resets that state.
      const prevBlocks = curSelected
        ? (turnsRef.current[curSelected]?.blocks ?? [])
        : [];
      const done = prevBlocks.map((b) =>
        b.kind === "assistant" ? { ...b, streaming: false } : b,
      );
      setMessages((prev) => [
        ...prev,
        ...done,
        { kind: "user", text: typed },
      ]);
      bumpSessionEpoch(id);
      cancelRequestedRef.current.delete(id);
      deliveredUserMsgIdRef.current.delete(id);
      markedBusy = true;
      const busyTurn: TurnState = { ...emptyTurn, busy: true };
      turnsRef.current = { ...turnsRef.current, [id]: busyTurn };
      updateTurn(id, () => busyTurn);
      // `history: 0` = the execution profile's window, decided by the core.
      await api.sendTurn({
        sessionId: id,
        model,
        prompt,
        mode,
        agent: activeAgent,
        reasoningEffort: effort === "off" ? null : effort,
        orchestrator,
        executorModel: orchestrator ? executorModel || model : null,
        history: 0,
        goal: !!goalMatch,
      });
    } catch (e) {
      setError(String(e));
      if (markedBusy && id && !String(e).includes("já está respondendo")) {
        turnsRef.current = {
          ...turnsRef.current,
          [id]: { ...(turnsRef.current[id] ?? emptyTurn), busy: false },
        };
        updateTurn(id, (prev) => ({ ...prev, busy: false }));
      }
    } finally {
      sendingSessionsRef.current.delete(sendKey);
      if (extraSendKey) sendingSessionsRef.current.delete(extraSendKey);
      if (createdLock) creatingSessionRef.current = false;
    }
  }, [
    draft,
    model,
    executorModel,
    orchestrator,
    mode,
    effort,
    activeAgent,
    workflowsList,
    setError,
    folder,
    updateTurn,
    bumpSessionEpoch,
    setDraft,
  ]);

  const steer = useCallback(
    async (text: string) => {
      if (!selected || !text.trim()) return;
      const trimmed = text.trim();
      setDraft("");
      try {
        const ok = await api.steerTurn(selected, trimmed);
        if (!ok) {
          setDraft(trimmed);
          setStatus("O turno já encerrou; envie novamente como nova mensagem");
          return;
        }
        setStatus("Instrução enviada ao turno em andamento");
      } catch (e) {
        setDraft(trimmed);
        setError(String(e));
      }
    },
    [selected, setDraft],
  );

  // "Editar" / "Ramificar daqui": a copy of the conversation up to `upto`
  // (same folder), opened in place of this one, which stays untouched. An
  // edit copies up to just before the prompt and puts it back in the
  // composer, to be changed and sent as a normal turn.
  const fork = useCallback(
    (upto: number, suffix: string, text?: string) => {
      if (!selected) return;
      api
        .forkSession(selected, upto, `${current?.title ?? "Conversa"} (${suffix})`)
        .then((s) => {
          setSessions((prev) => [s, ...prev]);
          setSelected(s.id);
          if (text == null) return;
          setDraft(text, s.id);
          requestAnimationFrame(() =>
            document.querySelector<HTMLTextAreaElement>(".composer textarea")?.focus(),
          );
        })
        .catch((e) => setStatus(`Não consegui copiar a conversa: ${e}`));
    },
    [selected, current, setDraft],
  );

  // A provider saved in Ajustes changes the models the composer can offer, so
  // the boot payload is re-read when the panel closes.
  // Appearance and profile live in the settings file, so every change is
  // written there and shown at once; the boot payload keeps the last word.
  const setTheme = useCallback((t: string) => {
    setBoot((b) => (b ? { ...b, theme: t as Boot["theme"] } : b));
    document.documentElement.dataset.theme =
      t === "system"
        ? window.matchMedia("(prefers-color-scheme: light)").matches
          ? "light"
          : "dark"
        : t;
    void api.savePrefs({ theme: t as Boot["theme"] });
  }, []);

  const setProfile = useCallback((p: string) => {
    setBoot((b) => (b ? { ...b, profile: p as Boot["profile"] } : b));
    void api.savePrefs({ profile: p as Boot["profile"] });
  }, []);

  const setReduceMotion = useCallback((v: boolean) => {
    setBoot((b) => (b ? { ...b, reduce_motion: v } : b));
    document.documentElement.dataset.motion = v ? "reduced" : "full";
    void api.savePrefs({ reduce_motion: v });
  }, []);

  // A note in the transcript, for what a command reports.
  const note = useCallback((text: string) => {
    setMessages((prev) => [...prev, { kind: "note", text }]);
  }, []);

  const runCommand = useCallback(
    (name: CommandName) => {
      setStatus("");
      if (name === "compact") {
        if (!selected) return setStatus("Nada para compactar ainda");
        if (turnsRef.current[selected]?.busy)
          return setStatus("Espere o turno terminar para compactar");
        if (!model) return setStatus("Escolha um modelo antes de compactar");
        const sid = selected;
        setStatus("compactando…");
        api
          .compact(sid, model)
          .then((n) => {
            const msg =
              n === 0
                ? "Nada para compactar ainda"
                : `Contexto compactado: ${n} mensagens resumidas`;
            if (selectedRef.current !== sid) {
              setStatus(msg);
              return;
            }
            setStatus("");
            note(msg);
          })
          .catch((e) => setStatus(`Não consegui compactar: ${e}`));
        return;
      }
      if (name === "context") {
        const total = tokenTotal(
          (selected ? turnsRef.current[selected]?.usage : null) ?? null,
        );
        if (!total) return setStatus("Contexto: nenhum turno ainda");
        setStatus(`Contexto: ${total.toLocaleString()} tokens neste turno`);
        return;
      }
      if (name === "rewind") {
        if (!selected) return setStatus("Nada para rebobinar");
        if (turnsRef.current[selected]?.busy)
          return setStatus("Espere o turno terminar para rebobinar");
        const sid = selected;
        const epoch = bumpSessionEpoch(sid);
        api
          .checkpoints(sid)
          .then((cps) => {
            if (
              selectedRef.current !== sid ||
              epoch !== sessionEpochRef.current[sid] ||
              turnsRef.current[sid]?.busy
            ) {
              return;
            }
            if (!cps.length)
              return setStatus("Nenhum ponto de restauração nesta conversa");
            if (
              !window.confirm(
                tr(
                  lang,
                  "Rebobinar para o último ponto de restauração? Arquivos e mensagens depois dele serão desfeitos.",
                  "Rewind to the last checkpoint? Files and messages after it will be undone.",
                ),
              )
            )
              return;
            return api.rewind(sid).then((r) => {
              if (
                selectedRef.current !== sid ||
                epoch !== sessionEpochRef.current[sid] ||
                turnsRef.current[sid]?.busy
              ) {
                return;
              }
              setTurns((prev) => {
                const cur = prev[sid];
                if (!cur) return prev;
                return {
                  ...prev,
                  [sid]: {
                    ...cur,
                    blocks: [],
                    busy: false,
                    approvals: [],
                    question: null,
                  },
                };
              });
              return api.sessionMessages(sid, histLimitRef.current).then((list) => {
                if (
                  selectedRef.current !== sid ||
                  epoch !== sessionEpochRef.current[sid] ||
                  turnsRef.current[sid]?.busy
                ) {
                  return;
                }
                setMessages([
                  ...toBlocks(list),
                  { kind: "note", text: `${r.text || "Rebobinado"} (turno ${r.turn})` },
                ]);
              });
            });
          })
          .catch((e) => setStatus(`Não consegui rebobinar: ${e}`));
        return;
      }
      if (name === "plan") return setMode("plan");
      if (name === "mode") {
        const i = MODES.findIndex(([v]) => v === mode);
        const next = MODES[(i + 1) % MODES.length][0];
        if (next === "bypass" && !confirmBypass(lang)) return;
        setMode(next);
        return;
      }
      if (name === "model") {
        setStatus("Escolha o modelo na lista ao lado do campo.");
        return;
      }
      if (name === "clear") {
        setSelected(null);
        setMessages([]);
        draftRef.current?.focus();
        return;
      }
      if (name === "resume") return setSettings("Histórico");
      if (name === "agents") return setSettings("Agentes");
      if (name === "skills") return setSettings("Skills");
      if (name === "memory") return setSettings("Memória");
    },
    [selected, model, mode, note, bumpSessionEpoch, lang],
  );

  const closeSettings = useCallback(() => {
    setSettings(null);
    api
      .boot()
      .then((b) => {
        setBoot(b);
        setSessions(b.sessions);
        setMemoryPaused(b.memory_paused);
        // The open chat was deleted in Histórico: stop showing it.
        const sel = selectedRef.current;
        if (sel && !b.sessions.some((s) => s.id === sel)) {
          setSelected(null);
          setMessages([]);
        }
        void api
          .pinnedSessions()
          .then(setPinnedSessions)
          .catch(() => undefined);
        void Promise.all([
          api.accounts(),
          api.routing().catch(() => ({}) as Routing),
        ]).then(([list, r]) => {
          fetchedLiveModels.current.clear();
          setAccounts(list);
          loadProfiles(r);
          // an `account:model` pick survives while its account is still on,
          // a profile while it still exists
          setModel((prev) =>
            prev &&
            (b.providers.some(
              (p) => p.default_model === prev || p.known_models.includes(prev),
            ) ||
              list.some((a) => a.enabled && prev.startsWith(a.id + ":")) ||
              (r.combos ?? []).some((c) => c.name === prev))
              ? prev
              : (b.providers[0]?.default_model ?? ""),
          );
        });
      })
      .catch(() => undefined);
  }, []);

  // Every connected account's models, sent as `account:model` so the router
  // reaches that exact account (catalogue providers like Groq have no model
  // prefix the core could infer). Then whatever the saved providers list.
  const models = useMemo(() => {
    if (!boot) return [];
    const out: { value: string; label: string }[] = profiles.map((n) => ({
      value: n,
      label: "Perfil · " + n,
    }));
    for (const a of [...accounts].sort((x, y) => x.priority - y.priority)) {
      if (!a.enabled) continue;
      const name =
        a.label || catalogEntry(a.oauth_tool || a.provider)?.name || a.id;
      // An account that publishes its list is the authority: a model it no
      // longer serves (Antigravity retired Gemini 4 Hydrogen) is not offered,
      // not even as its saved default.
      const live = liveModels[a.id];
      const ids = [
        ...(live ? [] : [a.default_model]),
        ...(live ?? catalogEntry(a.oauth_tool || a.provider)?.models ?? []).map(
          (m) => m.id,
        ),
      ];
      for (const m of [...new Set(ids)].filter(
        (m): m is string => !!m && !NOT_CHAT.test(m),
      ))
        out.push({ value: a.id + ":" + m, label: name + " · " + m });
    }
    for (const p of boot.providers)
      for (const m of (p.known_models.length
        ? p.known_models
        : [p.default_model ?? ""]
      ).filter(Boolean))
        out.push({ value: m, label: p.name + " · " + m });
    // a typed `conta:modelo` (or one restored from disk) is listed too
    if (model && !out.some((o) => o.value === model)) {
      const cut = model.indexOf(":");
      const acc =
        cut > 0
          ? accounts.find((a) => a.id === model.slice(0, cut))
          : undefined;
      const name = acc
        ? acc.label ||
          catalogEntry(acc.oauth_tool || acc.provider)?.name ||
          acc.id
        : "";
      out.unshift({
        value: model,
        label: name ? name + " · " + model.slice(cut + 1) : model,
      });
    }
    return out;
  }, [boot, accounts, model, profiles, liveModels]);

  const surface = useMemo(() => {
    const out: Block[] = [...messages];
    // The assistant draft of the running turn is appended live, so the
    // transcript always ends with what is being produced.
    return [...out, ...turn.blocks];
  }, [messages, turn.blocks]);

  const userHistory = useMemo(() => {
    const out: string[] = [];
    for (const b of surface) {
      if (b.kind === "user" && b.text.trim()) out.push(b.text);
    }
    return out.slice(-25);
  }, [surface]);

  const lastUserText = userHistory.length ? userHistory[userHistory.length - 1] : undefined;

  const contextGauge = useMemo(() => {
    const used = turn.usage?.input ?? 0;
    if (!used) return null;
    const limit = estimateContextWindow(turn.route?.model || model);
    const cacheRead = turn.usage?.cacheRead ?? 0;
    const cacheHitPct = cacheRead > 0 ? Math.round((100 * cacheRead) / used) : undefined;
    return { used, limit, cacheHitPct };
  }, [turn.usage, turn.route?.model, model]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteQuery("");
        setPaletteOpen((v) => !v);
        return;
      }
      if (e.key === "Escape" && paletteOpen) {
        e.preventDefault();
        setPaletteOpen(false);
        return;
      }
      // Esc stops the running answer, unless a dialog or popup owns the key.
      if (
        e.key === "Escape" &&
        !e.defaultPrevented &&
        turn.busy &&
        selected &&
        !document.querySelector('[role="dialog"], [role="listbox"], [role="menu"]')
      ) {
        e.preventDefault();
        cancelRequestedRef.current.add(selected);
        void api.cancelTurn(selected).catch(() => undefined);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteOpen, turn.busy, selected]);

  const exportCurrentReport = useCallback(() => {
    if (!selected) {
      setStatus(tr(lang, "Abra uma conversa para exportar", "Open a conversation to export"));
      return;
    }
    const title = current?.title ?? "Relatório Zerith";
    const sid = selected;
    // A long chat shows only its last page: the copy reads every stored row.
    const full: Promise<Block[]> =
      hist?.sid === sid && hist.more
        ? api
            .sessionMessages(sid, 1_000_000_000)
            .then((list) => [...toBlocks(list), ...(turnsRef.current[sid]?.blocks ?? [])])
            .catch(() => surface)
        : Promise.resolve(surface);
    void full.then((blocks) => {
    const mdLines: string[] = [`# ${title}\n`];
    for (const b of blocks) {
      if (b.kind === "user") {
        mdLines.push(`## Solicitação\n\n${b.text}\n`);
      } else if (b.kind === "assistant") {
        if (b.tools.length > 0) {
          mdLines.push(
            `### Ferramentas Executadas\n` +
              b.tools.map((t) => `- **\`${t.name}\`**: ${t.preview || "ok"}`).join("\n") +
              "\n",
          );
        }
        if (b.text.trim()) {
          mdLines.push(`## Análise & Resposta\n\n${b.text}\n`);
        }
      }
    }
    const reportMd = mdLines.join("\n");
    void navigator.clipboard?.writeText(reportMd).catch(() => undefined);
    });
    api
      .exportSession(selected)
      .then((path) => {
        setStatus(
          tr(
            lang,
            `Relatório exportado em ${path} (e copiado em Markdown)`,
            `Report exported to ${path} (and copied as Markdown)`,
          ),
        );
      })
      .catch(() => {
        setStatus(
          tr(
            lang,
            "Relatório Markdown copiado para a área de transferência",
            "Markdown report copied to clipboard",
          ),
        );
      });
  }, [selected, current?.title, surface, lang, hist]);

  const shownHist = hist && hist.sid === selected ? hist : null;
  const historyHead = !shownHist ? null : shownHist.state === "loading" &&
    messages.length === 0 ? (
    <div className="msg-note" role="status">
      <span className="spinner" />
      <span>{tr(lang, "Carregando conversa…", "Loading chat…")}</span>
    </div>
  ) : shownHist.state === "error" ? (
    <div className="callout danger" role="alert">
      <div>
        <strong>{tr(lang, "Não consegui abrir o histórico", "Could not load the history")}</strong>
        <p>{shownHist.error}</p>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => loadHistory(shownHist.sid, shownHist.limit)}
        >
          {tr(lang, "Tentar de novo", "Try again")}
        </button>
      </div>
    </div>
  ) : shownHist.more ? (
    <div className="msg-note">
      <span>
        {tr(
          lang,
          `Mostrando as últimas ${shownHist.limit} mensagens.`,
          `Showing the last ${shownHist.limit} messages.`,
        )}
      </span>
      <button
        type="button"
        className="btn btn-sm"
        disabled={shownHist.state === "loading"}
        onClick={() => loadHistory(shownHist.sid, shownHist.limit + HISTORY_PAGE)}
      >
        {tr(lang, "Carregar anteriores", "Load earlier")}
      </button>
    </div>
  ) : null;

  const pendingApproval = turn.approvals[0] ?? null;
  const tail = (
    <>
      {pendingApproval ? (
        <ApprovalCard
          key={pendingApproval.id}
          tool={pendingApproval.tool}
          summary={pendingApproval.summary}
          always={pendingApproval.always}
          onAnswer={(d) => {
            if (!selected) return;
            const sid = selected;
            const pending = pendingApproval;
            return api
              .answerApproval(sid, pending.id, d)
              .then(() => {
                updateTurn(sid, (prev) => ({
                  ...prev,
                  approvals: prev.approvals.filter((a) => a.id !== pending.id),
                }));
              })
              .catch((e) => {
                setStatus(`Erro ao responder permissão: ${String(e)}`);
                updateTurn(sid, (prev) =>
                  prev.approvals.some((a) => a.id === pending.id)
                    ? prev
                    : { ...prev, approvals: [pending, ...prev.approvals] },
                );
                throw e;
              });
          }}
        />
      ) : null}
      {turn.question ? (
        <QuestionCard
          key={turn.question.id}
          questions={turn.question.questions}
          onAnswer={(answers) => {
            if (!selected || !turn.question) return;
            const sid = selected;
            const pending = turn.question;
            return api
              .answerQuestion(sid, pending.id, answers)
              .then(() => {
                updateTurn(sid, (prev) =>
                  prev.question?.id === pending.id
                    ? { ...prev, question: null }
                    : prev,
                );
              })
              .catch((e) => {
                setStatus(`Erro ao enviar resposta: ${String(e)}`);
                updateTurn(sid, (prev) =>
                  prev.question ? prev : { ...prev, question: pending },
                );
                throw e;
              });
          }}
        />
      ) : null}
    </>
  );

  const activeBotSpec = useMemo(
    () => (activeAgent ? agents.find((a) => a.name === activeAgent) ?? null : null),
    [agents, activeAgent],
  );

  const selectBot = useCallback(
    (bot: AgentView) => {
      setActiveAgent(bot.name);
      try {
        const raw = localStorage.getItem("zerith.botSessions");
        const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
        const sid = map[bot.name];
        if (sid && sessions.some((s) => s.id === sid && !s.archived)) {
          setSelected(sid);
          return;
        }
      } catch {
        // ignore storage errors
      }
      setSelected(null);
      setMessages([]);
      draftRef.current?.focus();
    },
    [sessions],
  );

  const handleSwitchView = useCallback((v: AppView) => {
    if (
      activeView === "workflows" &&
      v !== "workflows" &&
      workflowsDirtyRef.current &&
      !window.confirm(
        tr(
          lang,
          "Há alterações não salvas no workflow. Sair mesmo assim?",
          "The workflow has unsaved changes. Leave anyway?",
        ),
      )
    )
      return;
    if (v !== "workflows") workflowsDirtyRef.current = false;
    setActiveView(v);
    if (v === "bots") {
      void api.agentsList().then(setAgents).catch(() => undefined);
      void workflowsApi.list().then(setWorkflowsList).catch(() => undefined);
    } else if (v === "chat") {
      void workflowsApi.list().then(setWorkflowsList).catch(() => undefined);
    }
  }, [activeView, lang]);

  const runBotWorkflow = useCallback(async (wf: Workflow) => {
    setRunningWfId(wf.id);
    setStatus(`Executando "${wf.name}"…`);
    try {
      const res = await workflowsApi.runNow(wf.id);
      setStatus(`Workflow "${wf.name}" finalizado (${res.status})`);
    } catch (e) {
      setStatus(`Erro em "${wf.name}": ${e}`);
    } finally {
      setRunningWfId(null);
    }
  }, []);

  const createBotHeartbeat = useCallback(
    async (bot: AgentView) => {
      const now = Date.now();
      const tz = -new Date().getTimezoneOffset();
      const firstQP = bot.quickPrompts?.[0] ?? "";
      const taskPrompt = firstQP
        ? `${firstQP}\nResponda com 1 item/link novo por linha ou deixe vazio se não houver novidade.`
        : `Execute uma checagem periódica na sua especialidade (@${bot.name}) usando suas skills e plugins ativos.\nListe apenas novidades relevantes (1 por linha) ou responda vazio se tudo estiver normal.`;
      const wf: Workflow = {
        id: `wf-hb-${bot.name}-${now}`,
        name: `Rotina · @${bot.name}`,
        description: `Heartbeat autônomo do bot @${bot.name} com Skills/Plugins + anti-repetição`,
        icon: bot.icon || emblemForAgent(bot.name),
        enabled: false,
        bot_name: bot.name,
        nodes: [
          {
            id: "hb_cron",
            kind: "trigger_cron",
            label: "A cada 1h",
            x: 60,
            y: 140,
            config: { cron: "0 * * * *", tz_offset_minutes: tz },
          },
          {
            id: "hb_agent",
            kind: "ai_agent",
            label: `@${bot.name} (Loop)`,
            x: 340,
            y: 140,
            config: {
              bot_name: bot.name,
              model: bot.model ?? "",
              use_tools: true,
              max_turns: 6,
              tool_mode: "bypass",
              system: "",
              prompt: taskPrompt,
            },
          },
          {
            id: "hb_dedup",
            kind: "transform",
            label: "Apenas inéditos",
            x: 620,
            y: 140,
            config: { mode: "dedup_lines", pattern: `hb:${bot.name}` },
          },
          {
            id: "hb_check",
            kind: "condition",
            label: "Teve novidade?",
            x: 900,
            y: 140,
            config: { mode: "not_empty", pattern: "" },
          },
        ],
        edges: [
          { id: "e_hb1", from: "hb_cron", to: "hb_agent", branch: null },
          { id: "e_hb2", from: "hb_agent", to: "hb_dedup", branch: null },
          { id: "e_hb3", from: "hb_dedup", to: "hb_check", branch: null },
        ],
        created_at_ms: now,
        updated_at_ms: now,
      };
      try {
        await workflowsApi.save(wf);
        const list = await workflowsApi.list().catch(() => []);
        setWorkflowsList(list);
        setActiveView("workflows");
        setStatus(`Rotina criada para @${bot.name}`);
      } catch (e) {
        setStatus(`Erro ao criar rotina: ${e}`);
      }
    },
    [],
  );

  // Stabilized props & callbacks for memoized sibling chrome so 60Hz turn delta
  // updates do not re-render Sidebar, Composer, BackgroundJobs, or ActiveTerminals.
  const anyTurnBusy = useMemo(
    () => Object.values(turns).some((t) => t.busy),
    [turns],
  );

  const sessionStatusesRef = useRef<Record<string, "busy" | "waiting">>({});
  const sessionStatuses = useMemo(() => {
    const next: Record<string, "busy" | "waiting"> = {};
    for (const [id, t] of Object.entries(turns)) {
      if (!t.busy) continue;
      next[id] = t.approvals.length > 0 || t.question ? "waiting" : "busy";
    }
    const prev = sessionStatusesRef.current;
    const prevKeys = Object.keys(prev);
    const nextKeys = Object.keys(next);
    if (
      prevKeys.length === nextKeys.length &&
      nextKeys.every((k) => prev[k] === next[k])
    ) {
      return prev;
    }
    sessionStatusesRef.current = next;
    return next;
  }, [turns]);

  const sidebarStatus = useCallback(
    (id: string) => sessionStatuses[id] ?? null,
    [sessionStatuses],
  );

  const handleSelectBotSession = useCallback((sid: string) => {
    setSelected(sid);
  }, []);

  const handleCreateBot = useCallback(() => {
    setBotModal({ open: true, editing: null });
  }, []);

  const handleEditBot = useCallback((bot: AgentView) => {
    setBotModal({ open: true, editing: bot });
  }, []);

  const handleNewBotChat = useCallback(() => {
    if (activeAgent) {
      try {
        const raw = localStorage.getItem("zerith.botSessions");
        const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
        delete map[activeAgent];
        localStorage.setItem("zerith.botSessions", JSON.stringify(map));
      } catch {
        // ignore
      }
    }
    setSelected(null);
    setMessages([]);
    draftRef.current?.focus();
  }, [activeAgent]);

  const handleSelectSession = useCallback((id: string) => {
    setActiveAgent(null);
    setSelected(id);
  }, []);

  const handleNewChat = useCallback(() => {
    setActiveAgent(null);
    setSelected(null);
    setMessages([]);
    draftRef.current?.focus();
  }, []);

  const handleTogglePin = useCallback((id: string, pin: boolean) => {
    void api
      .setSessionPinned(id, pin)
      .then(setPinnedSessions)
      .catch((e) => setStatus(String(e)));
  }, []);

  const handleRenameSession = useCallback((id: string, title: string) => {
    void api
      .renameSession(id, title)
      .then(() =>
        setSessions((prev) =>
          prev.map((s) => (s.id === id ? { ...s, title } : s)),
        ),
      )
      .catch((e) => setStatus(String(e)));
  }, []);

  const handleArchiveSession = useCallback(
    (id: string) => {
      void api
        .setArchived(id, true)
        .then(() => {
          cleanupSessionState(id);
          sessionsRef.current = sessionsRef.current.filter((s) => s.id !== id);
          setSessions((prev) => prev.filter((s) => s.id !== id));
          if (selectedRef.current === id) {
            setSelected(null);
            setMessages([]);
          }
        })
        .catch((e) => setStatus(String(e)));
    },
    [cleanupSessionState],
  );

  const handleDeleteSession = useCallback(
    (id: string) => {
      const title =
        sessionsRef.current.find((s) => s.id === id)?.title ||
        tr(lang, "esta conversa", "this chat");
      if (
        !window.confirm(
          tr(
            lang,
            `Excluir "${title}"? Não dá para desfazer.`,
            `Delete "${title}"? This cannot be undone.`,
          ),
        )
      )
        return;
      void api
        .deleteSession(id)
        .then(() => {
          cleanupSessionState(id);
          sessionsRef.current = sessionsRef.current.filter((s) => s.id !== id);
          setSessions((prev) => prev.filter((s) => s.id !== id));
          setPinnedSessions((prev) => prev.filter((x) => x !== id));
          if (selectedRef.current === id) {
            setSelected(null);
            setMessages([]);
          }
        })
        .catch((e) => setStatus(String(e)));
    },
    [cleanupSessionState, lang],
  );

  const handleOpenSettings = useCallback(() => setSettings("Geral"), []);
  const handleOpenQuotas = useCallback(() => setSettings("Cotas"), []);
  const handleOpenWorkflows = useCallback(() => setActiveView("workflows"), []);
  const handleToggleMemory = useCallback(() => {
    setMemoryPaused((prev) => {
      const next = !prev;
      void api.setMemoryPaused(next);
      return next;
    });
  }, []);

  const handleComposerSend = useCallback(() => {
    void send();
  }, [send]);

  const handleComposerSteer = useCallback(
    (t: string) => {
      void steer(t);
    },
    [steer],
  );

  const handleCancelTurn = useCallback(() => {
    const sid = selectedRef.current;
    if (!sid) return;
    cancelRequestedRef.current.add(sid);
    void api.cancelTurn(sid).catch(() => undefined);
  }, []);

  const localizedModes = useMemo(
    () => MODES.map(([k, v]) => [k, translateText(v, lang)] as const),
    [lang],
  );

  const browseFolder = useCallback(() => {
    void api
      .pickFolder(folder || null)
      .then((p) => p && pickFolder(p))
      .catch((e) => setStatus(String(e)));
  }, [folder, pickFolder]);

  const revealFolder = useCallback(() => {
    void api
      .revealWorkspace(folder)
      .catch((e) => setStatus(String(e)));
  }, [folder]);

  const folderNode = useMemo(
    () => (
      <FolderPill
        value={folder}
        branch={branch}
        recent={recent}
        onPick={pickFolder}
        onBrowse={browseFolder}
        onReveal={revealFolder}
      />
    ),
    [folder, branch, recent, pickFolder, browseFolder, revealFolder],
  );

  const handleEditMessage = useCallback(
    (id: number, text: string) => fork(id - 1, "editada", text),
    [fork],
  );

  const handleBranchMessage = useCallback(
    (id: number) => fork(id, "ramo"),
    [fork],
  );

  const handleRetryMessage = useMemo(
    () => (lastUserText ? () => void send(lastUserText) : undefined),
    [lastUserText, send],
  );

  if (error && !boot) {
    return (
      <main className="boot">
        <TitleBar brand />
        <ZerithMark size={40} />
        <h1>Zerith</h1>
        <p>{error}</p>
      </main>
    );
  }
  if (!boot) {
    return (
      <main className="boot">
        <TitleBar brand />
        <ZerithMark size={40} />
        <h1>Zerith</h1>
        <p>
          <span className="spinner" /> Abrindo…
        </p>
      </main>
    );
  }

  const hero = !selected && surface.length === 0;
  // The route the core reported, or the one the next turn will take.
  const pinned = accounts.find((a) => model.startsWith(a.id + ":")) ?? null;
  const planned = boot.providers.find(
    (p) => p.default_model === model || p.known_models.includes(model),
  );
  const route =
    turn.route ??
    (pinned
      ? {
          provider: pinned.oauth_tool || pinned.provider,
          model: model.slice(pinned.id.length + 1),
        }
      : model
        ? { provider: planned?.name ?? "", model }
        : null);
  const account =
    pinned?.id ??
    boot.providers.find((p) => p.name === route?.provider)?.account_id ??
    null;
  const doneTasks = turn.tasks.filter((t) => t.status === "completed").length;

  return (
    <div className="frame">
      <TitleBar view={activeView} onView={handleSwitchView} />
      <div className="shell">
        {activeView === "workflows" ? (
          <InlineBoundary>
            <Suspense fallback={null}>
              <WorkflowsStudio
                inline
                models={models}
                onClose={() => {
                  workflowsDirtyRef.current = false;
                  setActiveView("chat");
                }}
                onDirtyChange={(d) => {
                  workflowsDirtyRef.current = d;
                }}
              />
            </Suspense>
          </InlineBoundary>
        ) : (
        <>
        <Sidebar
          sessions={sessions}
          selected={selected}
          query={query}
          onQuery={setQuery}
          view={activeView}
          agents={agents}
          activeBot={activeAgent}
          onSelectBot={selectBot}
          onSelectBotSession={handleSelectBotSession}
          onCreateBot={handleCreateBot}
          onEditBot={handleEditBot}
          onNewBotChat={handleNewBotChat}
          onSelect={handleSelectSession}
          onNew={handleNewChat}
          pinned={pinnedSessions}
          onTogglePin={handleTogglePin}
          onRename={handleRenameSession}
          onArchive={handleArchiveSession}
          onDelete={handleDeleteSession}
          status={sidebarStatus}
          onSettings={handleOpenSettings}
          onQuotas={handleOpenQuotas}
          onWorkflows={handleOpenWorkflows}
          memoryPaused={memoryPaused}
          onToggleMemory={handleToggleMemory}
        />
        <main className="main">
          <header className="topbar">
            <h1 className="topbar-title">
              {activeAgent ? (
                <BotEmblem
                  icon={activeBotSpec?.icon || emblemForAgent(activeAgent)}
                  size={22}
                  status="on"
                />
              ) : null}
              <span className="topbar-title-text">
                {current?.title ??
                  (activeAgent ? `@${activeAgent}` : tr(lang, "Nova conversa", "New chat"))}
              </span>
              {activeBotSpec ? (
                <button
                  type="button"
                  className="icon-btn sm"
                  onClick={() => setBotModal({ open: true, editing: activeBotSpec })}
                  title={tr(lang, "Configurar este bot", "Configure this bot")}
                  aria-label={tr(lang, "Configurar este bot", "Configure this bot")}
                >
                  <Pencil size={13} strokeWidth={2} />
                </button>
              ) : null}
            </h1>
            {turn.busy ? (
              <span className="top-pill busy" role="status">
                <span className="dot accent live" />
                respondendo
              </span>
            ) : null}
            {selected ? (
              <>
                <button
                  className="icon-btn"
                  onClick={exportCurrentReport}
                  aria-label={tr(
                    lang,
                    "Exportar relatório / conversa em Markdown (.md)",
                    "Export report / conversation as Markdown (.md)",
                  )}
                  title={tr(
                    lang,
                    "Exportar relatório / conversa em Markdown (.md)",
                    "Export report / conversation as Markdown (.md)",
                  )}
                >
                  <Download size={17} strokeWidth={1.75} />
                </button>
                <button
                  className={"icon-btn" + (inspector ? " on" : "")}
                  onClick={() => setInspector((v) => !v)}
                  aria-pressed={inspector}
                  aria-label={
                    inspector
                      ? "Ocultar painel lateral"
                      : "Mostrar painel lateral"
                  }
                  title={
                    inspector
                      ? "Ocultar painel lateral"
                      : "Mostrar painel lateral"
                  }
                >
                  <PanelRight size={18} strokeWidth={1.75} />
                </button>
              </>
            ) : null}
          </header>
          <BackgroundJobs
            active={anyTurnBusy}
            selected={selected}
            sessions={sessions}
          />
          <ActiveTerminals active={anyTurnBusy} />
          <div className="main-body">
            {/* The composer keeps its place in the tree, so the first send from
              the hero does not remount it. */}
            <div className={"convo" + (hero ? " is-hero" : "")}>
              {hero ? (
                <div className="hero-head">
                  {activeBotSpec ? (
                    <>
                      <BotEmblem
                        icon={activeBotSpec.icon || emblemForAgent(activeBotSpec.name)}
                        size={54}
                        status="on"
                      />
                      <h2>@{activeBotSpec.name}</h2>
                      <p>
                        {activeBotSpec.description ||
                          tr(
                            lang,
                            "Bot especialista pronto para atuar no seu workspace.",
                            "Specialist bot ready to act in your workspace.",
                          )}
                      </p>
                      <div className="bot-hero-actions">
                        <button
                          type="button"
                          className="btn sm"
                          onClick={() => setBotModal({ open: true, editing: activeBotSpec })}
                        >
                          <Pencil size={13} />
                          <span>{tr(lang, "Editar bot", "Edit bot")}</span>
                        </button>
                        <button
                          type="button"
                          className="btn sm"
                          onClick={() => void createBotHeartbeat(activeBotSpec)}
                        >
                          <Sparkles size={13} />
                          <span>{tr(lang, "Criar Rotina (Heartbeat)", "Create Routine (Heartbeat)")}</span>
                        </button>
                        <button
                          type="button"
                          className="btn sm"
                          onClick={() => setActiveView("workflows")}
                        >
                          <WorkflowIcon size={13} />
                          <span>Workflows</span>
                        </button>
                      </div>
                      {(activeBotSpec.quickPrompts?.length ?? 0) > 0 ? (
                        <div className="bot-quick-prompts">
                          {activeBotSpec.quickPrompts!.map((qp) => (
                            <button
                              key={qp}
                              type="button"
                              className="bot-quick-chip"
                              onClick={() => void send(qp)}
                            >
                              <Sparkles size={12} />
                              <span>{qp}</span>
                            </button>
                          ))}
                        </div>
                      ) : null}
                      {(() => {
                        const linked = workflowsList.filter(
                          (w) => w.bot_name === activeBotSpec.name,
                        );
                        if (!linked.length) return null;
                        return (
                          <div className="bot-linked-wfs">
                            <span className="bot-linked-label">
                              {tr(lang, "Workflows vinculados:", "Linked workflows:")}
                            </span>
                            {linked.map((wf) => (
                              <button
                                key={wf.id}
                                type="button"
                                className="bot-wf-pill"
                                disabled={runningWfId === wf.id}
                                onClick={() => void runBotWorkflow(wf)}
                                title={wf.description || wf.name}
                              >
                                <Play size={11} />
                                <span>{runningWfId === wf.id ? "…" : wf.name}</span>
                              </button>
                            ))}
                          </div>
                        );
                      })()}
                    </>
                  ) : (
                    <>
                      <ZerithMark size={40} />
                      <h2>Como posso ajudar?</h2>
                      <p>
                        Peça código, revisão ou um plano. As ferramentas rodam no
                        seu computador e pedem aprovação.
                      </p>
                      {hermesOffer ? (
                        <button
                          type="button"
                          className="btn btn-sm"
                          onClick={() => setSettings("Histórico")}
                        >
                          <Import size={14} strokeWidth={2} /> Migrar do Hermes
                        </button>
                      ) : null}
                      <AppVersion />
                    </>
                  )}
                </div>
              ) : (
                <Transcript
                  // Not the Composer's key: siblings sharing a key left the old
                  // transcript in the DOM under the hero after "Nova conversa".
                  key={"t:" + (selected ?? "new")}
                  blocks={surface}
                  tail={tail}
                  sessionId={selected}
                  busy={turn.busy}
                  onEdit={handleEditMessage}
                  onBranch={handleBranchMessage}
                  onRetry={handleRetryMessage}
                  head={historyHead}
                />
              )}
              <Composer
                key={selected ?? "new"}
                textareaRef={draftRef}
                draft={draft}
                onDraft={setDraft}
                onSend={handleComposerSend}
                onSteer={handleComposerSteer}
                onRun={runCommand}
                busy={turn.busy}
                onCancel={handleCancelTurn}
                mode={mode}
                modes={localizedModes}
                onMode={setMode}
                effort={effort}
                onEffort={setEffort}
                lastUserText={lastUserText}
                userHistory={userHistory}
                contextGauge={contextGauge}
                model={model}
                models={models}
                onModel={setModel}
                orchestrator={orchestrator}
                onToggleOrchestrator={toggleOrchestrator}
                executorModel={executorModel || model}
                onExecutorModel={setExecutorModel}
                hidden={hidden}
                onToggleHidden={toggleHidden}
                hint={error || status || "/ comandos  ·  @ arquivos"}
                files={fetchFiles}
                folder={folderNode}
              />
            </div>
            {selected && inspector ? (
              <aside className="inspector" aria-label="Detalhes da conversa">
                {turn.subagents?.length ? (
                  <section className="insp-card">
                    <header className="insp-head">
                      <span className="insp-icon">
                        <Bot size={14} strokeWidth={2} />
                      </span>
                      <h3>Subagentes</h3>
                      <span className="insp-meta">
                        {turn.subagents.some((s) => s.state === "running")
                          ? `${turn.subagents.filter((s) => s.state === "running").length} rodando`
                          : `${turn.subagents.length} no turno`}
                      </span>
                    </header>
                    <ul className="insp-tasks">
                      {turn.subagents.map((s) => (
                        <li
                          key={s.id}
                          title={s.summary || s.name}
                          className={
                            "insp-task " +
                            (s.state === "running"
                              ? "in_progress"
                              : s.state === "done"
                                ? "completed"
                                : "")
                          }
                        >
                          {s.state === "running" ? (
                            <Loader size={14} strokeWidth={2} className="insp-spin" />
                          ) : s.state === "done" ? (
                            <CheckCircle2 size={14} strokeWidth={2} />
                          ) : (
                            <CircleDashed size={14} strokeWidth={2} />
                          )}
                          <span>
                            {s.name}
                            {s.model ? <code className="insp-model">{s.model}</code> : null}
                            {s.summary ? (
                              <em className="insp-sub">{s.summary}</em>
                            ) : null}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </section>
                ) : null}

                {turn.tasks.length > 0 ? (
                  <section className="insp-card">
                    <header className="insp-head">
                      <span className="insp-icon">
                        <ListChecks size={14} strokeWidth={2} />
                      </span>
                      <h3>{tr(lang, "Tarefas", "Tasks")}</h3>
                      <span className="insp-meta">
                        {doneTasks}/{turn.tasks.length}
                      </span>
                    </header>
                    <div className="insp-bar" aria-hidden>
                      <span
                        style={{
                          width: `${(100 * doneTasks) / turn.tasks.length}%`,
                        }}
                      />
                    </div>
                    <ul className="insp-tasks">
                      {turn.tasks.map((t) => (
                        <li key={t.id} className={"insp-task " + t.status}>
                          {t.status === "completed" ? (
                            <CheckCircle2 size={14} strokeWidth={2} />
                          ) : t.status === "in_progress" ? (
                            <Loader
                              size={14}
                              strokeWidth={2}
                              className="insp-spin"
                            />
                          ) : (
                            <CircleDashed size={14} strokeWidth={2} />
                          )}
                          <span>{t.subject}</span>
                        </li>
                      ))}
                    </ul>
                  </section>
                ) : null}

                <section className="insp-card">
                  <header className="insp-head">
                    <span className="insp-icon">
                      <Route size={14} strokeWidth={2} />
                    </span>
                    <h3>{tr(lang, "Rota", "Route")}</h3>
                    <span className="insp-meta">
                      {turn.route
                        ? tr(lang, "em uso", "in use")
                        : tr(lang, "próximo turno", "next turn")}
                    </span>
                  </header>
                  {route ? (
                    <>
                      <div className="insp-route">
                        <Logo id={route.provider} size={28} />
                        <div>
                          <strong>
                            {orchestrator
                              ? `${tr(lang, "Pensamento", "Think")}: ${route.provider}`
                              : route.provider}
                          </strong>
                          {account ? <span>{account}</span> : null}
                        </div>
                        <code>{route.model}</code>
                      </div>
                      {orchestrator ? (
                        <div className="insp-route" style={{ marginTop: 6 }}>
                          <span
                            className="logo logo-mark"
                            style={{
                              width: 28,
                              height: 28,
                              borderRadius: 7,
                              display: "grid",
                              placeItems: "center",
                            }}
                            aria-hidden
                          >
                            <Bot size={15} strokeWidth={1.75} style={{ width: 15, height: 15 }} />
                          </span>
                          <div>
                            <strong>{tr(lang, "Executor", "Executor")}</strong>
                            <span>{tr(lang, "Subagentes / Tools", "Subagents / Tools")}</span>
                          </div>
                          <code>{executorModel || model}</code>
                        </div>
                      ) : null}
                    </>
                  ) : (
                    <p className="insp-empty">
                      {tr(lang, "Nenhum modelo escolhido.", "No model selected.")}
                    </p>
                  )}
                </section>

                {(() => {
                  const includeLive =
                    turn.busy || Boolean(selected && pendingUsage[selected]);
                  const persistedIn = sessionUsage?.promptTokens ?? 0;
                  const persistedOut = sessionUsage?.completionTokens ?? 0;
                  const persistedCache = sessionUsage?.cacheReadTokens ?? 0;
                  const persistedSaved = sessionUsage?.savedTokens ?? 0;
                  const persistedRtk = sessionUsage?.savedRtkTokens ?? 0;
                  const liveIn = turn.usage?.input ?? 0;
                  const liveOut = turn.usage?.output ?? 0;
                  const liveCache = turn.usage?.cacheRead ?? 0;
                  const liveSaved = turn.saved;
                  const useLive =
                    includeLive ||
                    ((sessionUsage?.requests ?? 0) === 0 &&
                      (liveIn > 0 || liveOut > 0));
                  const totalIn = persistedIn + (useLive ? liveIn : 0);
                  const totalOut = persistedOut + (useLive ? liveOut : 0);
                  const totalCache =
                    persistedCache + (useLive ? liveCache : 0);
                  const totalSaved =
                    persistedSaved + (useLive ? liveSaved : 0);
                  // RTK is measured (bytes cut); Ponytail/Caveman are estimates.
                  const totalRtk = persistedRtk + (useLive ? turn.savedRtk : 0);
                  const totalEstimated = Math.max(0, totalSaved - totalRtk);
                  const cacheDenom = Math.max(totalIn, totalCache);
                  const cachePct =
                    totalCache > 0 && cacheDenom > 0
                      ? `${num(totalCache)} (${Math.round((100 * totalCache) / cacheDenom)}%)`
                      : "—";
                  return (
                    <section className="insp-card">
                      <header className="insp-head">
                        <span className="insp-icon">
                          <Gauge size={14} strokeWidth={2} />
                        </span>
                        <h3>{tr(lang, "Uso", "Usage")}</h3>
                        <span className="insp-meta">tokens</span>
                      </header>
                      <dl className="insp-usage">
                        <div>
                          <dt>{tr(lang, "Entrada", "Input")}</dt>
                          <dd>{num(totalIn)}</dd>
                        </div>
                        <div>
                          <dt>{tr(lang, "Saída", "Output")}</dt>
                          <dd>{num(totalOut)}</dd>
                        </div>
                        {selected ? (
                          <SessionCost
                            sessionId={selected}
                            requests={sessionUsage?.requests ?? 0}
                            lang={lang}
                          />
                        ) : null}
                        <div className={totalCache > 0 ? "saved" : undefined}>
                          <dt>Cache hit</dt>
                          <dd>{cachePct}</dd>
                        </div>
                        {(turn.usage?.ttftMs ?? 0) > 0 ? (
                          <div>
                            <dt>TTFT</dt>
                            <dd>{turn.usage!.ttftMs} ms</dd>
                          </div>
                        ) : null}
                        {totalRtk > 0 ? (
                          <div className="saved">
                            <dt>{tr(lang, "Economizados (RTK)", "Saved (RTK)")}</dt>
                            <dd>{num(totalRtk)}</dd>
                          </div>
                        ) : null}
                        {totalEstimated > 0 ? (
                          <div
                            title={tr(
                              lang,
                              "Estimativa de Ponytail/Caveman: não há resposta sem eles para comparar.",
                              "Ponytail/Caveman estimate: there is no answer without them to compare.",
                            )}
                          >
                            <dt>{tr(lang, "Estimado (Ponytail/Caveman)", "Estimated (Ponytail/Caveman)")}</dt>
                            <dd>~{num(totalEstimated)}</dd>
                          </div>
                        ) : null}
                      </dl>
                    </section>
                  );
                })()}
              </aside>
            ) : null}
          </div>
        </main>
        </>
        )}
        {settings ? (
          <Suspense fallback={null}>
            <Settings
              onClose={closeSettings}
              open={settings}
              general={{
                theme: boot.theme,
                reduceMotion: boot.reduce_motion,
                profile: boot.profile,
                memoryPaused,
                models,
                onTheme: setTheme,
                onReduceMotion: setReduceMotion,
                onProfile: setProfile,
                onMemory: (paused) => {
                  setMemoryPaused(paused);
                  void api.setMemoryPaused(paused);
                },
              }}
            />
          </Suspense>
        ) : null}
        {paletteOpen ? (
          <CommandPalette
            query={paletteQuery}
            onQuery={setPaletteQuery}
            onClose={() => setPaletteOpen(false)}
            actions={[
              {
                label: "Nova conversa",
                hint: "Começar do zero",
                run: () => {
                  setActiveAgent(null);
                  setSelected(null);
                  setMessages([]);
                  draftRef.current?.focus();
                },
              },
              {
                label: "Alternar modo de permissão",
                hint: MODES.find(([v]) => v === mode)?.[1] ?? mode,
                run: () => {
                  const i = MODES.findIndex(([v]) => v === mode);
                  setMode(MODES[(i + 1) % MODES.length][0]);
                },
              },
              {
                label: "Compactar conversa atual",
                hint: "/compact",
                run: () => runCommand("compact"),
              },
              {
                label: "Exportar relatório / conversa (.md)",
                hint: "Markdown + Clipboard",
                run: exportCurrentReport,
              },
              {
                label: inspector ? "Ocultar painel lateral" : "Mostrar painel lateral",
                hint: "Inspetor",
                run: () => setInspector((v) => !v),
              },
              {
                label: "Abrir Workflows & Automações",
                hint: "Visual Studio",
                run: () => setActiveView("workflows"),
              },
              {
                label: "Abrir Delegação & Modelos",
                hint: "Roteamento",
                run: () => setSettings("Delegação"),
              },
              {
                label: "Abrir Contas & Provedores",
                hint: "Ajustes",
                run: () => setSettings("Contas"),
              },
              {
                label: "Abrir Skills",
                hint: "Ajustes",
                run: () => setSettings("Skills"),
              },
              {
                label: "Abrir MCP",
                hint: "Ajustes",
                run: () => setSettings("MCP"),
              },
              ...sessions.slice(0, 8).map((s) => ({
                label: `Conversa: ${s.title}`,
                hint: s.model ?? "sessão",
                run: () => {
                  setActiveAgent(null);
                  setSelected(s.id);
                },
              })),
            ]}
          />
        ) : null}
        <BotModal
          open={botModal.open}
          editing={botModal.editing}
          models={models}
          onClose={() => setBotModal({ open: false, editing: null })}
          onSaved={(updated, savedName) => {
            setAgents(updated);
            if (savedName) {
              setActiveAgent(savedName);
            } else if (botModal.editing && activeAgent === botModal.editing.name) {
              setActiveAgent(updated[0]?.name ?? null);
              setSelected(null);
              setMessages([]);
            }
          }}
        />
      </div>
    </div>
  );
}

function CommandPalette(props: {
  query: string;
  onQuery: (v: string) => void;
  onClose: () => void;
  actions: { label: string; hint: string; run: () => void }[];
}) {
  const [cursor, setCursor] = useState(0);
  const q = props.query.trim().toLowerCase();
  const filtered = props.actions.filter(
    (a) =>
      !q ||
      a.label.toLowerCase().includes(q) ||
      a.hint.toLowerCase().includes(q),
  );
  useEffect(() => setCursor(0), [props.query]);
  const pick = (i: number) => {
    const item = filtered[i];
    if (!item) return;
    props.onClose();
    item.run();
  };
  return (
    <div className="palette-backdrop" onMouseDown={props.onClose}>
      <div
        className="palette-modal"
        role="dialog"
        aria-label="Paleta de comandos"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="palette-search">
          <Search size={15} strokeWidth={2} />
          <input
            autoFocus
            type="text"
            placeholder="Ir para conversa ou executar comando…"
            value={props.query}
            onChange={(e) => props.onQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setCursor((i) => (filtered.length ? (i + 1) % filtered.length : 0));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setCursor((i) =>
                  filtered.length ? (i - 1 + filtered.length) % filtered.length : 0,
                );
              } else if (e.key === "Enter") {
                e.preventDefault();
                pick(cursor);
              }
            }}
          />
        </div>
        <div className="palette-list">
          {filtered.length === 0 ? (
            <div className="palette-empty">Nenhum comando encontrado</div>
          ) : (
            filtered.map((a, i) => (
              <button
                key={a.label + i}
                className={"palette-item" + (i === cursor ? " active" : "")}
                onMouseEnter={() => setCursor(i)}
                onClick={() => pick(i)}
              >
                <span>{a.label}</span>
                <span className="palette-hint">{a.hint}</span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
