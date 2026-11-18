import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Bug,
  CheckCircle2,
  CircleDashed,
  FolderTree,
  Gauge,
  GitPullRequest,
  ListChecks,
  Loader,
  PanelRight,
  Route,
  type LucideIcon,
} from "lucide-react";
import {
  api,
  onTurnDone,
  onTurnEvent,
  onTurnLearned,
  type Account,
  type Boot,
  type ChatMessage,
  type Routing,
  type Session,
  type TurnEvent,
} from "./lib/api";
import {
  applyEvent,
  emptyTurn,
  tokenTotal,
  type Block,
  type TurnState,
} from "./lib/turn";
import { Sidebar } from "./components/Sidebar";
import { Transcript } from "./components/Transcript";
import { ApprovalCard, QuestionCard } from "./components/Cards";
import { Settings, type SettingsTab } from "./components/Settings";
import { Composer, type CommandName } from "./components/Composer";
import { Logo, ZerithMark } from "./components/ui";
import { catalogEntry, NOT_CHAT } from "./lib/catalog";
import { TitleBar } from "./components/TitleBar";
import { FolderPill } from "./components/FolderPill";

const MODEL_KEY = "zerith.model";
// Models the user took out of the chat menu (`account:model` values).
const HIDDEN_KEY = "zerith.hiddenModels";

const MODES = [
  ["default", "Padrão"],
  ["acceptEdits", "Aceitar edições"],
  ["plan", "Plano"],
  ["dontAsk", "Sem perguntas"],
  ["bypass", "Livre"],
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

// A stored message becomes a transcript block. Shared by the conversation load
// and by anything that rewrites the transcript (rewind).
function toBlocks(list: ChatMessage[]): Block[] {
  return list.map((m) =>
    m.role === "user"
      ? ({ kind: "user", text: m.content } as Block)
      : ({
          kind: "assistant",
          text: m.content,
          reasoning: m.reasoning ?? "",
          tools: [],
          streaming: false,
        } as Block),
  );
}

export default function App() {
  const [boot, setBoot] = useState<Boot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Block[]>([]);
  const [turn, setTurn] = useState<TurnState>(emptyTurn);
  const [draft, setDraft] = useState("");
  const [query, setQuery] = useState("");
  // The last pick survives a restart; settings.toml's routing.main_model is the fallback.
  const [model, setModel] = useState(
    () => localStorage.getItem(MODEL_KEY) ?? "",
  );
  const [accounts, setAccounts] = useState<Account[]>([]);
  // Live lists, for the accounts that publish one (Antigravity), by account id.
  const [liveModels, setLiveModels] = useState<
    Record<string, { id: string; name: string }[]>
  >({});
  const [hidden, setHidden] = useState<string[]>(() =>
    JSON.parse(localStorage.getItem(HIDDEN_KEY) ?? "[]"),
  );
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
  const [memoryPaused, setMemoryPaused] = useState(false);
  const [settings, setSettings] = useState<SettingsTab | null>(null);
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
          .recentWorkspaces()
          .then(setRecent)
          .catch(() => undefined);
        void api
          .routing()
          .then((r) => (loadProfiles(r), r))
          .then((r) =>
            setModel(
              (prev) =>
                prev || r.main_model || b.providers[0]?.default_model || "",
            ),
          )
          .catch(() =>
            setModel((prev) => prev || b.providers[0]?.default_model || ""),
          );
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

  // OAuth accounts that publish their models (Antigravity's Gemini 4 Hydrogen
  // is not in any static catalogue); a failure keeps the catalogue list.
  useEffect(() => {
    for (const a of accounts)
      if (a.enabled && a.auth === "oauth" && !liveModels[a.id])
        api
          .accountModels(a.id)
          .then(
            (list) =>
              list.length && setLiveModels((m) => ({ ...m, [a.id]: list })),
          )
          .catch(() => undefined);
  }, [accounts, liveModels]);

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
    api
      .sessionMessages(selected)
      .then((list) => setMessages(toBlocks(list)))
      .catch(() => setMessages([]));
    setTurn(emptyTurn);
  }, [selected]);

  // Turn events. The subscription is per conversation, so a stale stream can
  // never write into the next one. `send` subscribes a brand-new conversation
  // itself before starting its first turn (the effect would subscribe after
  // the next render, and the first events, approval card included, would be
  // lost); the effect then reuses that subscription.
  const listening = useRef<{ id: string; ready: Promise<() => void> } | null>(
    null,
  );
  const subscribe = useCallback((id: string) => {
    if (listening.current?.id === id) return listening.current.ready;
    const ready = onTurnEvent(id, (ev: TurnEvent) => {
      // applyEvent mutates and returns the same object; React skips a render
      // when the reference does not change, so it gets a copy.
      setTurn((prev) =>
        ev.type === "autoCompacted"
          ? {
              ...prev,
              blocks: [
                ...prev.blocks,
                {
                  kind: "note",
                  text: `Conversa resumida automaticamente (${num(ev.before)} → ${num(ev.after)} tokens)`,
                },
              ],
            }
          : applyEvent(structuredClone(prev), ev),
      );
    });
    listening.current = { id, ready };
    return ready;
  }, []);
  useEffect(() => {
    if (!selected) return;
    let stop = false;
    let unlisten: (() => void) | undefined;
    void subscribe(selected).then((u) => (stop ? u() : (unlisten = u)));
    const done = onTurnDone((id) => {
      if (id === selected) setTurn((prev) => ({ ...prev, busy: false }));
    });
    // The background review lands after the answer: a note under it.
    const learned = onTurnLearned((id, items) => {
      if (id !== selected || !items.length) return;
      const text =
        "Aprendi: " +
        items
          .map((i) => (i.kind === "skill" ? `skill ${i.name}` : i.text))
          .join(" · ");
      setTurn((prev) => ({
        ...prev,
        blocks: [...prev.blocks, { kind: "note", text, learned: items }],
      }));
    });
    return () => {
      stop = true;
      unlisten?.();
      if (listening.current?.id === selected) listening.current = null;
      void done.then((u) => u());
      void learned.then((u) => u());
    };
  }, [selected, subscribe]);

  const send = useCallback(async () => {
    const prompt = draft.trim();
    if (!prompt) return;
    if (!model) {
      setError("Escolha um modelo antes de enviar");
      return;
    }
    setError(null);
    setDraft("");
    let id = selected;
    try {
      if (!id) {
        const created = await api.createSession(prompt.slice(0, 48), folder);
        setSessions((prev) => [created, ...prev]);
        id = created.id;
        await subscribe(id);
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
      setTurn({ ...emptyTurn, busy: true });
      await api.sendTurn({ sessionId: id, model, prompt, mode, history: 20 });
    } catch (e) {
      setError(String(e));
      setTurn((prev) => ({ ...prev, busy: false }));
    }
  }, [draft, model, mode, selected, turn.blocks, folder, subscribe]);

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
      const ids = [
        a.default_model,
        ...(
          liveModels[a.id] ??
          catalogEntry(a.oauth_tool || a.provider)?.models ??
          []
        ).map((m) => m.id),
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
            setTurn(emptyTurn);
            draftRef.current?.focus();
          }}
          onSettings={() => setSettings("Geral")}
          onQuotas={() => setSettings("Cotas")}
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
            ) : null}
          </header>
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
                </div>
              ) : (
                <Transcript blocks={surface} tail={tail} />
              )}
              <Composer
                draft={draft}
                onDraft={setDraft}
                onSend={() => void send()}
                onRun={runCommand}
                busy={turn.busy}
                onCancel={() => selected && void api.cancelTurn(selected)}
                mode={mode}
                modes={MODES}
                onMode={setMode}
                model={model}
                models={models}
                onModel={setModel}
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
                <section className="insp-card">
                  <header className="insp-head">
                    <span className="insp-icon">
                      <ListChecks size={14} strokeWidth={2} />
                    </span>
                    <h3>Tarefas</h3>
                    {turn.tasks.length ? (
                      <span className="insp-meta">
                        {doneTasks}/{turn.tasks.length}
                      </span>
                    ) : null}
                  </header>
                  {turn.tasks.length ? (
                    <div className="insp-bar" aria-hidden>
                      <span
                        style={{
                          width: `${(100 * doneTasks) / turn.tasks.length}%`,
                        }}
                      />
                    </div>
                  ) : null}
                  {turn.tasks.length ? (
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
                  ) : (
                    <p className="insp-empty">
                      O agente lista aqui os passos do turno.
                    </p>
                  )}
                </section>

                <section className="insp-card">
                  <header className="insp-head">
                    <span className="insp-icon">
                      <Route size={14} strokeWidth={2} />
                    </span>
                    <h3>Rota</h3>
                    <span className="insp-meta">
                      {turn.route ? "em uso" : "próximo turno"}
                    </span>
                  </header>
                  {route ? (
                    <div className="insp-route">
                      <Logo id={route.provider} size={28} />
                      <div>
                        <strong>{route.provider}</strong>
                        {account ? <span>{account}</span> : null}
                      </div>
                      <code>{route.model}</code>
                    </div>
                  ) : (
                    <p className="insp-empty">Nenhum modelo escolhido.</p>
                  )}
                </section>

                <section className="insp-card">
                  <header className="insp-head">
                    <span className="insp-icon">
                      <Gauge size={14} strokeWidth={2} />
                    </span>
                    <h3>Uso</h3>
                    <span className="insp-meta">tokens</span>
                  </header>
                  <dl className="insp-usage">
                    <div>
                      <dt>Entrada</dt>
                      <dd>{num(turn.usage?.input ?? 0)}</dd>
                    </div>
                    <div>
                      <dt>Saída</dt>
                      <dd>{num(turn.usage?.output ?? 0)}</dd>
                    </div>
                    <div className={turn.saved > 0 ? "saved" : undefined}>
                      <dt>Economizados</dt>
                      <dd>{num(turn.saved)}</dd>
                    </div>
                  </dl>
                </section>
              </aside>
            ) : null}
          </div>
        </main>
        {settings ? (
          <Settings
            onClose={closeSettings}
            open={settings}
            general={{
              theme: boot.theme,
              reduceMotion: boot.reduce_motion,
              profile: boot.profile,
              memoryPaused,
              onTheme: setTheme,
              onReduceMotion: setReduceMotion,
              onProfile: setProfile,
              onMemory: (paused) => {
                setMemoryPaused(paused);
                void api.setMemoryPaused(paused);
              },
            }}
          />
        ) : null}
      </div>
    </div>
  );
}
