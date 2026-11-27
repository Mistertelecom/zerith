// Accounts, plan quotas and usage tabs of the settings screen (loaded on demand).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  type Account,
  type AccountForm,
  type ProbeView,
  type Preset,
  type UsageSummary,
  type Quota,
  type QuotaWindow,
} from "../lib/api";
import {
  Activity,
  Download,
  ChartColumn,
  ChevronRight,
  CircleAlert,
  Coins,
  Cpu,
  ExternalLink,
  Gauge,
  Gift,
  Globe,
  HardDrive,
  KeyRound,
  ListChecks,
  ListOrdered,
  LogIn,
  Pencil,
  Plug,
  Plus,
  RefreshCw,
  Save,
  Search,
  Trash2,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
import {
  CATALOG,
  type CatalogAuth,
  type CatalogEntry,
  NOT_CHAT,
} from "../lib/catalog";
import {
  mergeModelChoices,
  normalizeModelList,
} from "../lib/accountModels";
import { Card, EmptyState, Logo, Stat, Switch, Field, ProbeResult } from "./ui";
import { fmt, compact, nameOf, host, isOauth } from "./settingsFmt";
import { CostCard } from "./SessionCost";

/** The window a new account gets; rows only mention it when it differs. */
const WINDOW_H = 5;

/** Plan line for each subscription login the core knows (`OAuthTool`). */
const OAUTH_TOOLS: Record<string, string> = {
  "claude-code": "Assinatura Claude Pro ou Max",
  codex: "Assinatura ChatGPT Plus ou Pro",
  antigravity: "Conta Google · Gemini",
};

type OAuthState = { tool: string; minutes: number } | null;

const left = (min: number) =>
  min < 60
    ? min + " min"
    : min < 48 * 60
      ? Math.floor(min / 60) + " h"
      : Math.floor(min / 1440) + " d";

/** Where an OAuth credential stands; `undefined` = still asking the cofre. */
function OAuthPill({ state }: { state: OAuthState | undefined }) {
  if (state === undefined) return <span className="badge">verificando…</span>;
  if (state === null)
    return (
      <span className="badge warn">
        <span className="dot warn" />
        sem credencial
      </span>
    );
  if (state.minutes <= 0)
    return (
      <span className="badge danger">
        <span className="dot danger" />
        expirada
      </span>
    );
  return (
    <span className="badge ok">
      <span className="dot ok" />
      expira em {left(state.minutes)}
    </span>
  );
}


/** An account as the form edits it; an empty key or URL keeps what is saved. */
const accountForm = (a: Account): AccountForm => ({
  id: a.id,
  provider: a.provider,
  priority: a.priority,
  quota_tokens: a.quota_tokens,
  window_hours: a.window_hours,
  default_model: a.default_model ?? "",
  enabled: a.enabled,
  key: "",
  base_url: a.base_url ?? "",
  protocol:
    a.protocol === "anthropic" || a.protocol === "openai"
      ? a.protocol
      : undefined,
  enabled_models: normalizeModelList(a.enabled_models ?? []),
});

const AUTH: Record<CatalogAuth, { label: string; icon: LucideIcon }> = {
  oauth: { label: "OAuth", icon: LogIn },
  local: { label: "Local", icon: HardDrive },
  apikey: { label: "Chave", icon: KeyRound },
};

function AuthBadge({ auth }: { auth: CatalogAuth }) {
  const { label, icon: Icon } = AUTH[auth];
  return (
    <span className="badge">
      <Icon size={12} strokeWidth={2} />
      {label}
    </span>
  );
}

const SECTIONS: { auth: CatalogAuth; title: string; hint: string }[] = [
  {
    auth: "oauth",
    title: "Assinaturas",
    hint: "Entre pelo navegador com a conta que você já paga. Sem chave para colar.",
  },
  {
    auth: "local",
    title: "Locais",
    hint: "Servidores rodando na sua máquina. Sem chave e sem custo.",
  },
  {
    auth: "apikey",
    title: "Chave de API",
    hint: "Cole a chave do provedor e escolha o modelo.",
  },
];

const FILTERS = [
  ["all", "Todos"],
  ["oauth", "Assinaturas"],
  ["local", "Locais"],
  ["apikey", "Chave de API"],
  ["on", "Conectados"],
] as const;
type Filter = (typeof FILTERS)[number][0];

/** A provider outside the catalogue: the user names it. */
const CUSTOM: CatalogEntry = {
  id: "",
  name: "Outro provedor",
  auth: "apikey",
  provider: "",
  protocol: "openai",
  baseUrl: "",
};

/** The catalogue card an account belongs to: OAuth by tool, the rest by provider. */
const entryOf = (a: Account): CatalogEntry | undefined =>
  isOauth(a)
    ? CATALOG.find((e) => e.oauthTool === a.oauth_tool)
    : (CATALOG.find((e) => e.auth !== "oauth" && e.id === a.provider) ??
      CATALOG.find((e) => e.auth !== "oauth" && e.provider === a.provider));

/** The card an account is edited with; one from outside the catalogue gets an ad-hoc card. */
const entryFor = (a: Account): CatalogEntry =>
  entryOf(a) ?? {
    ...CUSTOM,
    id: a.provider,
    name: a.label || a.provider,
    auth: isOauth(a) ? "oauth" : "apikey",
    provider: a.provider,
    baseUrl: a.base_url ?? "",
  };

const blurb = (e: CatalogEntry) =>
  e.oauthTool
    ? OAUTH_TOOLS[e.oauthTool]
    : e.baseUrl
      ? host(e.baseUrl)
      : "Endpoint compatível com OpenAI ou Anthropic";

/** `base`, else `base-2`, `base-3`…: account ids are unique. */
const freeId = (base: string, taken: Account[]) => {
  let id = base;
  for (let n = 2; taken.some((a) => a.id === id); n++) id = base + "-" + n;
  return id;
};

/** "22/11 04:59" — Claude sends RFC 3339, Codex sends unix seconds. */
function when(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("pt-BR", {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function resetsAt(w: QuotaWindow): string {
  if (w.resets_at) return when(w.resets_at);
  if (w.resets_at_epoch)
    return when(new Date(w.resets_at_epoch * 1000).toISOString());
  return "";
}

export function AccountsTab(p: {
  accounts: Account[];
  presets: Preset[];
  onSave: (a: AccountForm) => Promise<boolean>;
  onDelete: (id: string) => void;
  onLogin: (tool: string, account: string) => void;
  onImport: (tool: string, account: string) => void;
  onImportPasted: (tool: string, account: string, raw: string) => void;
  onImport9: (tool: string, account: string) => void;
}) {
  const [oauth, setOauth] = useState<Record<string, OAuthState>>({});
  const [quotas, setQuotas] = useState<Record<string, Quota>>({});
  const [sheet, setSheet] = useState<{
    entry: CatalogEntry;
    edit?: Account;
    focusModels?: boolean;
  } | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  // Lower priority goes first; the row shows its place, the raw value rides in the tooltip.
  const ordered = [...p.accounts].sort((a, b) => a.priority - b.priority);
  const linked = (e: CatalogEntry) =>
    p.accounts.filter((a) => entryOf(a) === e).length;
  const passes = (e: CatalogEntry, f: Filter) =>
    f === "all" || (f === "on" ? linked(e) > 0 : e.auth === f);
  const q = query.trim().toLowerCase();
  const shown = CATALOG.filter(
    (e) =>
      passes(e, filter) &&
      (!q || (e.name + " " + e.id + " " + e.baseUrl).toLowerCase().includes(q)),
  );
  const edit = (a: Account, focusModels = false) =>
    setSheet({ entry: entryFor(a), edit: a, focusModels });

  // The pill reads the cofre, so it refreshes whenever the list changes
  // (a login just landed, an account was saved).
  useEffect(() => {
    let live = true;
    for (const a of p.accounts)
      if (isOauth(a)) {
        void api
          .oauthStatus(a.id)
          .then((s) => live && setOauth((o) => ({ ...o, [a.id]: s })))
          .catch(() => live && setOauth((o) => ({ ...o, [a.id]: null })));
        // the plan answers with how much of the window is gone (9router's usage page)
        void api
          .accountQuota(a.id)
          .then((q) => live && setQuotas((m) => ({ ...m, [a.id]: q })))
          .catch(() => undefined);
      }
    return () => {
      live = false;
    };
  }, [p.accounts]);

  return (
    <div className="page">
      <Card
        title="Conectadas"
        icon={ListOrdered}
        description="Ordem de uso: quando uma conta atinge a cota, a próxima do mesmo provedor assume."
      >
        {ordered.length === 0 ? (
          <EmptyState
            icon={Plug}
            title="Nenhuma conta conectada"
            text="Escolha um provedor abaixo: assinatura pelo navegador, servidor local ou chave de API."
          />
        ) : (
          <div className="item-list">
            {ordered.map((a, i) => {
              const e = entryOf(a);
              const quota = quotas[a.id];
              return (
                <div
                  key={a.id}
                  className={
                    "item-row account" +
                    (a.id === sheet?.edit?.id ? " on" : "") +
                    (a.enabled ? "" : " off")
                  }
                >
                  <span
                    className="prio"
                    title={"Prioridade " + a.priority + " (menor vai primeiro)"}
                  >
                    {i + 1}
                  </span>
                  <Logo id={e?.id ?? (a.oauth_tool || a.provider)} size={30} />
                  <span className="item-text">
                    <span className="item-title">
                      {a.label || e?.name || nameOf(p.presets, a.provider)}
                    </span>
                    <span className="item-sub">
                      <span className="mono">{a.id}</span>
                      {a.default_model ? (
                        <>
                          <span className="sep">·</span>
                          <span className="mono">{a.default_model}</span>
                        </>
                      ) : null}
                      {a.enabled_models?.length ? (
                        <>
                          <span className="sep">·</span>
                          <span title="Somente estes modelos aparecem no chat e em Delegação">
                            {a.enabled_models.length}{" "}
                            {a.enabled_models.length === 1
                              ? "modelo no chat"
                              : "modelos no chat"}
                          </span>
                        </>
                      ) : null}
                      {a.window_hours !== WINDOW_H ? (
                        <>
                          <span className="sep">·</span>janela de{" "}
                          {a.window_hours} h
                        </>
                      ) : null}
                      {quota ? (
                        <>
                          {quota.email ? (
                            <>
                              <span className="sep">·</span>
                              <span className="mono">{quota.email}</span>
                              {quota.plan ? (
                                <span className="quota-use">
                                  {" "}
                                  ({quota.plan})
                                </span>
                              ) : null}
                            </>
                          ) : null}
                          {quota.seven_day ? (
                            <>
                              <span className="sep">·</span>
                              <span
                                className={
                                  "quota-use" +
                                  (quota.seven_day.utilization >= 90
                                    ? " hot"
                                    : "")
                                }
                                title={
                                  quota.seven_day
                                    ? "reseta " + resetsAt(quota.seven_day)
                                    : ""
                                }
                              >
                                semana {Math.round(quota.seven_day.utilization)}
                                %
                              </span>
                            </>
                          ) : null}
                          {quota.five_hour ? (
                            <>
                              <span className="sep">·</span>
                              <span className="quota-use">
                                5 h {Math.round(quota.five_hour.utilization)}%
                              </span>
                            </>
                          ) : null}
                        </>
                      ) : null}
                    </span>
                  </span>
                  <span className="cell">
                    {isOauth(a) ? (
                      <OAuthPill state={oauth[a.id]} />
                    ) : (
                      <AuthBadge
                        auth={e?.auth === "local" ? "local" : "apikey"}
                      />
                    )}
                  </span>
                  <span
                    className={"quota" + (a.quota_tokens ? "" : " none")}
                    title={
                      a.quota_tokens
                        ? fmt(a.quota_tokens) +
                          " tokens a cada " +
                          a.window_hours +
                          " h"
                        : "Sem limite de tokens por janela"
                    }
                  >
                    {a.quota_tokens
                      ? "cota " + compact(a.quota_tokens) + " tokens"
                      : "sem limite"}
                  </span>
                  <Switch
                    label={(a.enabled ? "Pausar " : "Ativar ") + a.id}
                    checked={a.enabled}
                    onChange={(v) =>
                      void p.onSave({ ...accountForm(a), enabled: v })
                    }
                  />
                  <span className="item-actions">
                    <button
                      className={
                        "icon-btn sm" + (a.enabled_models?.length ? " on" : "")
                      }
                      title="Escolher modelos visíveis no chat"
                      aria-label={"Modelos de " + a.id}
                      onClick={() => edit(a, true)}
                    >
                      <ListChecks size={14} strokeWidth={2} />
                    </button>
                    <button
                      className="icon-btn sm"
                      title="Editar conta"
                      aria-label={"Editar " + a.id}
                      onClick={() => edit(a)}
                    >
                      <Pencil size={14} strokeWidth={2} />
                    </button>
                    <button
                      className="icon-btn sm danger"
                      title="Excluir conta"
                      aria-label={"Excluir " + a.id}
                      onClick={() => {
                        if (window.confirm(`Excluir a conta ${a.label ?? a.id}? A chave salva dela também é apagada.`))
                          p.onDelete(a.id);
                      }}
                    >
                      <Trash2 size={14} strokeWidth={2} />
                    </button>
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </Card>

      <section className="hub" aria-label="Provedores para conectar">
        <div className="hub-bar">
          <label className="hub-search">
            <Search size={16} strokeWidth={1.75} />
            <input
              className="input"
              type="search"
              value={query}
              placeholder="Buscar provedor…"
              aria-label="Buscar provedor"
              onChange={(ev) => setQuery(ev.target.value)}
            />
          </label>
          <div
            className="hub-chips"
            role="group"
            aria-label="Filtrar provedores"
          >
            {FILTERS.map(([f, label]) => (
              <button
                key={f}
                className={"hub-chip" + (f === filter ? " on" : "")}
                aria-pressed={f === filter}
                onClick={() => setFilter(f)}
              >
                {label}
                <span className="hub-count">
                  {CATALOG.filter((e) => passes(e, f)).length}
                </span>
              </button>
            ))}
          </div>
        </div>

        {SECTIONS.map((s) => {
          const items = shown.filter((e) => e.auth === s.auth);
          const withCustom =
            s.auth === "apikey" &&
            (filter === "all" || filter === "apikey") &&
            !q;
          if (!items.length) return null;
          const Icon = AUTH[s.auth].icon;
          return (
            <div key={s.auth} className="hub-sec">
              <div className="hub-sec-head">
                <span className="hub-sec-icon">
                  <Icon size={14} strokeWidth={2} />
                </span>
                <h4>{s.title}</h4>
                <span className="hub-count">{items.length}</span>
                <p>{s.hint}</p>
              </div>
              <div className="hub-grid">
                {items.map((e) => {
                  const n = linked(e);
                  const expired = p.accounts.some(
                    (a) =>
                      entryOf(a) === e &&
                      isOauth(a) &&
                      oauth[a.id]?.minutes === 0,
                  );
                  return (
                    <button
                      key={e.id}
                      className={"hub-tile" + (n ? " on" : "")}
                      data-entry={e.id}
                      title={e.name + (n ? " · conectado" : "")}
                      onClick={() => setSheet({ entry: e })}
                    >
                      <span className="hub-tile-top">
                        <Logo id={e.id} size={32} />
                        {e.free ? (
                          <span className="badge">
                            <Gift size={12} strokeWidth={2} />
                            Grátis
                          </span>
                        ) : null}
                      </span>
                      <span className="hub-name">{e.name}</span>
                      <span className="hub-sub">{blurb(e)}</span>
                      <span className="hub-tile-foot">
                        <AuthBadge auth={e.auth} />
                        {expired ? (
                          <span className="badge danger">
                            <span className="dot danger" />
                            Expirada
                          </span>
                        ) : n ? (
                          <span className="badge ok">
                            <span className="dot ok" />
                            {n > 1 ? "Conectado ×" + n : "Conectado"}
                          </span>
                        ) : null}
                      </span>
                    </button>
                  );
                })}
                {withCustom ? (
                  <button
                    className="hub-tile custom"
                    data-entry="custom"
                    onClick={() => setSheet({ entry: CUSTOM })}
                  >
                    <span className="hub-tile-top">
                      <span className="hub-plus">
                        <Plus size={16} strokeWidth={1.75} />
                      </span>
                    </span>
                    <span className="hub-name">{CUSTOM.name}</span>
                    <span className="hub-sub">{blurb(CUSTOM)}</span>
                    <span className="hub-tile-foot">
                      <AuthBadge auth="apikey" />
                    </span>
                  </button>
                ) : null}
              </div>
            </div>
          );
        })}

        {shown.length === 0 ? (
          <EmptyState
            icon={Search}
            title={
              filter === "on" && !q ? "Nada conectado ainda" : "Nada encontrado"
            }
            text={
              filter === "on" && !q
                ? "Escolha um provedor em Todos para fazer a primeira conexão."
                : "Nenhum provedor do catálogo bate com a busca. Um endpoint próprio entra como outro provedor."
            }
          >
            <button
              className="btn btn-sm"
              onClick={() => setSheet({ entry: CUSTOM })}
            >
              <Plus size={14} strokeWidth={2} /> Outro provedor
            </button>
          </EmptyState>
        ) : null}
      </section>

      {sheet ? (
        <ConnectSheet
          key={
            sheet.entry.id +
            "/" +
            (sheet.edit?.id ?? "") +
            "/" +
            (sheet.focusModels ? "m" : "e")
          }
          entry={sheet.entry}
          edit={sheet.edit}
          focusModels={sheet.focusModels}
          accounts={p.accounts}
          presets={p.presets}
          oauth={oauth}
          onSave={p.onSave}
          onLogin={p.onLogin}
          onImport={p.onImport}
          onImportPasted={p.onImportPasted}
          onImport9={p.onImport9}
          onEdit={(a) => edit(a)}
          onClose={() => setSheet(null)}
        />
      ) : null}
    </div>
  );
}

const OTHER = "__outro";

/** Connect (or edit) one account: a native modal <dialog>, so it sits above the
 * settings frame and Esc closes only it. */
function ConnectSheet(p: {
  entry: CatalogEntry;
  edit?: Account;
  focusModels?: boolean;
  accounts: Account[];
  presets: Preset[];
  oauth: Record<string, OAuthState>;
  onSave: (a: AccountForm) => Promise<boolean>;
  onLogin: (tool: string, account: string) => void;
  onImport: (tool: string, account: string) => void;
  onImportPasted: (tool: string, account: string, raw: string) => void;
  onImport9: (tool: string, account: string) => void;
  onEdit: (a: Account) => void;
  onClose: () => void;
}) {
  const e = p.entry;
  const custom = !CATALOG.includes(e);
  const models = useMemo(() => e.models ?? [], [e.models]);
  const ref = useRef<HTMLDialogElement>(null);
  const modelsSearchRef = useRef<HTMLInputElement>(null);
  // only used by the "other subscription" block
  const [pasted, setPasted] = useState("");
  const [form, setForm] = useState<AccountForm>(() =>
    p.edit
      ? accountForm(p.edit)
      : {
          id: freeId(e.id || "conta", p.accounts),
          provider: e.provider,
          priority: p.accounts.reduce((m, a) => Math.max(m, a.priority + 1), 0),
          quota_tokens: null,
          window_hours: WINDOW_H,
          // a local server runs whatever was pulled; 9router's list is only a hint there
          default_model:
            e.auth === "local"
              ? ""
              : (models.find((m) => !NOT_CHAT.test(m.id))?.id ?? ""),
          enabled: true,
          key: "",
          label: custom ? null : e.name,
          base_url: e.baseUrl,
          protocol: e.protocol === "anthropic" ? "anthropic" : "openai",
          enabled_models: [],
        },
  );
  const [liveIds, setLiveIds] = useState<string[]>([]);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [modelsStatus, setModelsStatus] = useState<{
    ok: boolean;
    text: string;
  } | null>(null);
  const [modelQuery, setModelQuery] = useState("");
  const [modelTab, setModelTab] = useState<"all" | "selected">("all");
  const [showNonChat, setShowNonChat] = useState(false);
  const [customModel, setCustomModel] = useState("");
  const [other, setOther] = useState(
    () =>
      !!form.default_model &&
      !models.some((m) => m.id === form.default_model) &&
      !(form.enabled_models ?? []).includes(form.default_model),
  );
  const set = useCallback(
    (patch: Partial<AccountForm>) => setForm((f) => ({ ...f, ...patch })),
    [],
  );
  const close = () => ref.current?.close();
  const [probe, setProbe] = useState<ProbeView | null>(null);
  const [testing, setTesting] = useState(false);

  const tool = e.oauthTool ?? p.edit?.oauth_tool ?? null;
  const oauthNew = e.auth === "oauth" && !p.edit;
  const oauthEdit = !!p.edit && isOauth(p.edit);

  const selectedModels = useMemo(
    () => normalizeModelList(form.enabled_models ?? []),
    [form.enabled_models],
  );
  const selectedSet = useMemo(() => new Set(selectedModels), [selectedModels]);

  const allChoices = useMemo(
    () =>
      mergeModelChoices({
        liveIds,
        catalogModels: models,
        selectedIds: selectedModels,
        defaultModel: form.default_model,
      }),
    [liveIds, models, selectedModels, form.default_model],
  );

  // Options for the default-model <select>: selected models first, then catalogue/live
  // capped so a 400+ OpenRouter list stays snappy inside a native <select>.
  const defaultSelectChoices = useMemo(() => {
    const out = [
      ...allChoices.filter((c) => selectedSet.has(c.id)),
      ...allChoices.filter(
        (c) => !selectedSet.has(c.id) && !NOT_CHAT.test(c.id),
      ),
    ];
    return out.slice(0, 120);
  }, [allChoices, selectedSet]);

  const nonChatCount = useMemo(
    () =>
      allChoices.filter(
        (c) => NOT_CHAT.test(c.id) && !selectedSet.has(c.id),
      ).length,
    [allChoices, selectedSet],
  );

  const filteredChoices = useMemo(() => {
    const q = modelQuery.trim().toLowerCase();
    const base = allChoices.filter((c) => {
      if (modelTab === "selected") return selectedSet.has(c.id);
      if (!showNonChat && !selectedSet.has(c.id) && NOT_CHAT.test(c.id))
        return false;
      return true;
    });
    const matched = q
      ? base.filter(
          (c) =>
            c.id.toLowerCase().includes(q) || c.name.toLowerCase().includes(q),
        )
      : base;
    // Without a search query, float checked models to the top so the user sees
    // their active whitelist right away; while searching, keep API order (newest first).
    if (!q && modelTab === "all" && selectedSet.size > 0) {
      return [
        ...matched.filter((c) => selectedSet.has(c.id)),
        ...matched.filter((c) => !selectedSet.has(c.id)),
      ];
    }
    return matched;
  }, [allChoices, modelQuery, modelTab, showNonChat, selectedSet]);

  const toggleModel = (id: string) => {
    const next = selectedSet.has(id)
      ? selectedModels.filter((x) => x !== id)
      : [...selectedModels, id];
    set({ enabled_models: next });
  };

  const addCustomModel = () => {
    const id = customModel.trim();
    if (!id) return;
    if (!selectedSet.has(id)) {
      set({ enabled_models: [...selectedModels, id] });
    }
    setCustomModel("");
  };

  const fetchLiveModels = useCallback(
    async (snapshot: AccountForm) => {
      setFetchingModels(true);
      setModelsStatus(null);
      try {
        if (oauthEdit && p.edit) {
          const list = await api.accountModels(p.edit.id);
          const ids = normalizeModelList(list.map((m) => m.id));
          if (ids.length) {
            setLiveIds(ids);
            setModelsStatus({
              ok: true,
              text: `${ids.length} modelos carregados da conta.`,
            });
          } else {
            setModelsStatus({
              ok: true,
              text: "Esta conta não publica lista remota; marque dos sugeridos ou adicione um ID.",
            });
          }
        } else {
          const res = await api.accountTest(snapshot);
          const ids = normalizeModelList(res.models ?? []);
          if (res.ok && ids.length) {
            setLiveIds(ids);
            setModelsStatus({
              ok: true,
              text: `${ids.length} modelos carregados da API (${res.latency_ms} ms).`,
            });
          } else if (res.ok) {
            setModelsStatus({
              ok: true,
              text: "Conexão OK, mas o provedor não retornou lista em /models.",
            });
          } else {
            setModelsStatus({
              ok: false,
              text: res.detail || `Falha ao listar modelos (HTTP ${res.status}).`,
            });
          }
        }
      } catch (err) {
        setModelsStatus({ ok: false, text: String(err) });
      } finally {
        setFetchingModels(false);
      }
    },
    [oauthEdit, p.edit],
  );

  // the real request the router would make: endpoint + protocol + key (typed or in the cofre)
  const test = async () => {
    setTesting(true);
    setProbe(null);
    try {
      const res = await api.accountTest(form);
      setProbe(res);
      if (res.ok && res.models.length) {
        const ids = normalizeModelList(res.models);
        setLiveIds(ids);
        setModelsStatus({
          ok: true,
          text: `${ids.length} modelos carregados da API (${res.latency_ms} ms).`,
        });
      }
    } catch (err) {
      setProbe({
        ok: false,
        status: 0,
        latency_ms: 0,
        models: [],
        detail: String(err),
      });
    } finally {
      setTesting(false);
    }
  };

  const didAutoFetch = useRef(false);
  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal();
    if (p.focusModels) {
      modelsSearchRef.current?.focus();
      if (!didAutoFetch.current && p.edit) {
        didAutoFetch.current = true;
        void fetchLiveModels(accountForm(p.edit));
      }
    } else {
      d?.querySelector<HTMLElement>("[data-first]")?.focus();
    }
  }, [p.focusModels, p.edit, fetchLiveModels]);

  const linked =
    custom || p.edit ? [] : p.accounts.filter((a) => entryOf(a) === e);
  const url = (form.base_url ?? "").trim();
  const urlOk = !url || /^https?:\/\/\S+$/.test(url);
  const ready =
    !!form.id.trim() &&
    !!form.provider.trim() &&
    urlOk &&
    (!!p.edit || e.auth !== "apikey" || !!form.key?.trim());
  const keyUrl = e.keyUrl ?? e.website;
  const login = (id: string) => {
    if (tool) p.onLogin(tool, id);
    close();
  };
  const [saving, setSaving] = useState(false);
  const [saveErr, setSaveErr] = useState(false);
  // close only once the backend accepted it, so a failure keeps the typed key
  const save = async () => {
    setSaving(true);
    setSaveErr(false);
    const ok = await p.onSave({
      ...form,
      enabled_models: selectedModels,
    });
    setSaving(false);
    if (ok) close();
    else setSaveErr(true);
  };

  const MAX_RENDER = 120;
  const visibleChoices = filteredChoices.slice(0, MAX_RENDER);

  return (
    <dialog
      ref={ref}
      className={"sheet" + (oauthNew ? "" : " sheet-wide")}
      aria-label={(p.edit ? "Editar " : "Conectar ") + e.name}
      onClose={p.onClose}
      // Esc closes this dialog only, not the settings behind it.
      onKeyDown={(ev) => ev.key === "Escape" && ev.stopPropagation()}
      onMouseDown={(ev) => ev.target === ev.currentTarget && close()}
    >
      <div className="sheet-in">
        <header className="sheet-head">
          {e.id ? (
            <Logo id={e.id} size={40} />
          ) : (
            <span className="hub-plus big">
              <Plus size={20} strokeWidth={1.75} />
            </span>
          )}
          <div className="sheet-title">
            <h3>{p.edit ? p.edit.id : e.name}</h3>
            <p>{p.edit ? e.name + " · " + blurb(e) : blurb(e)}</p>
          </div>
          <AuthBadge auth={e.auth} />
          <button
            className="icon-btn"
            onClick={close}
            title="Fechar"
            aria-label="Fechar"
          >
            <X size={18} strokeWidth={1.75} />
          </button>
        </header>

        <div className="sheet-body">
          {linked.length ? (
            <div className="sheet-linked">
              <span className="field-label">Já conectadas</span>
              {linked.map((a) => (
                <div key={a.id} className="sheet-linked-row">
                  <span className="mono">{a.id}</span>
                  {isOauth(a) ? <OAuthPill state={p.oauth[a.id]} /> : null}
                  <span className="spacer" />
                  {tool ? (
                    <button
                      className={
                        "btn btn-sm" +
                        (p.oauth[a.id]?.minutes === 0 ? " btn-primary" : "")
                      }
                      onClick={() => login(a.id)}
                    >
                      <RefreshCw size={14} strokeWidth={2} />{" "}
                      {p.oauth[a.id]?.minutes === 0 ? "Reconectar" : "Renovar"}
                    </button>
                  ) : null}
                  <button
                    className="icon-btn sm"
                    title="Editar conta"
                    aria-label={"Editar " + a.id}
                    onClick={() => p.onEdit(a)}
                  >
                    <Pencil size={14} strokeWidth={2} />
                  </button>
                </div>
              ))}
            </div>
          ) : null}

          {oauthNew ? (
            <>
              <p className="sheet-lead">
                O botão abre o navegador para você entrar com a sua conta (
                {blurb(e)}). Nada de chave: a credencial vai direto para o cofre
                do sistema.
              </p>
              <Field
                label="ID da conta"
                value={form.id}
                onChange={(v) => set({ id: v })}
                hint="Nome desta conexão no Zerith; um ID novo cria uma segunda conta."
              />
              <button
                data-first
                className={
                  "btn btn-lg sheet-cta" + (linked.length ? "" : " btn-primary")
                }
                disabled={!form.id.trim()}
                onClick={() => login(form.id.trim())}
              >
                {linked.length ? (
                  <Plus size={18} strokeWidth={1.75} />
                ) : (
                  <LogIn size={18} strokeWidth={1.75} />
                )}
                {linked.length
                  ? "Conectar outra conta"
                  : "Conectar com o navegador"}
              </button>
              {/* 9router's way for Claude: take the CLI's saved token, no browser */}
              <button
                className="btn"
                disabled={!form.id.trim()}
                onClick={() => tool && p.onImport(tool, form.id.trim())}
              >
                <LogIn size={16} strokeWidth={1.75} /> Importar do CLI
              </button>
              <button
                className="btn"
                disabled={!form.id.trim()}
                onClick={() =>
                  tool && form.id.trim() && p.onImport9(tool, form.id.trim())
                }
              >
                <Download size={16} strokeWidth={1.75} /> Copiar do 9router
              </button>
              {/* a second subscription: paste the other machine's .credentials.json */}
              <details className="paste-cred">
                <summary>Tenho outra conta (colar credencial)</summary>
                <textarea
                  className="input"
                  rows={4}
                  placeholder={
                    tool === "claude-code"
                      ? '{\n  "claudeAiOauth": { "accessToken": "…", "refreshToken": "…", "expiresAt": 0 }\n}'
                      : '{\n  "tokens": { "access_token": "…", "refresh_token": "…", "account_id": "…" }\n}'
                  }
                  value={pasted}
                  onChange={(ev) => setPasted(ev.target.value)}
                />
                <button
                  className="btn btn-sm"
                  disabled={!form.id.trim() || !pasted.trim()}
                  onClick={() =>
                    tool &&
                    form.id.trim() &&
                    p.onImportPasted(tool, form.id.trim(), pasted)
                  }
                >
                  Importar credencial colada
                </button>
              </details>
            </>
          ) : (
            <div className="form">
              {e.auth === "local" && !p.edit ? (
                <p className="sheet-lead">
                  Servidor rodando nesta máquina em{" "}
                  <span className="mono">{host(e.baseUrl)}</span>. Deixe-o
                  aberto e escolha o modelo carregado nele.
                </p>
              ) : null}
              {custom ? (
                <label className="field">
                  <span className="field-label">Provedor</span>
                  <input
                    data-first
                    list="zerith-provider-kinds"
                    value={form.provider}
                    placeholder="ex.: openrouter ou o nome dado na aba Provedor"
                    onChange={(ev) => set({ provider: ev.target.value })}
                  />
                  <datalist id="zerith-provider-kinds">
                    {p.presets.map((x) => (
                      <option key={x.kind} value={x.kind}>
                        {x.label}
                      </option>
                    ))}
                  </datalist>
                </label>
              ) : null}
              {e.auth === "apikey" ? (
                <label className="field">
                  <span className="field-label sheet-label">
                    Chave de API
                    {keyUrl ? (
                      <a
                        className="sheet-link"
                        href={keyUrl}
                        target="_blank"
                        rel="noreferrer"
                      >
                        Obter chave <ExternalLink size={12} strokeWidth={2} />
                      </a>
                    ) : null}
                  </span>
                  <input
                    data-first
                    type="password"
                    autoComplete="off"
                    value={form.key ?? ""}
                    placeholder={
                      p.edit
                        ? "Deixe vazio para manter a atual"
                        : "Cole a chave aqui"
                    }
                    onChange={(ev) => set({ key: ev.target.value })}
                  />
                  <span className="field-hint">
                    Vai para o cofre do sistema; o arquivo de ajustes nunca a
                    recebe.
                  </span>
                </label>
              ) : null}
              <label className="field">
                <span className="field-label">Modelo padrão</span>
                {defaultSelectChoices.length ? (
                  <select
                    className="select"
                    value={other ? OTHER : (form.default_model ?? "")}
                    onChange={(ev) => {
                      const v = ev.target.value;
                      setOther(v === OTHER);
                      if (v !== OTHER) {
                        const nextPatch: Partial<AccountForm> = {
                          default_model: v,
                        };
                        // If a whitelist is active, keep the chosen default visible in chat too.
                        if (v && selectedModels.length > 0 && !selectedSet.has(v)) {
                          nextPatch.enabled_models = [v, ...selectedModels];
                        }
                        set(nextPatch);
                      }
                    }}
                  >
                    <option value="">Padrão do provedor</option>
                    {defaultSelectChoices.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.name === m.id ? m.id : `${m.name} (${m.id})`}
                      </option>
                    ))}
                    <option value={OTHER}>Outro…</option>
                  </select>
                ) : null}
                {!defaultSelectChoices.length || other ? (
                  <input
                    className="mono"
                    value={form.default_model ?? ""}
                    placeholder="id do modelo, ex.: qwen3:8b"
                    onChange={(ev) => set({ default_model: ev.target.value })}
                  />
                ) : null}
              </label>

              <section
                className="model-picker"
                aria-label="Modelos visíveis no chat"
              >
                <div className="model-picker-head">
                  <div className="model-picker-title">
                    <span className="field-label">Modelos no chat</span>
                    <span
                      className={
                        "badge" + (selectedModels.length > 0 ? " ok" : "")
                      }
                    >
                      {selectedModels.length > 0
                        ? `${selectedModels.length} ${selectedModels.length === 1 ? "ativo" : "ativos"}`
                        : "Padrão do catálogo"}
                    </span>
                  </div>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={
                      fetchingModels ||
                      (!oauthEdit && (!form.provider.trim() || !urlOk))
                    }
                    onClick={() => void fetchLiveModels(form)}
                    title="Busca a lista atualizada direto do endpoint /models do provedor"
                  >
                    {fetchingModels ? (
                      <span className="spinner" aria-label="Carregando modelos" />
                    ) : (
                      <RefreshCw size={13} strokeWidth={2} />
                    )}
                    Atualizar da API
                  </button>
                </div>

                <p className="field-hint">
                  {selectedModels.length > 0
                    ? "Somente os modelos marcados abaixo aparecem no chat e em Delegação."
                    : "Nenhum filtro ativo — o chat usa as sugestões padrão. Marque só os modelos que quiser usar."}
                </p>

                {modelsStatus ? (
                  <div
                    className={
                      "model-picker-status" + (modelsStatus.ok ? " ok" : " bad")
                    }
                    role="status"
                  >
                    {modelsStatus.text}
                  </div>
                ) : null}

                <div className="model-picker-bar">
                  <label className="model-picker-search">
                    <Search size={14} strokeWidth={2} />
                    <input
                      ref={modelsSearchRef}
                      data-models-search
                      className="input"
                      type="search"
                      value={modelQuery}
                      placeholder="Buscar modelo (ex.: claude, gpt, decision)…"
                      aria-label="Buscar modelo"
                      onChange={(ev) => setModelQuery(ev.target.value)}
                    />
                  </label>
                  <div
                    className="model-picker-tabs"
                    role="group"
                    aria-label="Filtrar lista de modelos"
                  >
                    <button
                      type="button"
                      className={
                        "model-picker-tab" + (modelTab === "all" ? " on" : "")
                      }
                      aria-pressed={modelTab === "all"}
                      onClick={() => setModelTab("all")}
                    >
                      Todos ({allChoices.length - (showNonChat ? 0 : nonChatCount)})
                    </button>
                    <button
                      type="button"
                      className={
                        "model-picker-tab" +
                        (modelTab === "selected" ? " on" : "")
                      }
                      aria-pressed={modelTab === "selected"}
                      onClick={() => setModelTab("selected")}
                    >
                      Ativos ({selectedModels.length})
                    </button>
                  </div>
                </div>

                <div className="model-picker-list" role="list">
                  {visibleChoices.length === 0 ? (
                    <div className="model-picker-empty">
                      {modelTab === "selected"
                        ? "Nenhum modelo marcado ainda. Alterne para Todos ou adicione um ID abaixo."
                        : "Nenhum modelo bate com a busca. Você pode adicionar o ID manualmente abaixo."}
                    </div>
                  ) : (
                    <>
                      {visibleChoices.map((c) => {
                        const checked = selectedSet.has(c.id);
                        const isDef = c.id === (form.default_model ?? "").trim();
                        return (
                          <label
                            key={c.id}
                            className={"model-pick-row" + (checked ? " on" : "")}
                            role="listitem"
                          >
                            <input
                              type="checkbox"
                              checked={checked}
                              onChange={() => toggleModel(c.id)}
                            />
                            <span className="model-pick-text">
                              <span className="model-pick-id mono">{c.id}</span>
                              {c.name !== c.id ? (
                                <span className="model-pick-name">{c.name}</span>
                              ) : null}
                            </span>
                            {isDef ? <span className="badge">padrão</span> : null}
                            {c.fromApi ? (
                              <span
                                className="model-pick-api"
                                title="Listado ao vivo pela API"
                              >
                                API
                              </span>
                            ) : null}
                          </label>
                        );
                      })}
                      {filteredChoices.length > MAX_RENDER ? (
                        <div className="model-picker-more">
                          Mostrando {MAX_RENDER} de {filteredChoices.length} —
                          refine na busca para achar outros modelos.
                        </div>
                      ) : null}
                    </>
                  )}
                </div>

                <div className="model-picker-foot">
                  <div className="model-picker-add">
                    <input
                      className="input mono"
                      value={customModel}
                      placeholder="Adicionar ID manual (ex.: openai/gpt-5.4)"
                      aria-label="Adicionar ID de modelo manualmente"
                      onChange={(ev) => setCustomModel(ev.target.value)}
                      onKeyDown={(ev) => {
                        if (ev.key === "Enter") {
                          ev.preventDefault();
                          addCustomModel();
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="btn btn-sm"
                      disabled={!customModel.trim()}
                      onClick={addCustomModel}
                    >
                      <Plus size={13} strokeWidth={2} /> Adicionar
                    </button>
                  </div>
                  <div className="model-picker-actions">
                    {nonChatCount > 0 && modelTab === "all" ? (
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm"
                        onClick={() => setShowNonChat((v) => !v)}
                      >
                        {showNonChat
                          ? "Ocultar utilitários"
                          : `+ ${nonChatCount} utilitários`}
                      </button>
                    ) : null}
                    {selectedModels.length > 0 ? (
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm"
                        onClick={() => set({ enabled_models: [] })}
                        title="Remove o filtro e volta a exibir as sugestões padrão do catálogo"
                      >
                        Usar padrão
                      </button>
                    ) : null}
                  </div>
                </div>
              </section>
              <details className="sheet-adv" open={custom || undefined}>
                <summary>
                  <ChevronRight size={14} strokeWidth={2} /> Avançado
                </summary>
                <div className="form">
                  {!p.edit || e.auth !== "oauth" ? (
                    <div className="row2">
                      {p.edit ? null : (
                        <Field
                          label="ID da conta"
                          value={form.id}
                          onChange={(v) => set({ id: v })}
                        />
                      )}
                      {e.auth === "oauth" ? null : (
                        <Field
                          label="URL base"
                          value={form.base_url ?? ""}
                          onChange={(v) => set({ base_url: v })}
                          placeholder={
                            e.baseUrl || "https://api.exemplo.com/v1"
                          }
                          hint={
                            urlOk
                              ? undefined
                              : "Precisa começar com http:// ou https://"
                          }
                        />
                      )}
                    </div>
                  ) : null}
                  {custom ? (
                    <label className="field">
                      <span className="field-label">Protocolo</span>
                      <select
                        className="select"
                        value={form.protocol ?? "openai"}
                        onChange={(ev) =>
                          set({
                            protocol: ev.target.value as "openai" | "anthropic",
                          })
                        }
                      >
                        <option value="openai">
                          OpenAI (chat/completions)
                        </option>
                        <option value="anthropic">Anthropic (messages)</option>
                      </select>
                    </label>
                  ) : null}
                  <div className="row3">
                    <Field
                      label="Prioridade"
                      type="number"
                      value={String(form.priority)}
                      onChange={(v) => set({ priority: Number(v) || 0 })}
                      hint="Menor vai primeiro."
                    />
                    <Field
                      label="Janela (h)"
                      type="number"
                      value={String(form.window_hours)}
                      onChange={(v) =>
                        set({ window_hours: Math.max(1, Number(v) || 1) })
                      }
                    />
                    <Field
                      label="Cota de tokens"
                      type="number"
                      value={
                        form.quota_tokens == null
                          ? ""
                          : String(form.quota_tokens)
                      }
                      onChange={(v) =>
                        set({
                          quota_tokens: v.trim() ? Number(v) || null : null,
                        })
                      }
                      placeholder="sem cota"
                    />
                  </div>
                </div>
              </details>
            </div>
          )}
        </div>

        {saveErr ? (
          <div className="sheet-probe" role="alert">
            Não foi possível salvar a conta. Confira os dados e tente de novo.
          </div>
        ) : null}
        {probe ? (
          <div className="sheet-probe">
            <ProbeResult probe={probe} />
          </div>
        ) : null}
        <footer className="sheet-foot">
          {e.website ? (
            <a
              className="btn btn-ghost btn-sm"
              href={e.website}
              target="_blank"
              rel="noreferrer"
            >
              <Globe size={14} strokeWidth={2} /> Site
            </a>
          ) : null}
          <span className="spacer" />
          <button className="btn" onClick={close}>
            Cancelar
          </button>
          {p.edit && tool ? (
            <button className="btn" onClick={() => login(p.edit!.id)}>
              <RefreshCw size={16} strokeWidth={1.75} /> Renovar login
            </button>
          ) : null}
          {oauthNew || (tool && p.edit) ? null : (
            <button
              className="btn"
              disabled={testing || !form.provider.trim() || !urlOk}
              onClick={() => void test()}
            >
              {testing ? (
                <span className="spinner" aria-label="Testando" />
              ) : (
                <Zap size={16} strokeWidth={1.75} />
              )}{" "}
              Testar
            </button>
          )}
          {oauthNew ? null : (
            <button
              className="btn btn-primary"
              disabled={!ready || saving}
              onClick={() => void save()}
            >
              {p.edit ? (
                <Save size={16} strokeWidth={1.75} />
              ) : (
                <Plug size={16} strokeWidth={1.75} />
              )}
              {p.edit ? "Salvar" : "Conectar"}
            </button>
          )}
        </footer>
      </div>
    </dialog>
  );
}

const ROLE_KIND: Record<string, string> = {
  main: "principal",
  subagent: "subagente",
  explore: "explorar",
  plan: "planejar",
};

/** A share-of-total bar with its percentage. */
function Share({ part, whole }: { part: number; whole: number }) {
  const pct = whole ? (part / whole) * 100 : 0;
  return (
    <span className="share">
      <span className="bar">
        <span style={{ width: pct + "%" }} />
      </span>
      <span className="pct">{Math.round(pct)}%</span>
    </span>
  );
}

/** "em 3 h 12 min" / "em 4 d 21 h" until the window refills. */
function until(w: QuotaWindow): string {
  const t = w.resets_at
    ? Date.parse(w.resets_at)
    : w.resets_at_epoch
      ? w.resets_at_epoch * 1000
      : NaN;
  if (Number.isNaN(t)) return "";
  const min = Math.max(0, Math.round((t - Date.now()) / 60000));
  if (min < 60) return "volta em " + min + " min";
  const h = Math.floor(min / 60);
  if (h < 48) return "volta em " + h + " h " + (min % 60) + " min";
  return "volta em " + Math.floor(h / 24) + " d " + (h % 24) + " h";
}

/** 0.2 and 99.8 keep their decimal: rounding them reads as "untouched". */
const pct = (n: number) =>
  (n > 0 && n < 1) || (n > 99 && n < 100)
    ? n.toFixed(1)
    : String(Math.round(n));

/** One limit as a bar: used share, left share, reset countdown. */
function QuotaBar({ label, w }: { label: string; w: QuotaWindow }) {
  const used = Math.min(100, Math.max(0, w.utilization));
  const tone = used >= 90 ? "danger" : used >= 70 ? "warn" : "ok";
  return (
    <div className="qbar">
      <div className="qbar-head">
        <span className="qbar-label" title={label}>
          {label}
        </span>
        <span className={"qbar-left " + tone}>{pct(100 - used)}% livre</span>
      </div>
      <div
        className="qbar-track"
        role="meter"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(used)}
        aria-label={label}
      >
        {used > 0 ? (
          <span
            className={"qbar-fill " + tone}
            style={{ width: Math.max(used, 1.5) + "%" }}
          />
        ) : null}
      </div>
      <div className="qbar-foot">
        <span>{pct(used)}% usado</span>
        <span title={w.resets_at ?? ""}>{until(w)}</span>
      </div>
    </div>
  );
}

/** 9router's usage page: every OAuth account, every limit its plan reports. */
/** The backend's wording for an account whose refresh token the provider
 * no longer accepts: only a new login fixes it, so the card offers one. */
const LOGIN_LOST = "o login desta conta expirou ou foi revogado";

export function QuotaTab(p: {
  accounts: Account[];
  quotas: Record<string, Quota | string>;
  busy: boolean;
  onRefresh: () => void;
  onLogin?: (tool: string, account: string) => void;
}) {
  const subs = p.accounts
    .filter((a) => a.enabled && isOauth(a))
    .sort((a, b) => a.priority - b.priority);
  const keys = p.accounts.filter((a) => a.enabled && !isOauth(a));
  return (
    <div className="page quota-page">
      <div className="quota-toolbar">
        <span className="quota-note">
          Lido direto do plano de cada assinatura. Contas por chave de API não
          têm cota de plano.
        </span>
        <button className="btn btn-sm" disabled={p.busy} onClick={p.onRefresh}>
          {p.busy ? (
            <span className="spinner" />
          ) : (
            <RefreshCw size={14} strokeWidth={2} />
          )}{" "}
          Atualizar
        </button>
      </div>
      {subs.length === 0 ? (
        <EmptyState
          icon={Gauge}
          title="Nenhuma assinatura conectada"
          text="Conecte Claude Code, Codex ou Antigravity em Contas."
        />
      ) : null}
      {subs.map((a) => {
        const q = p.quotas[a.id];
        const e = entryOf(a);
        const groups =
          typeof q === "object"
            ? [...new Set(q.buckets.map((b) => b.group))]
            : [];
        return (
          <Card
            key={a.id}
            title={(a.label || e?.name || a.id) + " · " + a.id}
            description={
              typeof q === "object"
                ? [q.email, q.plan].filter(Boolean).join(" · ") || "plano"
                : q
                  ? ""
                  : "lendo…"
            }
            actions={<Logo id={e?.id ?? a.oauth_tool} size={26} />}
          >
            {typeof q === "string" ? (
              <div className="quota-err">
                <CircleAlert size={14} strokeWidth={2} />{" "}
                {q.includes(LOGIN_LOST)
                  ? "O login desta conta expirou ou foi revogado no provedor. A cota volta a aparecer depois de entrar de novo."
                  : q}
                {q.includes(LOGIN_LOST) && p.onLogin && a.oauth_tool ? (
                  <button
                    className="btn btn-sm"
                    onClick={() => p.onLogin?.(a.oauth_tool as string, a.id)}
                  >
                    <LogIn size={14} strokeWidth={2} /> Entrar de novo
                  </button>
                ) : null}
              </div>
            ) : q ? (
              q.buckets.length === 0 ? (
                <div className="quota-err">
                  O plano não devolveu nenhum limite.
                </div>
              ) : (
                groups.map((g) => (
                  <div key={g} className="quota-group">
                    {groups.length > 1 ? (
                      <div className="quota-group-title">{g}</div>
                    ) : null}
                    <div className="quota-grid">
                      {q.buckets
                        .filter((b) => b.group === g)
                        .map((b) => (
                          <QuotaBar
                            key={b.label}
                            label={b.label}
                            w={b.window}
                          />
                        ))}
                    </div>
                  </div>
                ))
              )
            ) : (
              <div className="settings-loading">
                <span className="spinner" /> Consultando o plano…
              </div>
            )}
          </Card>
        );
      })}
      {keys.length ? (
        <Card
          title="Chaves de API"
          icon={KeyRound}
          description="Sem cota de plano; o limite é o que você definir na conta."
        >
          <div className="item-list">
            {keys.map((a) => (
              <div key={a.id} className="item-row">
                <Logo id={entryOf(a)?.id ?? a.provider} size={24} />
                <span className="item-text">
                  <span className="item-title">{a.label || a.id}</span>
                  <span className="item-sub mono">
                    {a.quota_tokens
                      ? "cota local " + compact(a.quota_tokens) + " tokens"
                      : "sem limite local"}
                  </span>
                </span>
              </div>
            ))}
          </div>
        </Card>
      ) : null}
    </div>
  );
}

export function UsageTab(p: { usage: UsageSummary | null; presets: Preset[] }) {
  if (!p.usage)
    return (
      <div className="page">
        <div className="settings-loading">
          <span className="spinner" /> Lendo o uso…
        </div>
      </div>
    );
  const u = p.usage;
  const total = u.accounts.reduce((s, a) => s + a.total_tokens, 0);
  const totalModel = u.models.reduce((s, m) => s + m.total_tokens, 0);
  const head = (who: string) => (
    <div className="usage-head">
      <span>{who}</span>
      <span>Participação</span>
      <span className="num-col">Tokens</span>
      <span className="num-col">Req.</span>
    </div>
  );
  return (
    <div className="page">
      <div className="stats">
        <Stat
          icon={Activity}
          value={fmt(u.total_requests)}
          label="requisições"
        />
        <Stat
          icon={Coins}
          value={fmt(u.total_prompt_tokens + u.total_completion_tokens)}
          label="tokens"
          tone="accent"
        />
        <Stat
          icon={Zap}
          value={fmt(u.total_saved_tokens)}
          label="tokens economizados"
          tone="ok"
        />
      </div>
      <p className="saver-split">
        {(
          [
            ["Saídas resumidas", "RTK", u.total_saved_rtk_tokens, false],
            ["Código enxuto", "Ponytail", u.total_saved_ponytail_tokens, true],
            ["Respostas curtas", "Caveman", u.total_saved_caveman_tokens, true],
          ] as const
        ).map(([what, tech, n, est]) => (
          <span
            key={tech}
            title={est ? "Estimativa: não há resposta sem esta opção para comparar." : "Medido: bytes cortados da saída das ferramentas."}
          >
            {what} <span className="setting-tech">({tech})</span>{" "}
            <strong>{est ? `~${fmt(n)}` : fmt(n)}</strong>
            {est ? " (estimado)" : null}
          </span>
        ))}
      </p>
      <CostCard />
      <Card
        title="Por conta"
        icon={KeyRound}
        description="Janela atual de cada conta."
      >
        {u.accounts.length === 0 ? (
          <EmptyState
            icon={ChartColumn}
            title="Sem uso nesta janela"
            text="Os números aparecem depois da primeira conversa."
          />
        ) : (
          <div className="item-list usage-table">
            {head("Conta")}
            {u.accounts.map((a) => {
              const wait = Math.ceil((a.cooldown_until - Date.now()) / 60000);
              return (
                <div key={a.account_id} className="usage-row">
                  <span className="usage-who">
                    <Logo id={a.provider} size={26} />
                    <span className="item-text">
                      <span className="item-title">
                        {a.account_id}
                        {wait > 0 ? (
                          <span
                            className="badge warn"
                            title={
                              a.last_error ?? "Limite de requisições atingido"
                            }
                          >
                            volta em {left(wait)}
                          </span>
                        ) : null}
                      </span>
                      <span className="item-sub">
                        {nameOf(p.presets, a.provider)}
                        {a.last_model ? (
                          <>
                            <span className="sep">·</span>
                            <span className="mono">{a.last_model}</span>
                          </>
                        ) : null}
                      </span>
                    </span>
                  </span>
                  <Share part={a.total_tokens} whole={total} />
                  <span className="num-col">{fmt(a.total_tokens)}</span>
                  <span className="num-col">{fmt(a.requests)}</span>
                </div>
              );
            })}
          </div>
        )}
      </Card>
      {u.models.length ? (
        <Card
          title="Por modelo"
          icon={Cpu}
          description="Quem gastou os tokens e em qual papel."
        >
          <div className="item-list usage-table">
            {head("Modelo")}
            {u.models.map((m) => (
              <div
                key={m.provider + "/" + m.model + "/" + m.role_kind}
                className="usage-row"
              >
                <span className="usage-who">
                  <Logo id={m.provider} size={26} />
                  <span className="item-text">
                    <span className="item-title mono">{m.model}</span>
                    <span className="item-sub">
                      {nameOf(p.presets, m.provider)}
                      <span className="sep">·</span>
                      {ROLE_KIND[m.role_kind] ?? m.role_kind}
                    </span>
                  </span>
                </span>
                <Share part={m.total_tokens} whole={totalModel} />
                <span className="num-col">{fmt(m.total_tokens)}</span>
                <span className="num-col">{fmt(m.requests)}</span>
              </div>
            ))}
          </div>
        </Card>
      ) : null}
    </div>
  );
}
