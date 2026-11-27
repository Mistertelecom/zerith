import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import {
  ArrowRight,
  Bot,
  Check,
  ChevronDown,
  CornerLeftUp,
  Layers,
  ListChecks,
  MessageSquare,
  Minimize2,
  PenLine,
  Plug,
  Route,
  Save,
  Scale,
  Search,
  Telescope,
  Workflow,
  Zap,
  type LucideIcon,
  Plus,
  Trash2,
  Crown,
} from "lucide-react";
import {
  api,
  type Account,
  type AgentView,
  type Preset,
  type ProviderView,
  type Routing,
} from "../lib/api";
import { catalogEntry } from "../lib/catalog";
import { Card, EmptyState, Logo } from "./ui";

type Key =
  | "main_model"
  | "subagent_model"
  | "explore_model"
  | "plan_model"
  | "compact_model"
  | "decision_model";
type Opt = { spec: string; model: string; name: string };
type Group = {
  key: string;
  logo: string;
  title: string;
  sub: string;
  options: Opt[];
};
/** How a model spec reads on screen: display name, where it runs, which logo. */
type Shown = {
  name: string;
  sub: string;
  logo: string | null;
  combo?: boolean;
  off?: boolean;
};

const CONV: Shown = {
  name: "modelo da conversa",
  sub: "o que você escolher no chat",
  logo: null,
};

const OFF: Shown = {
  name: "desligado (opcional)",
  sub: "nenhuma chamada extra",
  logo: null,
  off: true,
};

const ROLES: Record<
  Key,
  { label: string; hint: string; icon: LucideIcon; from: Key | null; optional?: boolean }
> = {
  main_model: {
    label: "Principal",
    hint: "Conversa com você e decide o que delegar.",
    icon: Zap,
    from: null,
  },
  subagent_model: {
    label: "Subagentes",
    hint: "Executam as tarefas delegadas.",
    icon: Workflow,
    from: "main_model",
  },
  explore_model: {
    label: "Explorar",
    hint: "Lê e busca no código, sem editar.",
    icon: Telescope,
    from: "subagent_model",
  },
  plan_model: {
    label: "Planejar",
    hint: "Monta o passo a passo no modo Plano.",
    icon: ListChecks,
    from: "subagent_model",
  },
  compact_model: {
    label: "Compactar",
    hint: "Resume históricos longos preservando entidades.",
    icon: Minimize2,
    from: "explore_model",
  },
  decision_model: {
    label: "Decisão",
    hint: "Modelo calibrado rápido (ex.: microsoft/microsoft-decision-1.1) para esforço Auto, alerta de risco e /goal. Opcional; nunca limita a resposta.",
    icon: Scale,
    from: null,
    optional: true,
  },
};

/** A provider id as people say it: catalogue/preset label, else capitalised. */
const nameOf = (presets: Preset[], id: string) =>
  catalogEntry(id)?.name ??
  presets.find((p) => p.kind === id)?.label ??
  id.charAt(0).toUpperCase() + id.slice(1);

// ponytail: name-based filter for non-chat catalogue models (no modality field); add one if it misfires.
const NOT_CHAT = /image|embed|tts|transcri|whisper|voice|rerank|moderation/i;

/** Same prefixes the Rust router infers a provider from (gateway.rs infer_provider). */
const infer = (m: string) =>
  /^claude-/.test(m)
    ? "anthropic"
    : /^(gpt-|chatgpt-|o[134](-|$))/.test(m)
      ? "openai"
      : /^gemini-/.test(m)
        ? "gemini"
        : /^deepseek-/.test(m)
          ? "deepseek"
          : m;

function options(
  ids: (string | null | undefined)[],
  catalogId: string,
  spec: (m: string) => string,
): Opt[] {
  const names = new Map(
    (catalogEntry(catalogId)?.models ?? []).map((m) => [m.id, m.name] as const),
  );
  const all = [...ids, ...names.keys()].filter(
    (m): m is string => !!m && !NOT_CHAT.test(m),
  );
  return [...new Set(all)].map((m) => ({
    spec: spec(m),
    model: m,
    name: names.get(m) ?? m,
  }));
}

/** Every pickable model: one group per enabled account, then providers no account covers. */
function buildGroups(
  accounts: Account[],
  providers: ProviderView[],
  presets: Preset[],
): Group[] {
  const on = accounts
    .filter((a) => a.enabled)
    .sort((a, b) => a.priority - b.priority);
  const fromAccounts = on.map((a) => {
    const cat = a.oauth_tool || a.provider;
    const pvs = providers.filter(
      (pv) => pv.account_id === a.id || pv.name === a.provider,
    );
    const ids = [
      a.default_model,
      ...pvs.flatMap((pv) => [pv.default_model, ...pv.known_models]),
    ];
    return {
      key: "a:" + a.id,
      logo: cat,
      title: a.label || a.id,
      sub: nameOf(presets, cat),
      options: options(ids, cat, (m) => `${a.id}:${m}`),
    };
  });
  const fromProviders = providers
    .filter(
      (pv) => !on.some((a) => a.id === pv.account_id || a.provider === pv.name),
    )
    .map((pv) => ({
      key: "p:" + pv.name,
      logo: pv.name,
      title: nameOf(presets, pv.name),
      sub: pv.has_key ? "provedor" : "provedor · sem chave",
      options: options(
        [pv.default_model, ...pv.known_models],
        pv.name,
        (m) => `${pv.name}/${m}`,
      ),
    }));
  return [...fromAccounts, ...fromProviders].filter((g) => g.options.length);
}

function describe(
  v: string,
  groups: Group[],
  accounts: Account[],
  presets: Preset[],
  combos: Routing["combos"],
): Shown {
  for (const g of groups) {
    const o = g.options.find((o) => o.spec === v);
    if (o) return { name: o.name, sub: `${g.sub} · ${g.title}`, logo: g.logo };
  }
  const combo = combos?.find((c) => c.name === v);
  if (combo)
    return {
      name: combo.name,
      sub: `combo · ${combo.models.length} modelos`,
      logo: null,
      combo: true,
    };
  const colon = v.indexOf(":");
  const acc =
    colon > 0 ? accounts.find((a) => a.id === v.slice(0, colon)) : undefined;
  if (acc) {
    const cat = acc.oauth_tool || acc.provider;
    const off = acc.enabled ? "" : " · desativada";
    return {
      name: v.slice(colon + 1),
      sub: `${nameOf(presets, cat)} · ${acc.label || acc.id}${off}`,
      logo: cat,
    };
  }
  const slash = v.indexOf("/");
  if (slash > 0)
    return {
      name: v.slice(slash + 1),
      sub: nameOf(presets, v.slice(0, slash)),
      logo: v.slice(0, slash),
    };
  for (const g of groups) {
    const o = g.options.find((o) => o.model === v);
    if (o)
      return { name: o.name, sub: `${g.sub} · via ${g.title}`, logo: g.logo };
  }
  return { name: v, sub: "provedor inferido pelo nome", logo: infer(v) };
}

function Mark({ s, size }: { s: Shown; size: number }) {
  if (s.logo && !s.combo) return <Logo id={s.logo} size={size} />;
  const Icon = s.combo ? Layers : s.off ? Scale : MessageSquare;
  return (
    <span className="dg-tile" style={{ width: size, height: size }}>
      <Icon size={14} strokeWidth={2} />
    </span>
  );
}

/** Button + listbox over every connected model; "Outro…" keeps free typing for odd specs. */
function Picker(p: {
  label: string;
  value: string;
  groups: Group[];
  inherit: Shown;
  emptyLabel?: string;
  show: (v: string) => Shown;
  onPick: (v: string) => void;
  end?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [other, setOther] = useState<string | null>(null);
  const [at, setAt] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const id = useId();

  const needle = q.trim().toLowerCase();
  const groups = p.groups
    .map((g) => ({
      ...g,
      options: g.options.filter(
        (o) =>
          !needle ||
          `${o.model} ${o.name} ${g.title} ${g.sub}`
            .toLowerCase()
            .includes(needle),
      ),
    }))
    .filter((g) => g.options.length);
  // "" = inherit, null = "Outro…"
  const flat: (string | null)[] = [
    "",
    ...groups.flatMap((g) => g.options.map((o) => o.spec)),
    null,
  ];

  const close = () => {
    setOpen(false);
    setQ("");
    setOther(null);
  };
  const pick = (v: string) => {
    p.onPick(v.trim());
    close();
  };

  useEffect(() => {
    if (!open) return;
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) close();
    };
    document.addEventListener("mousedown", off);
    return () => document.removeEventListener("mousedown", off);
  }, [open]);

  // The dialog body scrolls: bring the whole popover into view when it opens.
  useEffect(() => {
    if (open) pop.current?.scrollIntoView({ block: "nearest" });
  }, [open]);

  // Keep the active row in view by scrolling the list only (scrollIntoView would scroll the dialog too).
  useEffect(() => {
    const el = open ? document.getElementById(`${id}-${at}`) : null;
    const list = el?.closest<HTMLElement>(".dg-list");
    if (!el || !list) return;
    const top = el.offsetTop - 34; // room for the sticky group header
    if (top < list.scrollTop) list.scrollTop = top;
    else if (
      el.offsetTop + el.offsetHeight >
      list.scrollTop + list.clientHeight
    )
      list.scrollTop = el.offsetTop + el.offsetHeight - list.clientHeight;
  }, [open, at, id]);

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && open) {
      e.stopPropagation(); // the settings dialog closes on Escape too
      e.nativeEvent.stopImmediatePropagation();
      close();
      return;
    }
    if (!open || other !== null) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setAt(
        (i) =>
          (i + (e.key === "ArrowDown" ? 1 : flat.length - 1)) % flat.length,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const v = flat[at];
      if (v === null) setOther(p.value);
      else if (v !== undefined) pick(v);
    }
  };

  const cur = p.value ? p.show(p.value) : null;
  let i = 0;
  const row = (
    idx: number,
    selected: boolean,
    onClick: () => void,
    children: ReactNode,
    cls = "",
  ) => (
    <div
      key={idx}
      id={`${id}-${idx}`}
      role="option"
      aria-selected={selected}
      className={"dg-opt" + (idx === at ? " at" : "") + cls}
      onMouseDown={(e) => e.preventDefault()}
      onMouseEnter={() => setAt(idx)}
      onClick={onClick}
    >
      {children}
      {selected ? (
        <Check size={14} strokeWidth={2} className="dg-check" />
      ) : null}
    </div>
  );

  return (
    <div className="dg-pick" ref={box} onKeyDown={onKey}>
      <button
        type="button"
        className={"dg-trigger" + (cur ? "" : " inherit")}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Modelo: ${p.label}`}
        title={
          cur
            ? `${cur.name} · ${cur.sub}`
            : p.emptyLabel
              ? `${p.inherit.name} · ${p.inherit.sub}`
              : `Herdar: ${p.inherit.name}`
        }
        onClick={() => {
          if (open) return close();
          setAt(Math.max(0, flat.indexOf(p.value)));
          setOpen(true);
        }}
      >
        {cur ? (
          <Mark s={cur} size={26} />
        ) : (
          <span className="dg-faded">
            <Mark s={p.inherit} size={26} />
          </span>
        )}
        <span className="dg-text">
          <span className="dg-model">
            {cur ? cur.name : p.emptyLabel ? p.inherit.name : "Herdar"}
          </span>
          <span className="dg-sub">
            {cur ? (
              cur.sub
            ) : p.emptyLabel ? (
              p.inherit.sub
            ) : (
              <>
                <ArrowRight size={11} strokeWidth={2} />
                {p.inherit.name}
              </>
            )}
          </span>
        </span>
        <ChevronDown size={14} strokeWidth={2} className="dg-chev" />
      </button>
      {open ? (
        <div ref={pop} className={"popover dg-pop" + (p.end ? " end" : "")}>
          <label className="dg-search">
            <Search size={14} strokeWidth={2} />
            <input
              autoFocus
              role="combobox"
              aria-expanded
              aria-label={`Buscar modelo para ${p.label}`}
              aria-controls={`${id}-list`}
              aria-activedescendant={`${id}-${at}`}
              placeholder="Buscar modelo, conta ou provedor"
              value={q}
              onChange={(e) => {
                setQ(e.target.value);
                setAt(0);
              }}
            />
          </label>
          <div
            className="dg-list"
            role="listbox"
            id={`${id}-list`}
            aria-label={`Modelos para ${p.label}`}
          >
            {row(
              i++,
              !p.value,
              () => pick(""),
              p.emptyLabel ? (
                <>
                  <Scale size={14} strokeWidth={2} className="dg-opt-icon" />
                  <span className="dg-opt-name">{p.emptyLabel}</span>
                  <code>{p.inherit.sub}</code>
                </>
              ) : (
                <>
                  <CornerLeftUp
                    size={14}
                    strokeWidth={2}
                    className="dg-opt-icon"
                  />
                  <span className="dg-opt-name">Herdar</span>
                  <code>{p.inherit.name}</code>
                </>
              ),
            )}
            {groups.map((g) => (
              <div
                key={g.key}
                role="group"
                aria-label={`${g.title} · ${g.sub}`}
              >
                <div className="dg-ghead" aria-hidden>
                  <Logo id={g.logo} size={18} />
                  <strong>{g.title}</strong>
                  <span>{g.sub}</span>
                </div>
                {g.options.map((o) =>
                  row(
                    i++,
                    o.spec === p.value,
                    () => pick(o.spec),
                    <>
                      <span className="dg-opt-name">{o.name}</span>
                      {o.name !== o.model ? <code>{o.model}</code> : null}
                    </>,
                    " dg-opt-in",
                  ),
                )}
              </div>
            ))}
            {needle && !groups.length ? (
              <div className="dg-empty">Nenhum modelo com “{q.trim()}”.</div>
            ) : null}
            {other === null ? (
              row(
                i++,
                false,
                () => setOther(p.value),
                <>
                  <PenLine size={14} strokeWidth={2} className="dg-opt-icon" />
                  <span className="dg-opt-name">Outro…</span>
                  <code>conta:modelo · provedor/modelo</code>
                </>,
              )
            ) : (
              <form
                className="dg-other"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (other.trim()) pick(other);
                }}
              >
                <input
                  className="input mono"
                  autoFocus
                  value={other}
                  placeholder="ex.: openrouter/deepseek/deepseek-chat"
                  aria-label="Modelo personalizado"
                  onChange={(e) => setOther(e.target.value)}
                />
                <button
                  className="btn btn-primary btn-sm"
                  type="submit"
                  disabled={!other.trim()}
                >
                  Usar
                </button>
              </form>
            )}
          </div>
        </div>
      ) : null}
    </div>
  );
}

export function RoutingTab(p: {
  routing: Routing;
  providers: ProviderView[];
  accounts: Account[];
  presets: Preset[];
  onChange: (r: Routing) => void;
  onSave: () => void;
}) {
  const [agents, setAgents] = useState<AgentView[] | null>(null);
  useEffect(() => {
    void api
      .agentsList()
      .then(setAgents)
      .catch(() => setAgents([]));
  }, []);
  const groups = useMemo(
    () => buildGroups(p.accounts, p.providers, p.presets),
    [p.accounts, p.providers, p.presets],
  );

  const r = p.routing;
  const roles = r.role_models ?? {};
  const show = (v: string) =>
    describe(v, groups, p.accounts, p.presets, r.combos);
  const val = (k: Key) => (r[k] ?? "").trim();
  // Empty = inherit: main from the chat, subagents from main, explore/plan from subagents; decision is optional (off).
  const eff = (k: Key): string =>
    val(k) ||
    (k === "main_model" || k === "decision_model"
      ? ""
      : eff(k === "subagent_model" ? "main_model" : "subagent_model"));
  const shown = (spec: string) => (spec ? show(spec) : CONV);
  const setKey = (k: Key, v: string) => p.onChange({ ...r, [k]: v || null });
  const setRole = (name: string, v: string) => {
    const next = { ...roles };
    if (v) next[name] = v;
    else delete next[name];
    p.onChange({ ...r, role_models: next });
  };

  if (!p.accounts.length && !p.providers.length) {
    return (
      <div className="page">
        <Card title="Quem faz a delegação" icon={Route}>
          <EmptyState
            icon={Plug}
            title="Nenhum provedor conectado"
            text="Conecte uma conta na aba Contas; os modelos dela aparecem aqui para cada papel."
          />
        </Card>
      </div>
    );
  }

  const node = (k: Key, cls = "", end = false) => {
    const d = ROLES[k];
    const Icon = d.icon;
    const v = val(k);
    return (
      <div className={`dg-node ${cls}${v ? "" : " inherit"}`}>
        <div className="dg-node-head">
          <span className="dg-node-icon">
            <Icon size={16} strokeWidth={1.75} />
          </span>
          <span className="dg-node-text">
            <span className="dg-node-title">
              {d.label}
              {v ? null : (
                <span className="badge">
                  {d.optional ? (
                    "opcional"
                  ) : (
                    <>
                      <CornerLeftUp size={11} strokeWidth={2} />
                      {d.from
                        ? "herda de " + ROLES[d.from].label
                        : "herda da conversa"}
                    </>
                  )}
                </span>
              )}
            </span>
            <span className="dg-node-hint">{d.hint}</span>
          </span>
        </div>
        <Picker
          label={d.label}
          value={v}
          groups={groups}
          show={show}
          inherit={d.optional ? OFF : shown(d.from ? eff(d.from) : "")}
          emptyLabel={d.optional ? "Desligado (opcional)" : undefined}
          onPick={(x) => setKey(k, x)}
          end={end}
        />
      </div>
    );
  };

  const sub = shown(eff("subagent_model"));
  const list = agents
    ? [...agents].sort((a, b) => Number(a.disabled) - Number(b.disabled))
    : [];
  const extras = Object.keys(roles).filter(
    (k) => !list.some((a) => a.name === k),
  );
  const count = groups.reduce((n, g) => n + g.options.length, 0);

  return (
    <div className="page">
      <Card
        title="Quem faz a delegação"
        icon={Route}
        description={`O Principal conversa com você e distribui o trabalho; cada papel pode usar outro modelo. ${count} modelos de ${groups.length} contas e provedores conectados.`}
        actions={
          <button className="btn btn-primary btn-sm" onClick={p.onSave}>
            <Save size={14} strokeWidth={2} /> Salvar
          </button>
        }
      >
        <div className="dg-flow">
          <div className="dg-you">
            <MessageSquare size={14} strokeWidth={2} />
            <strong>Você</strong>
            <span>conversa</span>
          </div>
          <div className="dg-stem" />
          {node("main_model", "dg-main")}
          <div className="dg-stem bus" />
          <div className="dg-fan">
            {node("subagent_model")}
            {node("explore_model", "dg-end-md")}
            {node("plan_model")}
            {node("compact_model", "", true)}
          </div>
          {node("decision_model", "dg-decision")}

          <div className="dg-agents">
            <div className="dg-agents-head">
              <Bot size={14} strokeWidth={2} />
              <strong>Agentes</strong>
              <span>
                quem não tem modelo próprio usa o de Subagentes ({sub.name})
              </span>
            </div>
            {agents === null ? (
              <div className="dg-agents-note">
                <span className="spinner" /> Carregando agentes…
              </div>
            ) : !list.length && !extras.length ? (
              <div className="dg-agents-note">
                Nenhum agente. Crie um na aba Agentes.
              </div>
            ) : null}
            {list.map((a) => {
              const own = a.model ? show(a.model) : null;
              return (
                <div
                  key={a.name}
                  className={"dg-agent" + (a.disabled ? " off" : "")}
                >
                  <span className="dg-agent-icon">
                    <Bot size={15} strokeWidth={1.75} />
                  </span>
                  <span className="dg-agent-text">
                    <span className="dg-agent-name">
                      {a.name}
                      {own ? <span className="badge">modelo fixo</span> : null}
                      {a.disabled ? (
                        <span className="badge">desativado</span>
                      ) : null}
                    </span>
                    <span className="dg-agent-desc" title={a.description}>
                      {a.description}
                    </span>
                  </span>
                  {own ? (
                    <span
                      className="dg-fixed"
                      title={`${own.name} · ${own.sub}. Definido no arquivo do agente; edite na aba Agentes.`}
                    >
                      <Mark s={own} size={26} />
                      <span className="dg-text">
                        <span className="dg-model">{own.name}</span>
                        <span className="dg-sub">{own.sub}</span>
                      </span>
                    </span>
                  ) : (
                    <Picker
                      label={`agente ${a.name}`}
                      value={roles[a.name] ?? ""}
                      groups={groups}
                      show={show}
                      inherit={sub}
                      onPick={(x) => setRole(a.name, x)}
                      end
                    />
                  )}
                </div>
              );
            })}
            {extras.map((k) => (
              <div key={k} className="dg-agent">
                <span className="dg-agent-icon">
                  <Route size={15} strokeWidth={1.75} />
                </span>
                <span className="dg-agent-text">
                  <span className="dg-agent-name">
                    {k}
                    <span className="badge">papel</span>
                  </span>
                  <span className="dg-agent-desc">
                    Modelo fixo para quem delegar com esse nome.
                  </span>
                </span>
                <Picker
                  label={`papel ${k}`}
                  value={roles[k]}
                  groups={groups}
                  show={show}
                  inherit={sub}
                  onPick={(x) => setRole(k, x)}
                  end
                />
              </div>
            ))}
          </div>
        </div>

        <Profiles
          routing={r}
          groups={groups}
          show={show}
          onChange={p.onChange}
          onSave={p.onSave}
        />

        {r.combos?.some((c) => c.subagent_model == null) ? (
          <div className="dg-combos">
            <span className="dg-section-title">
              <Layers size={14} strokeWidth={2} /> Combos de fallback
              <span>
                se o primeiro falhar antes de responder, tenta o próximo
              </span>
            </span>
            {r.combos
              .filter((c) => c.subagent_model == null)
              .map((c) => (
                <div key={c.name} className="dg-combo">
                  <span className="dg-combo-name">{c.name}</span>
                  {c.models.map((m, j) => {
                    const s = show(m);
                    return (
                      <span key={m + j} className="dg-combo-step">
                        {j ? (
                          <ArrowRight
                            size={14}
                            strokeWidth={2}
                            className="dg-arrow"
                          />
                        ) : null}
                        <span className="dg-chip" title={`${m} · ${s.sub}`}>
                          <Mark s={s} size={20} />
                          <span>{s.name}</span>
                        </span>
                      </span>
                    );
                  })}
                </div>
              ))}
          </div>
        ) : null}

        <div className="dg-legend" aria-label="Legenda">
          <span>
            <i className="dg-lg-line" /> delega
          </span>
          <span>
            <i className="dg-lg-box" /> modelo escolhido
          </span>
          <span>
            <i className="dg-lg-box dashed" /> herda o de cima
          </span>
          <span>
            <i className="dg-lg-box off" /> desativado
          </span>
        </div>
      </Card>
    </div>
  );
}

type Combo = NonNullable<Routing["combos"]>[number];
const PICK: Shown = {
  name: "escolha um modelo",
  sub: "obrigatório",
  logo: null,
};

/** Named orchestrator + agent pairs. Each shows up in the chat's model menu;
 * picking it runs the chat on `models[0]` and delegates to `subagent_model`. */
function Profiles(p: {
  routing: Routing;
  groups: Group[];
  show: (v: string) => Shown;
  onChange: (r: Routing) => void;
  onSave: () => void;
}) {
  const all = p.routing.combos ?? [];
  const profiles = all.filter((c) => c.subagent_model != null);
  // edits replace in place, so a row never jumps while you type its name
  const put = (old: Combo | null, next: Combo | null) =>
    p.onChange({
      ...p.routing,
      combos: old
        ? all.flatMap((c) => (c !== old ? [c] : next ? [next] : []))
        : [...all, ...(next ? [next] : [])],
    });
  const add = () => {
    let n = profiles.length + 1;
    while (all.some((c) => c.name === "Perfil " + n)) n++;
    put(null, { name: "Perfil " + n, models: [], subagent_model: "" });
  };
  const clash = (c: Combo) =>
    all.filter((o) => o.name.trim() === c.name.trim()).length > 1;
  return (
    <div className="dg-profiles">
      <span className="dg-section-title">
        <Layers size={14} strokeWidth={2} /> Perfis de delegação
        <span>aparecem no seletor de modelo do chat</span>
        <button className="btn btn-sm dg-profile-add" onClick={add}>
          <Plus size={14} strokeWidth={2} /> Novo perfil
        </button>
      </span>
      {profiles.length === 0 ? (
        <div className="dg-agents-note">
          Ex.: Opus orquestrando e DeepSeek programando, salvo como
          “OPUS/DEEPSEEK AGENTE”.
        </div>
      ) : null}
      {profiles.map((c, i) => {
        const bad = !c.name.trim() || c.name.includes(":") || clash(c);
        return (
          <div key={i} className="dg-profile">
            <input
              className={"input dg-profile-name" + (bad ? " invalid" : "")}
              value={c.name}
              aria-label="Nome do perfil"
              aria-invalid={bad}
              title={bad ? "Nome vazio, repetido ou com “:”" : undefined}
              onChange={(e) => put(c, { ...c, name: e.target.value })}
            />
            <span className="dg-profile-role">
              <Crown size={14} strokeWidth={2} /> Orquestrador
            </span>
            <Picker
              label="Orquestrador do perfil"
              value={c.models[0] ?? ""}
              groups={p.groups}
              show={p.show}
              inherit={PICK}
              onPick={(x) => put(c, { ...c, models: x ? [x] : [] })}
            />
            <ArrowRight size={14} strokeWidth={2} className="dg-arrow" />
            <span className="dg-profile-role">
              <Bot size={14} strokeWidth={2} /> Agente
            </span>
            <Picker
              label="Agente do perfil"
              value={c.subagent_model ?? ""}
              groups={p.groups}
              show={p.show}
              inherit={{ ...PICK, name: "igual ao orquestrador", sub: "" }}
              onPick={(x) => put(c, { ...c, subagent_model: x })}
              end
            />
            <button
              className="icon-btn"
              title="Excluir perfil"
              aria-label="Excluir perfil"
              onClick={() => put(c, null)}
            >
              <Trash2 size={15} strokeWidth={1.75} />
            </button>
          </div>
        );
      })}
      {profiles.length ? (
        <div className="dg-profile-foot">
          <button className="btn btn-primary btn-sm" onClick={p.onSave}>
            <Save size={14} strokeWidth={2} /> Salvar perfis
          </button>
        </div>
      ) : null}
    </div>
  );
}
