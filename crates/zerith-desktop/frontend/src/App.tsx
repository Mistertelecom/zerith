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
  Bug,
  CheckCircle2,
  CircleDashed,
  Download,
  FolderTree,
  Gauge,
  GitPullRequest,
  ListChecks,
  Loader,
  PanelRight,
  Route,
  Search,
  type LucideIcon,
} from "lucide-react";
import {
  api,
  onTurnDone,
  onTurnEvent,
  onTurnLearned,
  type Account,
  type Boot,
  type Routing,
  type Session,
  type SessionUsageTotals,
} from "./lib/api";
import {
  applyEvent,
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
import { TitleBar } from "./components/TitleBar";
import { AppVersion } from "./components/Update";
import { FolderPill } from "./components/FolderPill";
import { BackgroundJobs } from "./components/BackgroundJobs";
import { ActiveTerminals } from "./components/ActiveTerminals";
import { translateText, tr, useDomTranslator, useLang } from "./lib/i18n";

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

// Starters on the empty conversation; a click puts the prompt in the draft.
const SUGGESTIONS: readonly [LucideIcon, string, string, string][] = [
  [
    FolderTree,
    "Explicar o projeto",
    "Estrutura, módulos e pontos de entrada",
    "Explique a estrutura deste projeto: módulos principais, pontos de entrada e como as peças se conectam.",
  ],
  [
    Bug,
    "Caçar um bug",
    "Rodar os testes e achar a causa raiz",
    "Rode os testes, encontre a primeira falha e investigue a causa raiz antes de propor a correção.",
  ],
  [
    ListChecks,
    "Planejar uma mudança",
    "Quebrar a tarefa em passos pequenos",
    "Antes de editar qualquer arquivo, monte um plano em passos pequenos para esta mudança: ",
  ],
  [
    GitPullRequest,
    "Revisar o diff",
    "Apontar riscos antes do commit",
    "Revise o diff atual e aponte riscos, regressões e o que falta testar antes do commit.",
  ],
];

const num = (n: number) => n.toLocaleString("pt-BR");

export default function App() {
  const [boot, setBoot] = useState<Boot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
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
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;
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
          !v.approval &&
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
  const turn: TurnState = (selected && turns[selected]) || emptyTurn;
  const setTurn = useCallback(
    (next: TurnState | ((prev: TurnState) => TurnState)) => {
      const id = selectedRef.current;
      if (!id) return;
      updateTurn(id, (prev) => (typeof next === "function" ? next(prev) : next));
    },
    [updateTurn],
  );
  const [draft, setDraft] = useState("");
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
  const [workflowsOpen, setWorkflowsOpen] = useState(false);
  const [status, setStatus] = useState("");
  const [inspector, setInspector] = useState(true);
  // Folders used before (newest first, from the store) and the one picked for
  // a conversation that does not exist yet.
  const [recent, setRecent] = useState<string[]>([]);
  const [newFolder, setNewFolder] = useState<string | null>(null);
  const current = sessions.find((s) => s.id === selected) ?? null;
  // The open conversation's folder; a new one starts in the last used.
  const folder = (selected ? current?.workspace : newFolder) ?? recent[0] ?? "";
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
        document.documentElement.dataset.theme =
          b.theme === "light"
            ? "light"
            : b.theme === "dark"
              ? "dark"
              : window.matchMedia("(prefers-color-scheme: light)").matches
                ? "light"
                : "dark";
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
          .catch(() => undefined);
      }
    }
  }, [accounts]);

  const fresh = useRef<string | null>(null);
  // Transcript of the conversation that is open.
  useEffect(() => {
    if (!selected) {
      setMessages([]);
      setTurn(emptyTurn);
      return;
    }
    // A conversation `send` just created already holds its first message and
    // turn; reading the (still empty) store here would wipe both.
    if (fresh.current === selected) {
      fresh.current = null;
      return;
    }
    let live = true;
    api
      .sessionMessages(selected)
      .then((list) => live && setMessages(toBlocks(list)))
      .catch(() => live && setMessages([]));
    return () => {
      live = false;
    };
  }, [selected, setTurn]);

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
      if (event.type === "route") {
        setSessions((list) =>
          list.map((s) =>
            s.id === id ? { ...s, provider: event.provider, model: event.model } : s,
          ),
        );
        return;
      }
      updateTurn(id, (prev) => applyEvent(prev, event));
    });
    const done = onTurnDone((id) => {
      flushDeltas();
      const isSelected = id === selectedRef.current;
      if (isSelected) {
        setPendingUsage((m) => ({ ...m, [id]: true }));
      }
      updateTurn(id, finishTurn);
      // The open conversation gets its stored rows back (with their ids, which
      // "Editar" needs); a closed one rereads them when it is opened.
      if (!isSelected) return;
      void api
        .sessionMessages(id)
        .then((list) => {
          if (id !== selectedRef.current) return;
          setMessages((prev) => {
            const last = turnsRef.current[id]?.blocks.at(-1);
            const stored = last?.kind === "assistant" ? last.stored : undefined;
            return borrowIds(prev, list, stored);
          });
        })
        .catch(() => undefined);
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
      const text =
        "Aprendi: " +
        items
          .map((i) => (i.kind === "skill" ? `skill ${i.name}` : i.text))
          .join(" · ");
      updateTurn(id, (prev) => ({
        ...prev,
        blocks: [...prev.blocks, { kind: "note", text, learned: items }],
      }));
    });
    return () => {
      flushDeltas();
      void ev.then((u) => u());
      void done.then((u) => u());
      void learned.then((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [updateTurn, flushDeltas]);

  const send = useCallback(async () => {
    const prompt = draft.trim();
    if (!prompt) return;
    // Enter while the answer streams: the backend would refuse the second
    // turn and the transcript would split. Keep the draft and say why.
    if (selected && (turns[selected]?.busy ?? false)) {
      setStatus("Espere a resposta terminar (ou pare) para enviar outra");
      return;
    }
    if (!model) {
      setError("Escolha um modelo antes de enviar");
      return;
    }
    setError(null);
    setDraft("");
    let id = selected;
    try {
      if (!id) {
        const created = await api.createSession(titleOf(prompt), folder);
        setSessions((prev) => [created, ...prev]);
        id = created.id;
        fresh.current = id;
        setSelected(id);
        setMessages([]);
      }
      // The previous answer lives only in `turn` until the session is reloaded:
      // keep it in the transcript before the new turn resets that state.
      const done = turn.blocks.map((b) =>
        b.kind === "assistant" ? { ...b, streaming: false } : b,
      );
      setMessages((prev) => [
        ...prev,
        ...done,
        { kind: "user", text: prompt },
      ]);
      updateTurn(id, () => ({ ...emptyTurn, busy: true }));
      // `history: 0` = the execution profile's window, decided by the core.
      await api.sendTurn({
        sessionId: id,
        model,
        prompt,
        mode,
        reasoningEffort: effort === "off" ? null : effort,
        orchestrator,
        executorModel: orchestrator ? executorModel || model : null,
        history: 0,
      });
    } catch (e) {
      setError(String(e));
      if (id) updateTurn(id, (prev) => ({ ...prev, busy: false }));
    }
  }, [
    draft,
    model,
    executorModel,
    orchestrator,
    mode,
    effort,
    selected,
    turns,
    folder,
    updateTurn,
  ]);

  const steer = useCallback(
    async (text: string) => {
      if (!selected || !text.trim()) return;
      const trimmed = text.trim();
      setDraft("");
      try {
        await api.steerTurn(selected, trimmed);
        setStatus("Instrução enviada ao turno em andamento");
      } catch (e) {
        setDraft(trimmed);
        setError(String(e));
      }
    },
    [selected],
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
          setDraft(text);
          requestAnimationFrame(() =>
            document.querySelector<HTMLTextAreaElement>(".composer textarea")?.focus(),
          );
        })
        .catch((e) => setStatus(`Não consegui copiar a conversa: ${e}`));
    },
    [selected, current],
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
        if (turn.busy)
          return setStatus("Espere o turno terminar para compactar");
        if (!model) return setStatus("Escolha um modelo antes de compactar");
        setStatus("compactando…");
        api
          .compact(selected, model)
          .then((n) => {
            setStatus("");
            note(
              n === 0
                ? "Nada para compactar ainda"
                : `Contexto compactado: ${n} mensagens resumidas`,
            );
          })
          .catch((e) => setStatus(`Não consegui compactar: ${e}`));
        return;
      }
      if (name === "context") {
        const total = tokenTotal(turn.usage);
        if (!total) return setStatus("Contexto: nenhum turno ainda");
        setStatus(`Contexto: ${total.toLocaleString()} tokens neste turno`);
        return;
      }
      if (name === "rewind") {
        if (!selected) return setStatus("Nada para rebobinar");
        if (turn.busy)
          return setStatus("Espere o turno terminar para rebobinar");
        api
          .checkpoints(selected)
          .then((turns) => {
            if (!turns.length)
              return setStatus("Nenhum ponto de restauração nesta conversa");
            return api
              .rewind(selected)
              .then((r) => {
                note(`Rebobinado para o turno ${r.turn}`);
                return api.sessionMessages(selected);
              })
              .then((list) => list && setMessages(toBlocks(list)));
          })
          .catch((e) => setStatus(`Não consegui rebobinar: ${e}`));
        return;
      }
      if (name === "plan") return setMode("plan");
      if (name === "mode") {
        const i = MODES.findIndex(([v]) => v === mode);
        setMode(MODES[(i + 1) % MODES.length][0]);
        return;
      }
      if (name === "model") {
        setStatus("Escolha o modelo na lista ao lado do campo.");
        return;
      }
      if (name === "clear") {
        setSelected(null);
        setMessages([]);
        setTurn(emptyTurn);
        draftRef.current?.focus();
        return;
      }
      if (name === "resume") return setSettings("Histórico");
      if (name === "agents") return setSettings("Agentes");
      if (name === "skills") return setSettings("Skills");
      if (name === "memory") return setSettings("Memória");
    },
    [selected, turn.busy, turn.usage, model, mode, note],
  );

  const closeSettings = useCallback(() => {
    setSettings(null);
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
        void Promise.all([
          api.accounts(),
          api.routing().catch(() => ({}) as Routing),
        ]).then(([list, r]) => {
          setAccounts(list);
          loadProfiles(r);
          // an `account:model` pick survives while its account is still on,
          // a profile while it still exists
          setModel((prev) =>
            (prev && b.providers.some((p) => p.default_model === prev)) ||
            list.some((a) => a.enabled && prev.startsWith(a.id + ":")) ||
            (r.combos ?? []).some((c) => c.name === prev)
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

  const lastUserText = useMemo(() => {
    for (let i = surface.length - 1; i >= 0; i--) {
      if (surface[i].kind === "user") return surface[i].text;
    }
    return undefined;
  }, [surface]);

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
      if (
        e.key === "Tab" &&
        e.shiftKey &&
        !settings &&
        !paletteOpen &&
        !(e.target as HTMLElement | null)?.closest?.("textarea")
      ) {
        e.preventDefault();
        const i = MODES.findIndex(([v]) => v === mode);
        const next = MODES[(i + 1) % MODES.length];
        setMode(next[0]);
        setStatus(`Modo: ${next[1]}`);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteOpen, settings, mode]);

  const exportCurrentReport = useCallback(() => {
    if (!selected) {
      setStatus(tr(lang, "Abra uma conversa para exportar", "Open a conversation to export"));
      return;
    }
    const title = current?.title ?? "Relatório Zerith";
    const mdLines: string[] = [`# ${title}\n`];
    for (const b of surface) {
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
  }, [selected, current?.title, surface, lang]);

  const tail = (
    <>
      {turn.approval ? (
        <ApprovalCard
          tool={turn.approval.tool}
          summary={turn.approval.summary}
          onAnswer={(d) => {
            if (selected)
              void api.answerApproval(selected, turn.approval!.id, d);
            setTurn((prev) => ({ ...prev, approval: null }));
          }}
        />
      ) : null}
      {turn.question ? (
        <QuestionCard
          questions={turn.question.questions}
          onAnswer={(answers) => {
            if (selected)
              void api.answerQuestion(selected, turn.question!.id, answers);
            setTurn((prev) => ({ ...prev, question: null }));
          }}
        />
      ) : null}
    </>
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
      <TitleBar />
      <div className="shell">
        <Sidebar
          sessions={sessions}
          selected={selected}
          query={query}
          onQuery={setQuery}
          onSelect={setSelected}
          onNew={() => {
            setSelected(null);
            setMessages([]);
            draftRef.current?.focus();
          }}
          pinned={pinnedSessions}
          onTogglePin={(id, pin) => {
            void api
              .setSessionPinned(id, pin)
              .then(setPinnedSessions)
              .catch((e) => setStatus(String(e)));
          }}
          onRename={(id, title) => {
            void api
              .renameSession(id, title)
              .then(() =>
                setSessions((prev) =>
                  prev.map((s) => (s.id === id ? { ...s, title } : s)),
                ),
              )
              .catch((e) => setStatus(String(e)));
          }}
          onArchive={(id) => {
            void api
              .setArchived(id, true)
              .then(() => {
                setSessions((prev) => prev.filter((s) => s.id !== id));
                if (selected === id) {
                  setSelected(null);
                  setMessages([]);
                }
              })
              .catch((e) => setStatus(String(e)));
          }}
          onDelete={(id) => {
            void api
              .deleteSession(id)
              .then(() => {
                setSessions((prev) => prev.filter((s) => s.id !== id));
                setPinnedSessions((prev) => prev.filter((x) => x !== id));
                if (selected === id) {
                  setSelected(null);
                  setMessages([]);
                }
              })
              .catch((e) => setStatus(String(e)));
          }}
          status={(id) => {
            const t = turns[id];
            if (!t?.busy) return null;
            return t.approval || t.question ? "waiting" : "busy";
          }}
          onSettings={() => setSettings("Geral")}
          onQuotas={() => setSettings("Cotas")}
          onWorkflows={() => setWorkflowsOpen(true)}
          memoryPaused={memoryPaused}
          onToggleMemory={() => {
            const next = !memoryPaused;
            setMemoryPaused(next);
            void api.setMemoryPaused(next);
          }}
        />
        <main className="main">
          <header className="topbar">
            <h1 className="topbar-title">
              {current?.title ?? "Nova conversa"}
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
            active={Object.values(turns).some((t) => t.busy)}
            selected={selected}
            sessions={sessions}
          />
          <ActiveTerminals active={Object.values(turns).some((t) => t.busy)} />
          <div className="main-body">
            {/* The composer keeps its place in the tree, so the first send from
              the hero does not remount it. */}
            <div className={"convo" + (hero ? " is-hero" : "")}>
              {hero ? (
                <div className="hero-head">
                  <ZerithMark size={40} />
                  <h2>Como posso ajudar?</h2>
                  <p>
                    Peça código, revisão ou um plano. As ferramentas rodam no
                    seu computador e pedem aprovação.
                  </p>
                  <AppVersion />
                </div>
              ) : (
                <Transcript
                  key={selected ?? "new"}
                  blocks={surface}
                  tail={tail}
                  sessionId={selected}
                  busy={turn.busy}
                  onEdit={(id, text) => fork(id - 1, "editada", text)}
                  onBranch={(id) => fork(id, "ramo")}
                />
              )}
              <Composer
                draft={draft}
                onDraft={setDraft}
                onSend={() => void send()}
                onSteer={(t) => void steer(t)}
                onRun={runCommand}
                busy={turn.busy}
                onCancel={() => selected && void api.cancelTurn(selected)}
                mode={mode}
                modes={MODES.map(([k, v]) => [k, translateText(v, lang)] as const)}
                onMode={setMode}
                effort={effort}
                onEffort={setEffort}
                lastUserText={lastUserText}
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
                hint={status || error || "/ comandos  ·  @ arquivos"}
                files={() => api.workspaceFiles(folder || undefined)}
                folder={
                  <FolderPill
                    value={folder}
                    recent={recent}
                    onPick={pickFolder}
                    onBrowse={() =>
                      void api
                        .pickFolder(folder || null)
                        .then((p) => p && pickFolder(p))
                        .catch((e) => setStatus(String(e)))
                    }
                    onReveal={() =>
                      void api
                        .revealWorkspace(folder)
                        .catch((e) => setStatus(String(e)))
                    }
                  />
                }
              />
              {hero ? (
                <div className="hero-grid">
                  {SUGGESTIONS.map(([Icon, title, text, prompt]) => (
                    <button
                      key={title}
                      className="hero-card"
                      onClick={() => setDraft(prompt)}
                    >
                      <span className="hero-card-icon">
                        <Icon size={16} strokeWidth={1.75} />
                      </span>
                      <span className="hero-card-text">
                        <strong>{title}</strong>
                        <span>{text}</span>
                      </span>
                    </button>
                  ))}
                </div>
              ) : null}
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
                          <Bot size={20} strokeWidth={1.75} />
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
                  const cacheDenom = totalIn + totalCache;
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
                        {totalSaved > 0 ? (
                          <div className="saved">
                            <dt>{tr(lang, "Economizados", "Saved")}</dt>
                            <dd>{num(totalSaved)}</dd>
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
        {workflowsOpen ? (
          <Suspense fallback={null}>
            <WorkflowsStudio
              models={models}
              onClose={() => setWorkflowsOpen(false)}
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
                run: () => setWorkflowsOpen(true),
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
                run: () => setSelected(s.id),
              })),
            ]}
          />
        ) : null}
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
