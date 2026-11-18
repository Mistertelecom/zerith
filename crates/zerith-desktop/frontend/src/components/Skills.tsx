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
  type PluginView,
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
  const [plugins, setPlugins] = useState<PluginView[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [from, setFrom] = useState("");
  const [showFolder, setShowFolder] = useState(false);
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

  useEffect(() => {
    if (tab === "Catálogo" && !catalog) {
      api.skillsCatalog().then(setCatalog).catch((e) => setError(String(e)));
    }
    if (tab === "Plugins" && !plugins) {
      api.skillsPlugins().then(setPlugins).catch((e) => setError(String(e)));
    }
  }, [tab, catalog, plugins]);

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
    Plugins: plugins?.length,
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
          <button className="btn btn-sm" aria-expanded={showFolder} onClick={() => setShowFolder((v) => !v)}>
            <FolderInput size={14} strokeWidth={2} /> Instalar de uma pasta…
          </button>
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
                onRemove={() =>
                  void act(async () => {
                    const list = await api.skillRemove(current.installed.name);
                    setSelected(null);
                    return list;
                  })
                }
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
              {catalog === null ? <Loading text="Lendo o catálogo…" /> : null}
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
        <Card
          title="Plugins"
          icon={Plug}
          description={plugins ? `${plugins.length} ${plugins.length === 1 ? "plugin instalado" : "plugins instalados"}` : undefined}
        >
          {plugins === null ? <Loading text="Lendo os plugins…" /> : null}
          {plugins?.length === 0 ? (
            <EmptyState icon={Plug} title="Nenhum plugin" text="Plugins instalados aparecem aqui." />
          ) : null}
          {plugins?.length ? (
            <div className="pn-list">
              {plugins.map((p) => (
                <div key={p.name} className={"pn-item" + (p.enabled ? "" : " dim")}>
                  <span className="pn-ico">
                    <Plug size={16} strokeWidth={1.75} />
                  </span>
                  <span className="pn-body">
                    <span className="pn-title">
                      {p.name} <span className="pn-num pn-hint">v{p.version}</span>
                    </span>
                    <span className="pn-sub">{p.description}</span>
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
                    <span className={"badge" + (p.trusted ? "" : " warn")}>
                      {p.trusted ? <ShieldCheck size={12} strokeWidth={2} /> : <AlertTriangle size={12} strokeWidth={2} />}
                      {p.trusted ? "Revisado" : "Não revisado"}
                    </span>
                    <span className={"badge" + (p.enabled ? " ok" : "")}>{p.enabled ? "Ativo" : "Desligado"}</span>
                  </span>
                </div>
              ))}
            </div>
          ) : null}
        </Card>
      ) : null}
    </div>
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
            {s.installed.source}
          </span>
        </dd>
        <dt>Pasta</dt>
        <dd>
          <span className="mono-path" title={s.installed.dir}>
            {s.installed.dir}
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
