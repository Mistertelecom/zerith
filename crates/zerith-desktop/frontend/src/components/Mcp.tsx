import { Fragment, useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  Check,
  CircleAlert,
  CircleCheck,
  Copy,
  Download,
  FlaskConical,
  Globe,
  Import,
  KeyRound,
  LogIn,
  LogOut,
  Plus,
  Save,
  Server,
  ShieldCheck,
  Terminal,
  Trash2,
  Wrench,
  X,
} from "lucide-react";
import { api, onOAuthUrl, type HermesMcp, type McpServer, type SetupImport } from "../lib/api";
import { Card, EmptyState, Segmented, Switch } from "./ui";
import { importLine } from "./Skills";

type Tested = { tools: { name: string; description: string }[] } | { error: string };

const HERMES: Record<HermesMcp["status"], [string, string]> = {
  importavel: ["Importável", " ok"],
  "nao-encontrado": ["Comando não encontrado", " warn"],
  oauth: ["Importável — depois clique em Entrar", " ok"],
  existe: ["Já existe", ""],
  invalido: ["Configuração inválida", " danger"],
};

const EMPTY = { name: "", command: "", url: "", env: "", headers: "" };

const MCP_PRESETS: {
  id: string;
  label: string;
  category: "Dev" | "NetOps";
  hint: string;
  kind: "stdio" | "http";
  command: string;
  url: string;
  env: string;
}[] = [
  {
    id: "context7",
    label: "Context7 (Docs)",
    category: "Dev",
    hint: "Documentação oficial e atualizada de crates/pacotes em tempo real (Upstash)",
    kind: "stdio",
    command: "npx -y @upstash/context7-mcp",
    url: "",
    env: "",
  },
  {
    id: "github",
    label: "GitHub MCP",
    category: "Dev",
    hint: "PRs, issues, código, releases e logs do GitHub Actions",
    kind: "stdio",
    command: "npx -y @modelcontextprotocol/server-github",
    url: "",
    env: "GITHUB_PERSONAL_ACCESS_TOKEN=",
  },
  {
    id: "ast_grep",
    label: "ast-grep (AST)",
    category: "Dev",
    hint: "Busca e refatoração estrutural por árvore sintática em 25+ linguagens",
    kind: "stdio",
    command: "uvx --from git+https://github.com/ast-grep/ast-grep-mcp ast-grep-server",
    url: "",
    env: "",
  },
  {
    id: "sequential_thinking",
    label: "Sequential Thinking",
    category: "Dev",
    hint: "Decomposição e validação passo a passo para problemas complexos",
    kind: "stdio",
    command: "npx -y @modelcontextprotocol/server-sequential-thinking",
    url: "",
    env: "",
  },
  {
    id: "netbox",
    label: "NetBox (IPAM/DCIM)",
    category: "NetOps",
    hint: "Consulta prefixos IPv4/IPv6, VLANs, VRFs, ASNs, circuitos e ativos de rede",
    kind: "stdio",
    command: "uvx netbox-mcp-server",
    url: "",
    env: "NETBOX_URL=https://netbox.lan\nNETBOX_TOKEN=",
  },
  {
    id: "grafana",
    label: "Grafana NOC",
    category: "NetOps",
    hint: "Dashboards, alertas, Loki e métricas de NOC/infraestrutura",
    kind: "stdio",
    command: "npx -y mcp-grafana",
    url: "",
    env: "GRAFANA_URL=http://localhost:3000\nGRAFANA_API_KEY=",
  },
  {
    id: "prometheus",
    label: "Prometheus (PromQL)",
    category: "NetOps",
    hint: "Séries temporais de tráfego, CPU, erros ópticos e disponibilidade",
    kind: "stdio",
    command: "uvx prometheus-mcp-server",
    url: "",
    env: "PROMETHEUS_URL=http://localhost:9090",
  },
  {
    id: "postgres",
    label: "PostgreSQL",
    category: "Dev",
    hint: "Inspeção de schemas e consultas SQL somente-leitura",
    kind: "stdio",
    command: "npx -y @modelcontextprotocol/server-postgres postgresql://localhost/mydb",
    url: "",
    env: "",
  },
];

export function McpTab() {
  const [servers, setServers] = useState<McpServer[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tested, setTested] = useState<Record<string, Tested>>({});
  const [testing, setTesting] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [kind, setKind] = useState<"stdio" | "http">("stdio");
  const [form, setForm] = useState(EMPTY);
  const [hermes, setHermes] = useState<HermesMcp[] | null>(null);
  const [showHermes, setShowHermes] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [result, setResult] = useState<SetupImport | null>(null);
  const [loggingIn, setLoggingIn] = useState<string | null>(null);
  const [loginUrl, setLoginUrl] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(() => {
    api
      .mcpList()
      .then((l) => {
        setServers(l);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);
  useEffect(load, [load]);

  const act = async (job: () => Promise<McpServer[]>) => {
    setBusy(true);
    try {
      setServers(await job());
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const test = (name: string) => {
    setTesting(name);
    api
      .mcpTest(name)
      .then((tools) => setTested((t) => ({ ...t, [name]: { tools } })))
      .catch((e) => setTested((t) => ({ ...t, [name]: { error: String(e) } })))
      .finally(() => setTesting(null));
  };

  /** "Entrar": the backend opens the browser and waits (5 min) for the
   * callback; meanwhile the row shows the link to copy. */
  const login = async (name: string) => {
    setLoggingIn(name);
    setError(null);
    const off = await onOAuthUrl(setLoginUrl).catch(() => null);
    try {
      setServers(await api.mcpOauthLogin(name));
      setTested((t) => {
        const n = { ...t };
        delete n[name];
        return n;
      });
    } catch (e) {
      setError(String(e));
    } finally {
      off?.();
      setLoginUrl(null);
      setLoggingIn(null);
    }
  };

  const openHermes = () => {
    setShowHermes((v) => !v);
    setResult(null);
    api
      .mcpHermesPreview()
      .then((l) => {
        setHermes(l);
        setPicked(new Set(l.filter((s) => s.status === "importavel" || s.status === "oauth").map((s) => s.name)));
      })
      .catch((e) => setError(String(e)));
  };

  const runImport = async () => {
    setBusy(true);
    try {
      const r = await api.mcpHermesImport([...picked]);
      setResult(r);
      setPicked(new Set());
      setHermes(await api.mcpHermesPreview());
      setServers(await api.mcpList());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const save = () =>
    void act(async () => {
      const list = await api.mcpAdd({
        name: form.name,
        command: kind === "stdio" ? form.command : "",
        url: kind === "http" ? form.url : "",
        env: kind === "stdio" ? form.env : "",
        headers: kind === "http" ? form.headers : "",
      });
      setForm(EMPTY);
      setAdding(false);
      return list;
    });

  const field = (key: keyof typeof EMPTY) => ({
    value: form[key],
    onChange: (e: { target: { value: string } }) => setForm((f) => ({ ...f, [key]: e.target.value })),
  });

  return (
    <div className="page pn">
      <div className="pn-toolbar">
        <p className="pn-hint pn-spacer">
          Servidores MCP viram ferramentas do agente (<span className="mono">mcp__servidor__ferramenta</span>).
        </p>
        <button className="btn btn-sm" aria-expanded={showHermes} onClick={openHermes}>
          <Import size={14} strokeWidth={2} /> Importar do Hermes
        </button>
        <button className="btn btn-primary btn-sm" aria-expanded={adding} onClick={() => setAdding((v) => !v)}>
          <Plus size={14} strokeWidth={2} /> Adicionar
        </button>
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

      {adding ? (
        <Card title="Novo servidor" icon={Plus} description="Escolha um preset do GitHub com 1 clique ou preencha manualmente (gravado em mcp.toml do usuário).">
          <div className="pn-form">
            <div className="field">
              <span className="field-label">Presets rápidos (Dev &amp; NetOps)</span>
              <div className="pn-inline wrap">
                {MCP_PRESETS.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    className={"btn btn-sm" + (form.name === p.id ? " btn-primary" : "")}
                    title={p.hint}
                    onClick={() => {
                      setKind(p.kind);
                      setForm({
                        name: p.id,
                        command: p.command,
                        url: p.url,
                        env: p.env,
                        headers: "",
                      });
                    }}
                  >
                    <span className="badge">{p.category}</span> {p.label}
                  </button>
                ))}
              </div>
            </div>
            <div className="pn-row2">
              <label className="field">
                <span className="field-label">Nome</span>
                <input className="input mono" placeholder="docs" {...field("name")} />
                <span className="field-hint">Letras, números, “-” e “_”.</span>
              </label>
              <div className="field pn-field-top">
                <span className="field-label">Transporte</span>
                <Segmented
                  label="Transporte"
                  value={kind}
                  options={[
                    ["stdio", "Comando (stdio)"],
                    ["http", "URL (HTTP)"],
                  ]}
                  onPick={(v) => setKind(v as "stdio" | "http")}
                />
              </div>
            </div>
            {kind === "stdio" ? (
              <>
                <label className="field">
                  <span className="field-label">Comando e argumentos</span>
                  <input className="input mono" placeholder="npx -y @exemplo/servidor-docs" {...field("command")} />
                </label>
                <label className="field">
                  <span className="field-label">Variáveis de ambiente</span>
                  <textarea className="input mono" rows={3} placeholder={"CHAVE=valor\nOUTRA=valor"} {...field("env")} />
                  <span className="field-hint">Uma por linha. Os valores não voltam para a tela depois de salvos.</span>
                </label>
              </>
            ) : (
              <>
                <label className="field">
                  <span className="field-label">URL</span>
                  <input className="input mono" placeholder="https://exemplo.test/mcp" {...field("url")} />
                </label>
                <label className="field">
                  <span className="field-label">Cabeçalhos</span>
                  <textarea className="input mono" rows={3} placeholder="Authorization=Bearer …" {...field("headers")} />
                  <span className="field-hint">Uma por linha, CHAVE=valor. Os valores não voltam para a tela.</span>
                </label>
              </>
            )}
            <div className="pn-inline pn-end">
              <button className="btn btn-ghost btn-sm" onClick={() => setAdding(false)}>
                Cancelar
              </button>
              <button
                className="btn btn-primary btn-sm"
                disabled={busy || !form.name.trim() || !(kind === "stdio" ? form.command : form.url).trim()}
                onClick={save}
              >
                <Save size={14} strokeWidth={2} /> Salvar
              </button>
            </div>
          </div>
        </Card>
      ) : null}

      {showHermes ? (
        <Card
          title="Importar do Hermes"
          icon={Import}
          description="Lido de config.yaml do Hermes (só leitura). Env e cabeçalhos vêm junto; servidores desligados lá chegam desligados."
          actions={
            <button
              className="btn btn-primary btn-sm"
              disabled={busy || picked.size === 0}
              onClick={() => void runImport()}
            >
              {busy ? <span className="spinner" /> : <Download size={14} strokeWidth={2} />} Importar selecionados
            </button>
          }
        >
          {result ? (
            <div className={"pn-callout" + (result.failed.length ? "" : " note")} role="status">
              {result.failed.length ? <AlertTriangle size={14} strokeWidth={2} /> : <CircleCheck size={14} strokeWidth={2} />}
              <span>
                <strong>{importLine(result)}</strong>
                {result.failed.length ? " · " + result.failed.join(" · ") : ""}
              </span>
            </div>
          ) : null}
          {hermes === null ? (
            <div className="pn-loading">
              <span className="spinner" /> Lendo config.yaml…
            </div>
          ) : null}
          {hermes?.length === 0 ? (
            <EmptyState icon={Server} title="Nenhum servidor no Hermes" text="O bloco mcp_servers está vazio." />
          ) : null}
          {hermes?.length ? (
            <div className="pn-list">
              {hermes.map((s) => {
                const ok = s.status === "importavel" || s.status === "oauth";
                const [label, tone] = HERMES[s.status];
                return (
                  <label key={s.name} className={"pn-item pn-hermes-row" + (ok ? "" : " dim")}>
                    <input
                      type="checkbox"
                      checked={picked.has(s.name)}
                      disabled={!ok}
                      aria-label={`Importar ${s.name}`}
                      onChange={(e) =>
                        setPicked((p) => {
                          const n = new Set(p);
                          if (e.target.checked) n.add(s.name);
                          else n.delete(s.name);
                          return n;
                        })
                      }
                    />
                    <span className="pn-ico">
                      {s.transport === "http" ? <Globe size={16} strokeWidth={1.75} /> : <Terminal size={16} strokeWidth={1.75} />}
                    </span>
                    <span className="pn-body">
                      <span className="pn-title">{s.name}</span>
                      <span className="pn-sub mono" title={s.target}>
                        {s.target}
                      </span>
                    </span>
                    <span className="pn-meta">
                      {s.envKeys.length ? (
                        <span className="badge" title={s.envKeys.join(", ")}>
                          <KeyRound size={12} strokeWidth={2} /> {s.envKeys.length}
                        </span>
                      ) : null}
                      {!s.enabled ? <span className="badge">Desligado</span> : null}
                      <span className={"badge" + tone}>
                        {s.status === "oauth" ? <LogIn size={12} strokeWidth={2} /> : ok ? <Check size={12} strokeWidth={2} /> : null}
                        {label}
                      </span>
                    </span>
                  </label>
                );
              })}
            </div>
          ) : null}
        </Card>
      ) : null}

      <Card
        title="Servidores"
        icon={Server}
        description={servers ? `${servers.length} ${servers.length === 1 ? "servidor configurado" : "servidores configurados"}` : undefined}
      >
        {servers === null ? (
          <div className="pn-loading">
            <span className="spinner" /> Lendo a configuração…
          </div>
        ) : null}
        {servers?.length === 0 ? (
          <EmptyState icon={Server} title="Nenhum servidor MCP" text="Adicione um ou importe os do Hermes." />
        ) : null}
        {servers?.length ? (
          <div className="pn-list">
            {servers.map((s) => {
              const t = tested[s.name];
              return (
                <Fragment key={s.name}>
                  <div className={"pn-item" + (s.enabled && s.approved ? "" : " dim")}>
                    <span className="pn-ico">
                      {s.transport === "http" ? <Globe size={16} strokeWidth={1.75} /> : <Terminal size={16} strokeWidth={1.75} />}
                    </span>
                    <span className="pn-body">
                      <span className="pn-title">{s.name}</span>
                      <span className="pn-sub mono" title={s.target}>
                        {s.target}
                      </span>
                    </span>
                    <span className="pn-meta">
                      <span className="badge mono">{s.transport}</span>
                      {s.envKeys.length + s.headerKeys.length ? (
                        <span className="badge" title={[...s.envKeys, ...s.headerKeys].join(", ")}>
                          <KeyRound size={12} strokeWidth={2} /> {s.envKeys.length + s.headerKeys.length}
                        </span>
                      ) : null}
                      {t && "tools" in t ? (
                        <span className="badge ok" title="Ferramentas no último teste">
                          <Wrench size={12} strokeWidth={2} /> {t.tools.length}
                        </span>
                      ) : null}
                      {t && "error" in t ? (
                        <span className="badge danger">
                          <AlertTriangle size={12} strokeWidth={2} /> Falhou
                        </span>
                      ) : null}
                      {s.oauth ? (
                        <span className={"badge" + (s.connected ? " ok" : " warn")}>
                          {s.connected ? <CircleCheck size={12} strokeWidth={2} /> : <CircleAlert size={12} strokeWidth={2} />}
                          {s.connected ? "Conectado" : "Precisa entrar"}
                        </span>
                      ) : null}
                      {s.approved ? null : (
                        <span className="badge warn">
                          <AlertTriangle size={12} strokeWidth={2} /> Do projeto, não aprovado
                        </span>
                      )}
                    </span>
                    <span className="pn-actions">
                      {s.oauth && !s.connected ? (
                        <button
                          className="btn btn-primary btn-sm"
                          aria-label={`Entrar em ${s.name}`}
                          disabled={loggingIn !== null}
                          onClick={() => void login(s.name)}
                        >
                          <LogIn size={14} strokeWidth={2} /> Entrar
                        </button>
                      ) : null}
                      {s.oauth && s.connected ? (
                        <button
                          className="btn btn-ghost btn-sm"
                          aria-label={`Sair de ${s.name}`}
                          disabled={busy || loggingIn !== null}
                          onClick={() => void act(() => api.mcpOauthLogout(s.name))}
                        >
                          <LogOut size={14} strokeWidth={2} /> Sair
                        </button>
                      ) : null}
                      <button
                        className="icon-btn sm"
                        title="Testar (lista as ferramentas)"
                        aria-label={`Testar ${s.name}`}
                        disabled={testing !== null}
                        onClick={() => test(s.name)}
                      >
                        {testing === s.name ? <span className="spinner" /> : <FlaskConical size={14} strokeWidth={2} />}
                      </button>
                      {s.approved ? null : (
                        <button
                          className="icon-btn sm"
                          title="Aprovar"
                          aria-label={`Aprovar ${s.name}`}
                          disabled={busy}
                          onClick={() => void act(() => api.mcpApprove(s.name))}
                        >
                          <ShieldCheck size={14} strokeWidth={2} />
                        </button>
                      )}
                      {s.project ? null : (
                        <button
                          className="icon-btn sm"
                          title="Remover"
                          aria-label={`Remover ${s.name}`}
                          disabled={busy}
                          onClick={() => {
                            // also drops its login from the vault: ask first
                            if (window.confirm(`Remover o servidor ${s.name}? O login salvo dele também é apagado.`))
                              void act(() => api.mcpRemove(s.name));
                          }}
                        >
                          <Trash2 size={14} strokeWidth={2} />
                        </button>
                      )}
                      <Switch
                        label={`Ativar ${s.name}`}
                        checked={s.enabled}
                        disabled={busy}
                        onChange={(v) => void act(() => api.mcpToggle(s.name, v))}
                      />
                    </span>
                  </div>
                  {loggingIn === s.name ? (
                    <div className="login-link pn-mcp-login" role="status">
                      <span className="spinner" aria-hidden />
                      <div className="login-link-text">
                        <strong>Aguardando o login no navegador…</strong>
                        <span>Se o navegador não abriu, copie o link e cole em qualquer navegador. Expira em 5 min.</span>
                      </div>
                      {loginUrl ? (
                        <button
                          className="btn btn-sm"
                          onClick={() =>
                            void navigator.clipboard.writeText(loginUrl).then(() => {
                              setCopied(true);
                              setTimeout(() => setCopied(false), 1500);
                            })
                          }
                        >
                          {copied ? <Check size={14} strokeWidth={2} /> : <Copy size={14} strokeWidth={2} />}{" "}
                          {copied ? "Copiado" : "Copiar link"}
                        </button>
                      ) : null}
                    </div>
                  ) : null}
                  {t && "error" in t ? (
                    <div className="pn-callout danger">
                      <CircleAlert size={14} strokeWidth={2} />
                      <span>{t.error}</span>
                    </div>
                  ) : null}
                  {t && "tools" in t && t.tools.length ? (
                    <div className="pn-inline wrap pn-mcp-tools">
                      {t.tools.map((tool) => (
                        <span key={tool.name} className="badge mono" title={tool.description}>
                          {tool.name}
                        </span>
                      ))}
                    </div>
                  ) : null}
                </Fragment>
              );
            })}
          </div>
        ) : null}
      </Card>
    </div>
  );
}
