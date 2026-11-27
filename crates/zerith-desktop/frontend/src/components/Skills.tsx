import { Fragment, useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  ArrowLeft,
  Check,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Download,
  ExternalLink,
  FileText,
  FolderInput,
  FolderOpen,
  GitBranch,
  Globe,
  Import,
  Info,
  Package,
  Plug,
  Power,
  Puzzle,
  RefreshCw,
  Scale,
  Search,
  ShieldCheck,
  Sparkles,
  Star,
  Store,
  Terminal,
  Trash2,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import {
  api,
  type Catalogue,
  type Finding,
  type HermesSkill,
  type SetupImport,
  type PluginAction,
  type PluginsView,
  type RemoteRepo,
  type RemoteSkill,
  type SkillView,
} from "../lib/api";
import { Card, EmptyState, Logo, Switch } from "./ui";

const TABS = ["Instaladas", "Catálogo", "Plugins"] as const;
type Tab = (typeof TABS)[number];

const bundled = (source: string) => source.startsWith("bundled:");

// Owners whose mark ships in assets/brand; the rest get their initial.
const OWNER_LOGO: Record<string, string> = { anthropics: "anthropic", openai: "openai", github: "github" };
const ownerLogo = (repo: string) => {
  const owner = repo.split("/")[0];
  return OWNER_LOGO[owner] ?? owner;
};

const sourceOf = (repo: string, path: string) => `github:${repo}${path ? "/" + path : ""}`;

const matches = (q: string, ...fields: string[]) => fields.some((f) => f.toLowerCase().includes(q));

const middleTruncate = (value: string, max = 42) => {
  if (value.length <= max) return value;
  const keepStart = Math.max(8, Math.floor((max - 1) * 0.38));
  const keepEnd = Math.max(10, max - 1 - keepStart);
  return `${value.slice(0, keepStart)}…${value.slice(value.length - keepEnd)}`;
};

// The body opens with the name as a heading and often the description as its
// first paragraph; the panel header already shows both.
const preview = (body: string, description: string) => {
  const rest = body.replace(/^\s*#\s[^\n]*\n+/, "");
  return rest.startsWith(description) ? rest.slice(description.length).replace(/^\s+/, "") : rest;
};

type Worst = "high" | "warn" | "note" | "none";

const worst = (s: SkillView): Worst =>
  s.findings.some((f) => f.severity === "high")
    ? "high"
    : s.findings.some((f) => f.severity === "warn")
      ? "warn"
      : s.findings.length
        ? "note"
        : "none";

const WORST: Record<Worst, [string, string]> = {
  high: ["Risco alto", " danger"],
  warn: ["Atenção", " warn"],
  note: ["Notas", ""],
  none: ["Sem alertas", " ok"],
};

const SEVERITY: Record<Finding["severity"], string> = {
  high: "Risco alto",
  warn: "Atenção",
  note: "Nota",
};

const CAPABILITY: Record<string, LucideIcon> = { shell: Terminal, fs: FolderOpen, net: Globe };

const Loading = ({ text }: { text: string }) => (
  <div className="pn-loading">
    <span className="spinner" /> {text}
  </div>
);

export function SkillsTab() {
  const [tab, setTab] = useState<Tab>("Instaladas");
  const [skills, setSkills] = useState<SkillView[] | null>(null);
  const [catalog, setCatalog] = useState<Catalogue | null>(null);
  const [catalogFailed, setCatalogFailed] = useState(false);
  const [plugins, setPlugins] = useState<PluginsView | null>(null);
  const [pluginsFailed, setPluginsFailed] = useState(false);
  const [pluginBusy, setPluginBusy] = useState<string | null>(null);
  const [marketSrc, setMarketSrc] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [from, setFrom] = useState("");
  const [showFolder, setShowFolder] = useState(false);
  const [showHermes, setShowHermes] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Catálogo: skills listed per repository, the one open, the one installing.
  const [remote, setRemote] = useState<Record<string, RemoteRepo>>({});
  const [repo, setRepo] = useState<string | null>(null);
  const [loadingRepo, setLoadingRepo] = useState<string | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  const load = useCallback(() => {
    api
      .skillsList()
      .then((list) => {
        setSkills(list);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(load, [load]);

  const loadCatalog = useCallback((force = false) => {
    setCatalogFailed(false);
    setCatalog(null);
    api
      .skillsCatalog(force)
      .then((c) => {
        setCatalog(c);
        setCatalogFailed(false);
        setError(null);
      })
      .catch((e) => {
        setCatalogFailed(true);
        setError(String(e));
      });
  }, []);

  const loadPlugins = useCallback(() => {
    setPluginsFailed(false);
    setPlugins(null);
    api
      .skillsPlugins()
      .then((p) => {
        setPlugins(p);
        setPluginsFailed(false);
        setError(null);
      })
      .catch((e) => {
        setPluginsFailed(true);
        setError(String(e));
      });
  }, []);

  useEffect(() => {
    if (tab === "Catálogo" && !catalog && !catalogFailed) {
      loadCatalog();
    }
    if (tab === "Plugins" && !plugins && !pluginsFailed) {
      loadPlugins();
    }
  }, [tab, catalog, catalogFailed, plugins, pluginsFailed, loadCatalog, loadPlugins]);

  const act = async (job: () => Promise<SkillView[]>) => {
    setBusy(true);
    try {
      setSkills(await job());
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const openRepo = (name: string, force = false) => {
    setRepo(name);
    setQuery("");
    if (remote[name] && !force) return;
    setLoadingRepo(name);
    api
      .skillsRemote(name, force)
      .then((r) => {
        setRemote((m) => ({ ...m, [name]: r }));
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoadingRepo(null));
  };

  const plugin = async (action: PluginAction, target: string, done?: string): Promise<boolean> => {
    setPluginBusy(action + ":" + target);
    setNotice(null);
    try {
      setPlugins(await api.pluginAction(action, target));
      setError(null);
      if (done) setNotice(done);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      setPluginBusy(null);
    }
  };

  const install = async (r: RemoteRepo, s: RemoteSkill) => {
    setInstalling(sourceOf(r.repo, s.path));
    setNotice(null);
    try {
      setSkills(await api.skillInstallRemote(r.repo, r.branch, s.path));
      setError(null);
      setNotice(`${s.name} instalada e ativa. Ela aparece em “Instaladas”.`);
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(null);
    }
  };

  const defaults = skills?.filter((s) => bundled(s.installed.source)) ?? [];
  const mine = skills?.filter((s) => !bundled(s.installed.source)) ?? [];
  const ordered = [...defaults, ...mine];
  const current = ordered.find((s) => s.installed.name === selected) ?? ordered[0] ?? null;
  const offDefaults = defaults.filter((s) => s.installed.status !== "enabled");
  const counts: Record<Tab, number | undefined> = {
    Instaladas: skills?.length,
    Catálogo: catalog?.entries.length,
    Plugins: plugins?.installed.length,
  };

  const viewFor = (r: RemoteRepo, s: RemoteSkill) =>
    skills?.find((v) => v.installed.source === sourceOf(r.repo, s.path) || v.installed.name === s.name);

  const tile = (r: RemoteRepo, s: RemoteSkill, sub: string) => (
    <SkillTile
      key={r.repo + ":" + s.path}
      skill={s}
      sub={sub}
      view={viewFor(r, s)}
      installing={installing === sourceOf(r.repo, s.path)}
      disabled={installing !== null || busy}
      onInstall={() => void install(r, s)}
      onEnable={(name) => void act(() => api.skillToggle(name, true))}
    />
  );

  const q = query.trim().toLowerCase();
  const open = repo ? catalog?.entries.find((e) => e.repo === repo) : undefined;
  const openList = repo ? remote[repo] : undefined;

  return (
    <div className="page pn">
      <div className="pn-toolbar">
        <div className="segmented" role="group" aria-label="Seção de skills">
          {TABS.map((t) => (
            <button
              key={t}
              className={"seg" + (t === tab ? " on" : "")}
              aria-pressed={t === tab}
              onClick={() => setTab(t)}
            >
              {t}
              {counts[t] !== undefined ? <span className="pn-count">{counts[t]}</span> : null}
            </button>
          ))}
        </div>
        <span className="pn-spacer" />
        {tab === "Instaladas" ? (
          <>
            <button className="btn btn-sm" aria-expanded={showHermes} onClick={() => setShowHermes((v) => !v)}>
              <Import size={14} strokeWidth={2} /> Importar do Hermes
            </button>
            <button className="btn btn-sm" aria-expanded={showFolder} onClick={() => setShowFolder((v) => !v)}>
              <FolderInput size={14} strokeWidth={2} /> Instalar de uma pasta…
            </button>
          </>
        ) : null}
      </div>
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
          <button className="icon-btn" aria-label="Fechar aviso" onClick={() => setError(null)}>
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      ) : null}
      {notice ? (
        <div className="toast" role="status">
          <CircleCheck size={16} strokeWidth={1.75} />
          <span>{notice}</span>
          <button className="icon-btn" aria-label="Fechar aviso" onClick={() => setNotice(null)}>
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      ) : null}

      {tab === "Instaladas" ? (
        <>
          {showHermes ? <HermesSkills onImported={setSkills} onClose={() => setShowHermes(false)} /> : null}
          {showFolder ? (
            <Card title="Instalar de uma pasta" icon={FolderInput} description="Aponte para a pasta que contém o SKILL.md.">
              <div className="pn-inline">
                <label className="pn-affix pn-spacer">
                  <span className="pn-affix-icon">
                    <FolderOpen size={16} strokeWidth={1.75} />
                  </span>
                  <input
                    className="input mono"
                    value={from}
                    placeholder="C:/caminho/da/skill"
                    aria-label="Pasta da skill"
                    onChange={(e) => setFrom(e.target.value)}
                  />
                </label>
                <button
                  className="btn btn-primary"
                  disabled={busy || !from.trim()}
                  onClick={() =>
                    void act(async () => {
                      const list = await api.skillInstall(from);
                      setFrom("");
                      return list;
                    })
                  }
                >
                  <Download size={16} strokeWidth={1.75} /> Instalar
                </button>
              </div>
            </Card>
          ) : null}

          {defaults.length ? (
            <section className="card pn-banner">
              <span className="pn-ico accent lg">
                <Sparkles size={18} strokeWidth={1.75} />
              </span>
              <span className="pn-body">
                <span className="pn-title">Skills padrão do Zerith</span>
                <span className="pn-hint">
                  {defaults.length} já vêm instaladas com o app · {defaults.length - offDefaults.length}{" "}
                  {defaults.length - offDefaults.length === 1 ? "ativa" : "ativas"}.
                  {offDefaults.length ? " Ative as que quiser usar; nenhuma roda sem estar ativa." : ""}
                </span>
              </span>
              {offDefaults.length ? (
                <button
                  className="btn btn-primary btn-sm"
                  disabled={busy}
                  onClick={() =>
                    void act(async () => {
                      let list = skills ?? [];
                      for (const s of offDefaults) list = await api.skillToggle(s.installed.name, true);
                      return list;
                    })
                  }
                >
                  <Power size={14} strokeWidth={2} /> Ativar todas
                </button>
              ) : (
                <span className="badge ok">
                  <Check size={12} strokeWidth={2} /> Todas ativas
                </span>
              )}
            </section>
          ) : null}

          {skills === null ? <Loading text="Lendo as skills…" /> : null}
          {skills?.length === 0 ? (
            <div className="card">
              <EmptyState icon={Puzzle} title="Nenhuma skill" text="Instale pelo Catálogo com um clique.">
                <button className="btn btn-primary btn-sm" onClick={() => setTab("Catálogo")}>
                  <Package size={14} strokeWidth={2} /> Abrir catálogo
                </button>
              </EmptyState>
            </div>
          ) : null}
          {current ? (
            <section className="card pn-split wide-list">
              <nav className="pn-split-list" aria-label="Skills instaladas">
                {(
                  [
                    ["Padrão do Zerith", defaults],
                    ["Instaladas por você", mine],
                  ] as const
                ).map(([label, list]) =>
                  list.length ? (
                    <Fragment key={label}>
                      <div className="pn-group-label">
                        {label} <span className="pn-count">{list.length}</span>
                      </div>
                      {list.map((s) => {
                        const name = s.installed.name;
                        const on = s.installed.status === "enabled";
                        const w = worst(s);
                        const here = current.installed.name === name;
                        return (
                          <div key={name} className={"list-row pn-skill-row" + (here ? " on" : "") + (on ? "" : " dim")}>
                            <button
                              className="pn-row-main"
                              aria-current={here ? "true" : undefined}
                              onClick={() => setSelected(name)}
                            >
                              <span className="pn-ico">
                                <Puzzle size={16} strokeWidth={1.75} />
                              </span>
                              <span className="pn-body">
                                <span className="pn-title">{name}</span>
                                <span className="pn-sub">{s.installed.description}</span>
                              </span>
                              {w === "high" || w === "warn" ? (
                                <span title={WORST[w][0]} className={"pn-sev" + (w === "high" ? " danger" : "")}>
                                  <AlertTriangle size={14} strokeWidth={2} aria-label={WORST[w][0]} />
                                </span>
                              ) : null}
                            </button>
                            <Switch
                              label={`Ativar ${name}`}
                              checked={on}
                              disabled={busy}
                              onChange={(v) => void act(() => api.skillToggle(name, v))}
                            />
                          </div>
                        );
                      })}
                    </Fragment>
                  ) : null,
                )}
              </nav>
              <SkillDetail
                skill={current}
                busy={busy}
                onToggle={(v) => void act(() => api.skillToggle(current.installed.name, v))}
                onRemove={() => {
                  if (!window.confirm(`Remover a skill ${current.installed.name}?`)) return;
                  void act(async () => {
                    const list = await api.skillRemove(current.installed.name);
                    setSelected(null);
                    return list;
                  });
                }}
              />
            </section>
          ) : null}
        </>
      ) : null}

      {tab === "Catálogo" ? (
        <>
          <div className="pn-toolbar">
            {repo ? (
              <button className="btn btn-sm btn-ghost" onClick={() => setRepo(null)}>
                <ArrowLeft size={14} strokeWidth={2} /> Repositórios
              </button>
            ) : null}
            <label className="pn-affix">
              <span className="pn-affix-icon">
                <Search size={16} strokeWidth={1.75} />
              </span>
              <input
                className="input"
                type="search"
                value={query}
                placeholder={repo ? `Buscar skill em ${repo}` : "Buscar repositório ou skill"}
                aria-label="Buscar skills"
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
          </div>

          {repo ? (
            <>
              <section className="card pn-repo-head">
                <Logo id={ownerLogo(repo)} size={40} />
                <div className="pn-body">
                  <h3 className="pn-h mono">{repo}</h3>
                  {open?.description ? <p className="pn-hint">{open.description}</p> : null}
                  <div className="pn-inline wrap">
                    {open ? (
                      <span className="badge" title="Estrelas no GitHub (popularidade, não certificação)">
                        <Star size={12} strokeWidth={2} /> {open.stars.toLocaleString("pt-BR")}
                      </span>
                    ) : null}
                    {open ? (
                      <span className="badge" title="Licença">
                        <Scale size={12} strokeWidth={2} /> {open.license === "unknown" ? "Licença não informada" : open.license}
                      </span>
                    ) : null}
                    {openList ? (
                      <span className="badge mono" title="Branch">
                        <GitBranch size={12} strokeWidth={2} /> {openList.branch}
                      </span>
                    ) : null}
                    {openList ? (
                      <span className="badge">
                        <Puzzle size={12} strokeWidth={2} /> {openList.skills.length}{" "}
                        {openList.skills.length === 1 ? "skill" : "skills"}
                      </span>
                    ) : null}
                  </div>
                </div>
                <div className="pn-actions">
                  <button
                    className="icon-btn"
                    title="Atualizar a lista"
                    aria-label="Atualizar a lista"
                    disabled={loadingRepo !== null}
                    onClick={() => openRepo(repo, true)}
                  >
                    <RefreshCw size={16} strokeWidth={1.75} />
                  </button>
                  <a
                    className="btn btn-sm pn-link"
                    href={open?.url ?? `https://github.com/${repo}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    <ExternalLink size={14} strokeWidth={2} /> Abrir no GitHub
                  </a>
                </div>
              </section>
              {loadingRepo === repo ? <Loading text="Procurando skills no repositório…" /> : null}
              {openList && loadingRepo !== repo ? (
                (() => {
                  const list = openList.skills.filter((s) => !q || matches(q, s.name, s.description, s.path));
                  return list.length ? (
                    <div className="pn-grid">{list.map((s) => tile(openList, s, s.path || "raiz do repositório"))}</div>
                  ) : (
                    <div className="card">
                      <EmptyState
                        icon={Puzzle}
                        title={q ? "Nada encontrado" : "Nenhuma skill aqui"}
                        text={q ? "Tente outro termo." : "Este repositório não tem nenhum SKILL.md."}
                      />
                    </div>
                  );
                })()
              ) : null}
            </>
          ) : (
            <>
              {(() => {
                const hits = q
                  ? Object.values(remote).flatMap((r) =>
                      r.skills.filter((s) => matches(q, s.name, s.description)).map((s) => tile(r, s, r.repo)),
                    )
                  : [];
                return hits.length ? (
                  <>
                    <div className="pn-group-label">
                      Skills encontradas <span className="pn-count">{hits.length}</span>
                    </div>
                    <div className="pn-grid">{hits}</div>
                  </>
                ) : null;
              })()}
              <div className="pn-group-label">Repositórios sugeridos</div>
              <p className="pn-hint pn-inline">
                <Info size={14} strokeWidth={2} />
                Abra um repositório para ver as skills dele e instalar com um clique — sem clonar nada.
              </p>
              {catalog === null && !catalogFailed ? <Loading text="Lendo o catálogo…" /> : null}
              {catalog === null && catalogFailed ? (
                <div className="card">
                  <EmptyState
                    icon={Package}
                    title="Catálogo indisponível"
                    text="Não consegui falar com o GitHub agora. Tente de novo em alguns minutos."
                  >
                    <button className="btn btn-sm" onClick={() => loadCatalog(true)}>
                      <RefreshCw size={14} strokeWidth={2} /> Tentar de novo
                    </button>
                  </EmptyState>
                </div>
              ) : null}
              {catalog?.entries.length === 0 ? (
                <div className="card">
                  <EmptyState
                    icon={Package}
                    title="Catálogo indisponível"
                    text="Não consegui falar com o GitHub agora. Tente de novo em alguns minutos."
                  />
                </div>
              ) : null}
              <div className="pn-grid">
                {catalog?.entries
                  .filter((e) => !q || matches(q, e.repo, e.description))
                  .map((e) => {
                    const loaded = remote[e.repo];
                    return (
                      <button
                        key={e.repo}
                        className="pn-tile"
                        aria-label={`Abrir ${e.repo}`}
                        onClick={() => openRepo(e.repo)}
                      >
                        <div className="pn-tile-head">
                          <Logo id={ownerLogo(e.repo)} size={32} />
                          <span className="pn-body">
                            <span className="pn-title mono">{e.repo.split("/")[1]}</span>
                            <span className="pn-sub">{e.repo.split("/")[0]}</span>
                          </span>
                        </div>
                        <p className="pn-desc" title={e.description}>
                          {e.description || "Sem descrição."}
                        </p>
                        <div className="pn-tile-foot">
                          <span className="badge" title="Estrelas no GitHub">
                            <Star size={12} strokeWidth={2} /> {e.stars.toLocaleString("pt-BR")}
                          </span>
                          {e.license && e.license !== "unknown" ? (
                            <span className="badge" title="Licença">
                              <Scale size={12} strokeWidth={2} /> {e.license}
                            </span>
                          ) : null}
                          {loaded ? (
                            <span className="badge">
                              <Puzzle size={12} strokeWidth={2} /> {loaded.skills.length}
                            </span>
                          ) : null}
                          <span className="pn-spacer" />
                          <span className="pn-tile-cta">
                            Ver skills <ChevronRight size={14} strokeWidth={2} />
                          </span>
                        </div>
                      </button>
                    );
                  })}
              </div>
            </>
          )}
        </>
      ) : null}

      {tab === "Plugins" ? (
        <>
          <Card
            title="Loja de plugins"
            icon={Store}
            description="Pacotes no formato do Claude Code. Depois de instalado, a IA usa as skills e os comandos dele sozinha quando o assunto aparece."
          >
            {plugins === null && !pluginsFailed ? <Loading text="Lendo as lojas…" /> : null}
            {plugins === null && pluginsFailed ? (
              <EmptyState
                icon={Store}
                title="Não consegui carregar os plugins"
                text="Falha ao ler as lojas e plugins configurados."
              >
                <button className="btn btn-sm" onClick={loadPlugins}>
                  <RefreshCw size={14} strokeWidth={2} /> Tentar de novo
                </button>
              </EmptyState>
            ) : null}
            {plugins?.offers.length ? (
              <div className="pn-list">
                {plugins.offers.map((o) => {
                  const spec = `${o.name}@${o.market}`;
                  const installed = plugins.installed.find((p) => p.name === o.name);
                  const hasUpdate = Boolean(
                    installed && o.version && installed.version && o.version !== installed.version,
                  );
                  const working = pluginBusy === "install:" + spec;
                  const updating = pluginBusy === "update:" + spec;
                  return (
                    <div key={spec} className="pn-item">
                      <span className="pn-ico">
                        <Package size={16} strokeWidth={1.75} />
                      </span>
                      <span className="pn-body">
                        <span className="pn-title">
                          {o.name} <span className="pn-num pn-hint">{spec}</span>
                        </span>
                        <span className="pn-sub">{o.description}</span>
                      </span>
                      <span className="pn-meta">
                        {o.category ? <span className="badge">{o.category}</span> : null}
                        {installed ? (
                          hasUpdate ? (
                            <button
                              className="btn btn-sm"
                              disabled={pluginBusy !== null}
                              title={`Atualizar de v${installed.version} para v${o.version}`}
                              onClick={() =>
                                void plugin(
                                  "update",
                                  spec,
                                  `${o.name} atualizado para v${o.version}.`,
                                )
                              }
                            >
                              {updating ? <span className="spinner" /> : <RefreshCw size={14} strokeWidth={2} />}
                              {updating ? "Atualizando…" : `Atualizar (v${o.version})`}
                            </button>
                          ) : (
                            <span className="badge ok">
                              <Check size={12} strokeWidth={2} /> Instalado
                            </span>
                          )
                        ) : (
                          <button
                            className="btn btn-sm btn-primary"
                            disabled={pluginBusy !== null}
                            onClick={() =>
                              void plugin("install", spec, `${o.name} instalado e ativo: a IA já pode usá-lo na próxima mensagem.`)
                            }
                          >
                            {working ? <span className="spinner" /> : <Download size={14} strokeWidth={2} />}
                            {working ? "Baixando…" : "Instalar"}
                          </button>
                        )}
                      </span>
                    </div>
                  );
                })}
              </div>
            ) : null}
            <div className="pn-inline pn-gap-top">
              <label className="pn-affix pn-spacer">
                <span className="pn-affix-icon">
                  <GitBranch size={16} strokeWidth={1.75} />
                </span>
                <input
                  className="input mono"
                  value={marketSrc}
                  placeholder="dono/repo de outra loja (ex.: elementalsouls/Claude-BugHunter)"
                  aria-label="Adicionar loja"
                  onChange={(e) => setMarketSrc(e.target.value)}
                />
              </label>
              <button
                className="btn"
                disabled={pluginBusy !== null || !marketSrc.trim()}
                onClick={() =>
                  void plugin("market-add", marketSrc.trim(), "Loja adicionada: os plugins dela aparecem na lista acima.").then(
                    (ok) => ok && setMarketSrc(""),
                  )
                }
              >
                <Store size={14} strokeWidth={2} /> Adicionar loja
              </button>
            </div>
          </Card>

          <Card
            title="Instalados"
            icon={Plug}
            description={
              plugins
                ? `${plugins.installed.length} ${plugins.installed.length === 1 ? "plugin instalado" : "plugins instalados"}`
                : undefined
            }
          >
            {plugins?.installed.length === 0 ? (
              <EmptyState icon={Plug} title="Nenhum plugin" text="Instale um da loja acima." />
            ) : null}
            {plugins?.installed.length ? (
              <div className="pn-list">
                {plugins.installed.map((p) => {
                  const offer = plugins.offers.find((o) => o.name === p.name);
                  const spec = offer ? `${offer.name}@${offer.market}` : p.name;
                  const hasUpdate = Boolean(
                    offer && offer.version && p.version && offer.version !== p.version,
                  );
                  const updating = pluginBusy === "update:" + spec;
                  return (
                    <div key={p.name} className={"pn-item" + (p.enabled ? "" : " dim")}>
                      <span className="pn-ico">
                        <Plug size={16} strokeWidth={1.75} />
                      </span>
                      <span className="pn-body">
                        <span className="pn-title">
                          {p.name} {p.version && p.version !== "-" ? <span className="pn-num pn-hint">v{p.version}</span> : null}
                        </span>
                        <span className="pn-sub" title={p.description}>
                          {p.skills} {p.skills === 1 ? "skill" : "skills"}
                          {p.agents ? ` · ${p.agents} ${p.agents === 1 ? "agente" : "agentes"}` : ""}
                          {p.runsCode ? (p.trusted ? " · programas liberados" : " · programas bloqueados") : ""}
                          {" · "}
                          {p.description}
                        </span>
                      </span>
                      <span className="pn-meta">
                        {p.capabilities.map((c) => {
                          const Icon = CAPABILITY[c] ?? Wrench;
                          return (
                            <span key={c} className="badge mono" title="Capacidade">
                              <Icon size={12} strokeWidth={2} /> {c}
                            </span>
                          );
                        })}
                        {offer ? (
                          hasUpdate ? (
                            <button
                              className="btn btn-sm"
                              disabled={pluginBusy !== null}
                              title={`Atualizar ${p.name} de v${p.version} para v${offer.version}`}
                              onClick={() =>
                                void plugin(
                                  "update",
                                  spec,
                                  `${p.name} atualizado para v${offer.version}.`,
                                )
                              }
                            >
                              {updating ? <span className="spinner" /> : <RefreshCw size={14} strokeWidth={2} />}
                              {updating ? "Atualizando…" : `Atualizar (v${offer.version})`}
                            </button>
                          ) : (
                            <button
                              className="btn-icon"
                              aria-label={`Atualizar ${p.name}`}
                              title={`Baixar a versão mais recente de ${p.name} (${spec})`}
                              disabled={pluginBusy !== null}
                              onClick={() =>
                                void plugin(
                                  "update",
                                  spec,
                                  `${p.name} atualizado a partir da loja ${offer.market}.`,
                                )
                              }
                            >
                              {updating ? <span className="spinner" /> : <RefreshCw size={14} strokeWidth={2} />}
                            </button>
                          )
                        ) : null}
                        {p.runsCode ? (
                          <button
                            className="btn-icon"
                            disabled={pluginBusy !== null}
                            aria-label={p.trusted ? `Bloquear programas de ${p.name}` : `Liberar programas de ${p.name}`}
                            title={
                              p.trusted
                                ? "Servidores MCP/hooks dele estão ligados. Clique para bloquear."
                                : "Ele também roda programas (MCP/hooks). Revise a pasta dele antes de liberar."
                            }
                            onClick={() => void plugin(p.trusted ? "untrust" : "trust", p.name)}
                          >
                            {p.trusted ? (
                              <ShieldCheck size={14} strokeWidth={2} />
                            ) : (
                              <AlertTriangle size={14} strokeWidth={2} className="pn-warn" />
                            )}
                          </button>
                        ) : null}
                        <Switch
                          checked={p.enabled}
                          label={p.enabled ? `Desligar ${p.name}` : `Ligar ${p.name}`}
                          disabled={pluginBusy !== null}
                          onChange={(on) => void plugin(on ? "enable" : "disable", p.name)}
                        />
                        <button
                          className="btn-icon"
                          aria-label={`Remover ${p.name}`}
                          title="Remover"
                          disabled={pluginBusy !== null}
                          onClick={() => {
                            if (window.confirm(`Remover o plugin ${p.name}?`))
                              void plugin("remove", p.name, `${p.name} removido.`);
                          }}
                        >
                          <Trash2 size={14} strokeWidth={2} />
                        </button>
                      </span>
                    </div>
                  );
                })}
              </div>
            ) : null}
          </Card>
        </>
      ) : null}
    </div>
  );
}

/** "N importadas, M já existiam, K com erro" */
export const importLine = (r: SetupImport) =>
  `${r.imported.length} ${r.imported.length === 1 ? "importada" : "importadas"}, ${r.existed.length} já ${
    r.existed.length === 1 ? "existia" : "existiam"
  }, ${r.failed.length} com erro`;

/** The Hermes skills folder, grouped by category, picked with checkboxes. */
function HermesSkills(props: { onImported: (list: SkillView[]) => void; onClose: () => void }) {
  const [list, setList] = useState<HermesSkill[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<SetupImport | null>(null);

  const load = useCallback(() => {
    api
      .hermesSkills()
      .then((l) => {
        setList(l);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);
  useEffect(load, [load]);

  const q = query.trim().toLowerCase();
  const shown = (list ?? []).filter((s) => !q || matches(q, s.name, s.category, s.description));
  const pickable = shown.filter((s) => !s.installed && !s.error);
  const all = pickable.length > 0 && pickable.every((s) => picked.has(s.name));
  const groups = [...new Set(shown.map((s) => s.category))];
  const flip = (name: string, on: boolean) =>
    setPicked((p) => {
      const n = new Set(p);
      if (on) n.add(name);
      else n.delete(name);
      return n;
    });

  const run = async () => {
    setBusy(true);
    try {
      const r = await api.hermesSkillsImport([...picked]);
      setResult(r);
      props.onImported(r.skills);
      setPicked(new Set());
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card
      title="Importar do Hermes"
      icon={Import}
      description={
        list
          ? `${list.length} skills no Hermes · ${(n => `${n} ${n === 1 ? "já está" : "já estão"}`)(list.filter((s) => s.installed).length)} aqui. Os arquivos são copiados (references, scripts e templates juntos) e chegam ativos.`
          : "Lendo a pasta de skills do Hermes…"
      }
      actions={
        <button className="icon-btn" aria-label="Fechar importação" title="Fechar" onClick={props.onClose}>
          <X size={16} strokeWidth={1.75} />
        </button>
      }
    >
      {error ? (
        <div className="pn-callout danger">
          <CircleAlert size={14} strokeWidth={2} />
          <span>{error}</span>
        </div>
      ) : null}
      {list === null && !error ? <Loading text="Procurando skills do Hermes…" /> : null}
      {list?.length ? (
        <>
          <div className="pn-toolbar">
            <label className="pn-affix">
              <span className="pn-affix-icon">
                <Search size={16} strokeWidth={1.75} />
              </span>
              <input
                className="input"
                type="search"
                value={query}
                placeholder="Buscar por nome, categoria ou descrição"
                aria-label="Buscar skills do Hermes"
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={all}
                disabled={!pickable.length}
                onChange={(e) => pickable.forEach((s) => flip(s.name, e.target.checked))}
              />
              Selecionar todos
            </label>
            <button className="btn btn-primary btn-sm" disabled={busy || picked.size === 0} onClick={() => void run()}>
              {busy ? <span className="spinner" /> : <Download size={14} strokeWidth={2} />}
              {busy ? "Importando…" : `Importar ${picked.size || ""}`.trim()}
            </button>
          </div>
          {result ? (
            <div className={"pn-callout" + (result.failed.length ? " danger" : " note")} role="status">
              {result.failed.length ? <AlertTriangle size={14} strokeWidth={2} /> : <CircleCheck size={14} strokeWidth={2} />}
              <span>
                <strong>{importLine(result)}</strong>
                {result.failed.length ? " · " + result.failed.join(" · ") : ""}
              </span>
            </div>
          ) : null}
          <div className="pn-scroll pn-hermes-list">
            {groups.map((g) => {
              const rows = shown.filter((s) => s.category === g);
              return (
                <Fragment key={g || "-"}>
                  <div className="pn-group-label">
                    <FolderOpen size={12} strokeWidth={2} /> {g || "Sem categoria"} <span className="pn-count">{rows.length}</span>
                  </div>
                  {rows.map((s) => (
                    <label key={g + "/" + s.name} className={"pn-item pn-hermes-row" + (s.installed || s.error ? " dim" : "")}>
                      <input
                        type="checkbox"
                        checked={s.installed || picked.has(s.name)}
                        disabled={s.installed || !!s.error}
                        aria-label={`Importar ${s.name}`}
                        onChange={(e) => flip(s.name, e.target.checked)}
                      />
                      <span className="pn-body">
                        <span className="pn-title">{s.name}</span>
                        <span className="pn-sub" title={s.error ?? s.description}>
                          {s.error ?? s.description}
                        </span>
                      </span>
                      {s.installed ? (
                        <span className="badge">
                          <Check size={12} strokeWidth={2} /> Já existe
                        </span>
                      ) : s.error ? (
                        <span className="badge danger">
                          <AlertTriangle size={12} strokeWidth={2} /> Inválida
                        </span>
                      ) : null}
                    </label>
                  ))}
                </Fragment>
              );
            })}
            {!shown.length ? <EmptyState icon={Search} title="Nada encontrado" text="Tente outro termo." /> : null}
          </div>
        </>
      ) : null}
      {list?.length === 0 ? (
        <EmptyState icon={Puzzle} title="Nenhuma skill no Hermes" text="A pasta skills do Hermes está vazia." />
      ) : null}
    </Card>
  );
}

/** One skill inside a GitHub repository: install it, or say it is already here. */
function SkillTile(props: {
  skill: RemoteSkill;
  sub: string;
  view: SkillView | undefined;
  installing: boolean;
  disabled: boolean;
  onInstall: () => void;
  onEnable: (name: string) => void;
}) {
  const { skill: s, view } = props;
  const on = view?.installed.status === "enabled";
  const alerts = view?.findings.filter((f) => f.severity !== "note") ?? [];
  return (
    <div className={"pn-tile" + (view ? " installed" : "")}>
      <div className="pn-tile-head">
        <span className={"pn-ico" + (view ? " accent" : "")}>
          <Puzzle size={16} strokeWidth={1.75} />
        </span>
        <span className="pn-body">
          <span className="pn-title">{s.name}</span>
          <span className="pn-sub mono" title={props.sub}>
            {props.sub}
          </span>
        </span>
      </div>
      <p className="pn-desc" title={s.description}>
        {s.description || "Sem descrição."}
      </p>
      {alerts.map((f, i) => (
        <div key={i} className={"pn-callout" + (f.severity === "high" ? " danger" : "")}>
          <AlertTriangle size={14} strokeWidth={2} />
          <span>{f.message}</span>
        </div>
      ))}
      <div className="pn-tile-foot">
        {view ? (
          <>
            {bundled(view.installed.source) ? (
              <span className="badge" title="Já vem com o Zerith">
                <Sparkles size={12} strokeWidth={2} /> Padrão
              </span>
            ) : (
              <span className="badge">
                <Check size={12} strokeWidth={2} /> Instalada
              </span>
            )}
            <span className="pn-spacer" />
            {on ? (
              <span className="badge ok">
                <span className="dot ok" /> Ativa
              </span>
            ) : (
              <button className="btn btn-sm" disabled={props.disabled} onClick={() => props.onEnable(view.installed.name)}>
                <Power size={14} strokeWidth={2} /> Ativar
              </button>
            )}
          </>
        ) : (
          <>
            <span className="pn-spacer" />
            <button className="btn btn-primary btn-sm" disabled={props.disabled} onClick={props.onInstall}>
              {props.installing ? <span className="spinner" /> : <Download size={14} strokeWidth={2} />}
              {props.installing ? "Instalando…" : "Instalar"}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function SkillDetail(props: {
  skill: SkillView;
  busy: boolean;
  onToggle: (v: boolean) => void;
  onRemove: () => void;
}) {
  const { skill: s, busy } = props;
  const on = s.installed.status === "enabled";
  const w = worst(s);
  const [label, tone] = WORST[w];
  return (
    <div className="pn-split-detail">
      <div className="pn-detail-head">
        <div className="pn-body">
          <h3 className="pn-h">{s.installed.name}</h3>
          <p className="pn-hint">{s.installed.description}</p>
        </div>
        <label className="pn-inline">
          <span className="pn-hint">{on ? "Ativa" : "Desligada"}</span>
          <Switch label={`Ativar ${s.installed.name}`} checked={on} disabled={busy} onChange={props.onToggle} />
        </label>
      </div>

      {w !== "none" || bundled(s.installed.source) || s.scripts.length ? (
        <div className="pn-inline wrap">
          {w === "none" ? null : (
            <span className={"badge" + tone}>
              {w === "note" ? <Info size={12} strokeWidth={2} /> : <AlertTriangle size={12} strokeWidth={2} />}
              {label}
            </span>
          )}
          {bundled(s.installed.source) ? (
            <span className="badge">
              <Sparkles size={12} strokeWidth={2} /> Padrão do Zerith
            </span>
          ) : null}
          {s.scripts.map((script) => (
            <span key={script} className="badge mono" title="Script incluído">
              <Terminal size={12} strokeWidth={2} /> {script}
            </span>
          ))}
        </div>
      ) : null}

      {s.findings.map((f, i) => (
        <div
          key={i}
          className={"pn-callout" + (f.severity === "high" ? " danger" : f.severity === "note" ? " note" : "")}
        >
          {f.severity === "note" ? <Info size={14} strokeWidth={2} /> : <AlertTriangle size={14} strokeWidth={2} />}
          <span>
            <strong>{SEVERITY[f.severity]}</strong> · {f.message}
          </span>
        </div>
      ))}

      <dl className="pn-dl">
        <dt>Origem</dt>
        <dd>
          <span className="mono-path" title={s.installed.source}>
            {middleTruncate(s.installed.source, 42)}
          </span>
        </dd>
        <dt>Pasta</dt>
        <dd>
          <span className="mono-path" title={s.installed.dir}>
            {middleTruncate(s.installed.dir, 42)}
          </span>
        </dd>
        <dt>Licença</dt>
        <dd>{s.installed.license || "—"}</dd>
      </dl>

      <div className="pn-code">
        <div className="pn-code-head">
          <FileText size={12} strokeWidth={2} /> SKILL.md
        </div>
        <pre>
          {/* Source, not rendered: headings only get weight so the file reads deliberately. */}
          {preview(s.body, s.installed.description)
            .slice(0, 4000)
            .split("\n")
            .map((line, i) => {
              const h = /^(#{1,6})\s(.*)$/.exec(line);
              return h ? (
                <span key={i} className="pn-md-h">
                  <span className="pn-md-mark">{h[1]} </span>
                  {h[2] + "\n"}
                </span>
              ) : (
                <span key={i}>{line + "\n"}</span>
              );
            })}
        </pre>
      </div>

      <div className="pn-detail-foot">
        <button className="btn btn-danger btn-sm" disabled={busy} onClick={props.onRemove}>
          <Trash2 size={14} strokeWidth={2} /> Remover
        </button>
      </div>
    </div>
  );
}
