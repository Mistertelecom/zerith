import { useCallback, useEffect, useState, lazy, Suspense } from "react";
import {
  api,
  onOAuthUrl,
  type Account,
  type AccountForm,
  type ProbeView,
  type ProviderForm,
  type ProviderView,
  type Preset,
  type Routing,
  type UsageSummary,
  type Quota,
} from "../lib/api";
import {
  Bot,
  Brain,
  ChartColumn,
  Check,
  ChevronRight,
  Copy,
  CircleAlert,
  CircleCheck,
  Gauge,
  HardDrive,
  History,
  Image,
  KeyRound,
  LogIn,
  Pencil,
  Plug,
  Plus,
  Puzzle,
  Route,
  Save,
  Server,
  SlidersHorizontal,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { Card, EmptyState, Logo, ZerithMark, Field, ProbeResult } from "./ui";
import { nameOf, host, domain, isOauth } from "./settingsFmt";
import { GeneralTab, type GeneralProps } from "./General";
import { InlineBoundary } from "./InlineBoundary";
import { translateText, tr, useLang } from "../lib/i18n";

const AgentsTab = lazy(() => import("./Agents").then((m) => ({ default: m.AgentsTab })));
const HistoryTab = lazy(() => import("./History").then((m) => ({ default: m.HistoryTab })));
const MediaTab = lazy(() => import("./Media").then((m) => ({ default: m.MediaTab })));
const MemoryTab = lazy(() => import("./Memory").then((m) => ({ default: m.MemoryTab })));
const SkillsTab = lazy(() => import("./Skills").then((m) => ({ default: m.SkillsTab })));
const McpTab = lazy(() => import("./Mcp").then((m) => ({ default: m.McpTab })));
const RoutingTab = lazy(() => import("./Routing").then((m) => ({ default: m.RoutingTab })));
const AccountsTab = lazy(() => import("./SettingsAccounts").then((m) => ({ default: m.AccountsTab })));
const QuotaTab = lazy(() => import("./SettingsAccounts").then((m) => ({ default: m.QuotaTab })));
const UsageTab = lazy(() => import("./SettingsAccounts").then((m) => ({ default: m.UsageTab })));
const ServersTab = lazy(() => import("./Servers").then((m) => ({ default: m.ServersTab })));

const TABS = [
  "Geral",
  "Contas",
  "Cotas",
  "Delegação",
  "Memória",
  "Skills",
  "MCP",
  "Mídia",
  "Agentes",
  "Histórico",
  "Uso",
  "Servidores",
] as const;
export type SettingsTab = (typeof TABS)[number];
type Tab = SettingsTab;

const NAV: {
  group: string;
  items: { tab: Tab; icon: LucideIcon; hint: string }[];
}[] = [
  {
    group: "Aplicativo",
    items: [
      {
        tab: "Geral",
        icon: SlidersHorizontal,
        hint: "Tema, privacidade, dados e economia de tokens.",
      },
    ],
  },
  {
    group: "Modelos",
    items: [
      {
        tab: "Contas",
        icon: KeyRound,
        hint: "Assinaturas, chaves de API e servidores locais num lugar só; a ordem é o failover.",
      },
      {
        tab: "Cotas",
        icon: Gauge,
        hint: "Quanto de cada plano já foi usado e quando volta.",
      },
      {
        tab: "Delegação",
        icon: Route,
        hint: "Qual modelo cuida de cada papel do agente.",
      },
      {
        tab: "Uso",
        icon: ChartColumn,
        hint: "Tokens e requisições da janela atual.",
      },
    ],
  },
  {
    group: "Agente",
    items: [
      {
        tab: "Memória",
        icon: Brain,
        hint: "O que ele lembra de você entre conversas.",
      },
      {
        tab: "Skills",
        icon: Puzzle,
        hint: "Habilidades e plugins instalados.",
      },
      { tab: "MCP", icon: Server, hint: "Servidores MCP: ferramentas externas do agente." },
      { tab: "Agentes", icon: Bot, hint: "Subagentes especializados." },
    ],
  },
  {
    group: "Automações",
    items: [
      {
        tab: "Servidores",
        icon: HardDrive,
        hint: "Máquinas com o Zerith (zerith server) que rodam workflows 24 h.",
      },
    ],
  },
  {
    group: "Conteúdo",
    items: [
      { tab: "Mídia", icon: Image, hint: "Modelos de imagem e vídeo." },
      {
        tab: "Histórico",
        icon: History,
        hint: "Conversas salvas e importação.",
      },
    ],
  },
];


const emptyForm = (): ProviderForm => ({
  name: "",
  base_url: "",
  protocol: "openai",
  model: "",
  timeout_ms: 60000,
  key: "",
});

const formOf = (p: ProviderView): ProviderForm => ({
  name: p.name,
  base_url: p.base_url,
  protocol: p.protocol,
  model: p.default_model ?? "",
  timeout_ms: p.timeout_ms,
  key: "",
});


/** While a login waits for the browser: the link, to copy into any browser if
 * the default one did not open. The callback still lands in the app. */
function LoginLink({ url, asksCode }: { url: string; asksCode: boolean }) {
  const [copied, setCopied] = useState(false);
  const [code, setCode] = useState("");
  const [sent, setSent] = useState(false);
  return (
    <div className="login-link" role="status">
      <span className="spinner" aria-hidden />
      <div className="login-link-text">
        <strong>Aguardando o login no navegador…</strong>
        <span>
          Se o navegador não abriu, copie o link e cole em qualquer navegador.
          Expira em 5 min.
        </span>
      </div>
      <button
        className="btn btn-sm"
        onClick={() =>
          void navigator.clipboard.writeText(url).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          })
        }
      >
        {copied ? (
          <Check size={14} strokeWidth={2} />
        ) : (
          <Copy size={14} strokeWidth={2} />
        )}{" "}
        {copied ? "Copiado" : "Copiar link"}
      </button>
      {asksCode ? (
        <form
          className="login-code"
          onSubmit={(e) => {
            e.preventDefault();
            if (!code.trim()) return;
            void api.oauthCode(code.trim()).then((ok) => setSent(ok));
          }}
        >
          <input
            className="input"
            placeholder="Cole aqui o código que o Claude mostrou"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoFocus
          />
          <button
            className="btn btn-sm btn-primary"
            type="submit"
            disabled={!code.trim() || sent}
          >
            {sent ? "Enviado" : "Entrar"}
          </button>
        </form>
      ) : null}
    </div>
  );
}

function Notice({
  notice,
  onClose,
}: {
  notice: { ok: boolean; text: string } | null;
  onClose: () => void;
}) {
  if (!notice) return null;
  const Icon = notice.ok ? CircleCheck : CircleAlert;
  return (
    <div className={"toast" + (notice.ok ? "" : " bad")} role="status">
      <Icon size={16} strokeWidth={1.75} />
      <span>{notice.text}</span>
      <button className="icon-btn" onClick={onClose} aria-label="Dispensar">
        <X size={14} />
      </button>
    </div>
  );
}

export function Settings({
  onClose,
  general,
  open = "Geral",
}: {
  onClose: () => void;
  general: GeneralProps;
  /** The tab to land on; a command can open its own. */
  open?: Tab;
}) {
  const lang = useLang();
  const [tab, setTab] = useState<Tab>(open);
  useEffect(() => setTab(open), [open]);
  const [quotas, setQuotas] = useState<Record<string, Quota | string>>({});
  const [quotaBusy, setQuotaBusy] = useState(false);
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [form, setForm] = useState<ProviderForm>(emptyForm);
  const [presets, setPresets] = useState<Preset[]>([]);
  const [probe, setProbe] = useState<ProbeView | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(
    null,
  );
  // the login link while the browser round trip is pending (copy fallback)
  const [pendingUrl, setPendingUrl] = useState<string | null>(null);
  // Claude ends the login with a pasted code, so the waiting card asks for it
  const [pendingAsksCode, setPendingAsksCode] = useState(false);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [routing, setRouting] = useState<Routing>({});
  const [usage, setUsage] = useState<UsageSummary | null>(null);

  const loadProviders = useCallback(async () => {
    const list = await api.providers();
    setProviders(list);
    if (list.length) {
      setForm((prev) =>
        prev.name && list.some((p) => p.name === prev.name) ? prev : formOf(list[0]),
      );
    }
  }, []);

  useEffect(() => {
    void loadProviders().catch((e) =>
      setNotice({ ok: false, text: String(e) }),
    );
    void api
      .presets()
      .then(setPresets)
      .catch(() => undefined);
    void api
      .accounts()
      .then(setAccounts)
      .catch(() => undefined);
    void api
      .routing()
      .then(setRouting)
      .catch(() => undefined);
  }, [loadProviders]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Usage is read when its tab opens: the numbers are only meaningful fresh.
  useEffect(() => {
    if (tab !== "Uso") return;
    void api
      .usage()
      .then(setUsage)
      .catch((e) => setNotice({ ok: false, text: String(e) }));
  }, [tab]);

  const loadQuotas = useCallback(async () => {
    setQuotaBusy(true);
    const list = await api.accounts().catch(() => [] as Account[]);
    setAccounts(list);
    await Promise.all(
      list
        .filter((a) => a.enabled && isOauth(a))
        .map((a) =>
          api
            .accountQuota(a.id)
            .then((q) => setQuotas((m) => ({ ...m, [a.id]: q })))
            .catch((e) => setQuotas((m) => ({ ...m, [a.id]: String(e) }))),
        ),
    );
    setQuotaBusy(false);
  }, []);

  useEffect(() => {
    if (tab === "Cotas") void loadQuotas();
  }, [tab, loadQuotas]);

  const pick = (p: ProviderView) => {
    setForm(formOf(p));
    setProbe(null);
    setNotice(null);
  };

  const preset = (preset: Preset) => {
    setForm({
      name: preset.kind,
      base_url: preset.base_url,
      protocol: preset.protocol,
      model: "",
      timeout_ms: 60000,
      key: "",
    });
    setProbe(null);
    setNotice(null);
  };

  const save = async () => {
    setBusy(true);
    try {
      const saved = await api.providerSave(form);
      setNotice({
        ok: true,
        text: "Salvo em settings.toml; a chave vai para o cofre do sistema.",
      });
      setForm(formOf(saved));
      await loadProviders();
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setBusy(true);
    setProbe(null);
    try {
      setProbe(await api.providerTest(form));
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const saveAccount = async (a: AccountForm): Promise<boolean> => {
    try {
      setAccounts(await api.accountSave(a));
      setNotice({ ok: true, text: "Conta salva." });
      return true;
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
      return false;
    }
  };

  const loginFromCli = async (tool: string, account: string) => {
    setNotice({ ok: true, text: "Lendo a credencial que o CLI gravou…" });
    try {
      setNotice({ ok: true, text: await api.oauthImport(tool, account) });
      setAccounts(await api.accounts());
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    }
  };

  const loginFrom9router = async (tool: string, account: string) => {
    setNotice({ ok: true, text: "Copiando a conexão do 9router…" });
    try {
      setNotice({
        ok: true,
        text: await api.oauthImport9router(tool, account),
      });
      setAccounts(await api.accounts());
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    }
  };

  const loginFromPasted = async (
    tool: string,
    account: string,
    raw: string,
  ) => {
    setNotice({ ok: true, text: "Lendo a credencial colada…" });
    try {
      setNotice({
        ok: true,
        text: await api.oauthImportPasted(tool, account, raw),
      });
      setAccounts(await api.accounts());
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    }
  };

  const login = async (tool: string, account: string) => {
    setNotice({ ok: true, text: "Abrindo o navegador para o login…" });
    setPendingAsksCode(tool === "claude-code");
    const off = await onOAuthUrl(setPendingUrl).catch(() => null);
    try {
      setNotice({ ok: true, text: await api.oauthLogin(tool, account) });
      setAccounts(await api.accounts());
    } catch (e) {
      setNotice({ ok: false, text: String(e) });
    } finally {
      off?.();
      setPendingUrl(null);
      setPendingAsksCode(false);
    }
  };

  const meta = NAV.flatMap((g) => g.items).find((i) => i.tab === tab)!;
  const MetaIcon = meta.icon;

  return (
    <div
      className="settings-backdrop"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div
        className="settings"
        role="dialog"
        aria-modal="true"
        aria-label={tr(lang, "Ajustes", "Settings")}
      >
        <nav className="settings-nav" aria-label={tr(lang, "Seções dos ajustes", "Settings sections")}>
          <div className="settings-nav-title">
            <ZerithMark size={22} />
            <span>{tr(lang, "Ajustes", "Settings")}</span>
          </div>
          {NAV.map((g) => (
            <div key={g.group} className="nav-group">
              <div className="nav-group-label">
                {translateText(g.group, lang)}
              </div>
              {g.items.map(({ tab: t, icon: Icon }) => (
                <button
                  key={t}
                  className={"list-row nav-item" + (t === tab ? " on" : "")}
                  aria-current={t === tab ? "page" : undefined}
                  onClick={() => {
                    setTab(t);
                    setNotice(null);
                  }}
                >
                  <Icon size={16} strokeWidth={1.75} />
                  <span>{translateText(t, lang)}</span>
                </button>
              ))}
            </div>
          ))}
          <div className="settings-nav-foot">
            <span className="kbd">Esc</span>
            <span>{tr(lang, "para fechar", "to close")}</span>
          </div>
        </nav>
        <div className="settings-main">
          <header className="settings-head">
            <span className="head-icon">
              <MetaIcon size={18} strokeWidth={1.75} />
            </span>
            <div className="head-text">
              <h2>{translateText(tab, lang)}</h2>
              <p>{translateText(meta.hint, lang)}</p>
            </div>
            <button
              className="icon-btn"
              onClick={onClose}
              title={tr(lang, "Fechar ajustes", "Close settings")}
              aria-label={tr(lang, "Fechar ajustes", "Close settings")}
            >
              <X size={18} strokeWidth={1.75} />
            </button>
          </header>
          <div className="settings-body">
            <div className="settings-col">
              <Notice notice={notice} onClose={() => setNotice(null)} />
              <InlineBoundary key={tab}>
              <Suspense
                fallback={
                  <div className="settings-loading">
                    <span className="spinner" /> {tr(lang, "Carregando…", "Loading…")}
                  </div>
                }
              >
              {pendingUrl ? (
                <LoginLink url={pendingUrl} asksCode={pendingAsksCode} />
              ) : null}
              {tab === "Contas" ? (
                <AccountsTab
                  accounts={accounts}
                  presets={presets}
                  onSave={saveAccount}
                  onDelete={async (id) => {
                    try {
                      setAccounts(await api.accountDelete(id));
                    } catch (e) {
                      setNotice({ ok: false, text: String(e) });
                    }
                  }}
                  onLogin={login}
                  onImport={loginFromCli}
                  onImportPasted={loginFromPasted}
                  onImport9={loginFrom9router}
                />
              ) : null}
              {tab === "Contas" && providers.length > 0 ? (
                <details className="legacy-providers">
                  <summary>
                    <Server size={14} strokeWidth={1.75} /> Endpoints avulsos (
                    {providers.length}) — formato antigo
                  </summary>
                  <ProviderTab
                    providers={providers}
                    form={form}
                    presets={presets}
                    probe={probe}
                    busy={busy}
                    onPick={pick}
                    onPreset={preset}
                    onForm={setForm}
                    onSave={save}
                    onTest={test}
                  />
                </details>
              ) : null}
              {tab === "Cotas" ? (
                <QuotaTab
                  accounts={accounts}
                  quotas={quotas}
                  busy={quotaBusy}
                  onRefresh={() => void loadQuotas()}
                  onLogin={(tool, account) =>
                    void login(tool, account).then(() => loadQuotas())
                  }
                />
              ) : null}
              {tab === "Delegação" ? (
                <RoutingTab
                  routing={routing}
                  providers={providers}
                  accounts={accounts}
                  presets={presets}
                  onChange={setRouting}
                  onSave={async () => {
                    try {
                      setRouting(await api.routingSave(routing));
                      setNotice({ ok: true, text: "Delegação salva." });
                    } catch (e) {
                      setNotice({ ok: false, text: String(e) });
                    }
                  }}
                />
              ) : null}
              {tab === "Geral" ? <GeneralTab {...general} /> : null}
              {tab === "Memória" ? (
                <MemoryTab paused={general.memoryPaused} onPause={general.onMemory} />
              ) : null}
              {tab === "Skills" ? <SkillsTab /> : null}
              {tab === "MCP" ? <McpTab /> : null}
              {tab === "Mídia" ? <MediaTab /> : null}
              {tab === "Agentes" ? <AgentsTab /> : null}
              {tab === "Histórico" ? <HistoryTab /> : null}
              {tab === "Servidores" ? <ServersTab /> : null}
              {tab === "Uso" ? (
                <UsageTab usage={usage} presets={presets} />
              ) : null}
              </Suspense>
              </InlineBoundary>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}


function ProviderTab(p: {
  providers: ProviderView[];
  form: ProviderForm;
  presets: Preset[];
  probe: ProbeView | null;
  busy: boolean;
  onPick: (p: ProviderView) => void;
  onPreset: (p: Preset) => void;
  onForm: (f: ProviderForm) => void;
  onSave: () => void;
  onTest: () => void;
}) {
  const set = (patch: Partial<ProviderForm>) =>
    p.onForm({ ...p.form, ...patch });
  const saved = p.providers.some((x) => x.name === p.form.name);
  const name = (id: string) => nameOf(p.presets, id);
  return (
    <div className="page">
      <Card
        title="Conectados"
        icon={Plug}
        description="O primeiro é o padrão. Clique em um para editar."
      >
        {p.providers.length === 0 ? (
          <EmptyState
            icon={Server}
            title="Nenhum provedor ainda"
            text="Escolha um modelo pronto abaixo para começar."
          />
        ) : (
          <div className="item-list">
            {p.providers.map((item, i) => (
              <button
                key={item.name}
                className={
                  "item-row provider" + (item.name === p.form.name ? " on" : "")
                }
                aria-pressed={item.name === p.form.name}
                onClick={() => p.onPick(item)}
                title={item.name}
              >
                <Logo id={item.kind || item.name} size={30} />
                <span className="item-text">
                  <span className="item-title">
                    {name(item.name)}
                    {i === 0 ? <span className="badge">padrão</span> : null}
                  </span>
                  <span className="item-sub">
                    <span className="mono">
                      {item.default_model ?? "sem modelo padrão"}
                    </span>
                    <span className="sep">·</span>
                    {host(item.base_url)}
                  </span>
                </span>
                {item.auth === "oauth" ? (
                  <span className="badge">
                    <LogIn size={12} strokeWidth={2} />
                    assinatura
                  </span>
                ) : item.has_key ? (
                  <span className="badge ok">
                    <span className="dot ok" />
                    chave ok
                  </span>
                ) : (
                  <span className="badge warn">
                    <span className="dot warn" />
                    sem chave
                  </span>
                )}
                <ChevronRight
                  className="item-chevron"
                  size={16}
                  strokeWidth={1.75}
                />
              </button>
            ))}
          </div>
        )}
      </Card>

      <Card
        title="Adicionar provedor"
        icon={Plus}
        description="Escolha um modelo pronto: URL e protocolo vêm preenchidos, você só cola a chave."
      >
        <div className="preset-grid">
          {p.presets.map((preset) => {
            const connected = p.providers.some(
              (x) => x.kind === preset.kind || x.name === preset.kind,
            );
            return (
              <button
                key={preset.kind}
                className={
                  "preset" + (preset.kind === p.form.name ? " on" : "")
                }
                aria-pressed={preset.kind === p.form.name}
                onClick={() => p.onPreset(preset)}
                title={preset.base_url + (connected ? " · já conectado" : "")}
              >
                <Logo id={preset.kind} size={24} />
                <span className="preset-text">
                  <span className="preset-name">{preset.label}</span>
                  <span className="preset-host">{domain(preset.base_url)}</span>
                </span>
                {connected ? (
                  <Check className="preset-check" size={14} strokeWidth={2} />
                ) : null}
              </button>
            );
          })}
        </div>
      </Card>

      <Card
        title={p.form.name ? name(p.form.name) : "Novo provedor"}
        icon={saved ? Pencil : Server}
        description={saved ? "Editando um provedor salvo." : "Ainda não salvo."}
        actions={p.form.name ? <Logo id={p.form.name} size={26} /> : null}
      >
        <div className="form">
          <div className="row2">
            <Field
              label="Nome"
              value={p.form.name}
              onChange={(v) => set({ name: v })}
              placeholder="openai"
            />
            <label className="field">
              <span className="field-label">Protocolo</span>
              <select
                className="select"
                value={p.form.protocol}
                onChange={(e) => set({ protocol: e.target.value })}
              >
                <option value="openai">OpenAI (chat/completions)</option>
                <option value="anthropic">Anthropic (messages)</option>
                <option value="responses">OpenAI Responses</option>
              </select>
            </label>
          </div>
          <Field
            label="URL base"
            value={p.form.base_url}
            onChange={(v) => set({ base_url: v })}
            placeholder="https://api.exemplo.com/v1"
          />
          <div className="row2">
            <Field
              label="Modelo padrão"
              value={p.form.model ?? ""}
              onChange={(v) => set({ model: v })}
              placeholder="ex.: claude-sonnet-4"
            />
            <Field
              label="Timeout (ms)"
              type="number"
              value={String(p.form.timeout_ms)}
              onChange={(v) => set({ timeout_ms: Number(v) || 0 })}
              hint="Entre 1000 e 600000."
            />
          </div>
          <Field
            label="Chave de API"
            type="password"
            value={p.form.key ?? ""}
            onChange={(v) => set({ key: v })}
            placeholder={saved ? "Deixe vazio para manter a atual" : "sk-…"}
            hint="Vai para o cofre do sistema; o arquivo de ajustes nunca a recebe."
          />
          <ProbeResult probe={p.probe} />
          <div className="form-foot">
            {p.busy ? (
              <span className="spinner" aria-label="Trabalhando" />
            ) : null}
            <button
              className="btn"
              disabled={p.busy || !p.form.base_url}
              onClick={p.onTest}
            >
              <Zap size={16} strokeWidth={1.75} /> Testar conexão
            </button>
            <button
              className="btn btn-primary"
              disabled={p.busy || !p.form.name}
              onClick={p.onSave}
            >
              <Save size={16} strokeWidth={1.75} /> Salvar
            </button>
          </div>
        </div>
      </Card>
    </div>
  );
}
