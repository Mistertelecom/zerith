import { useCallback, useEffect, useState } from "react";
import { Bot, Check, CircleAlert, CircleCheck, Cpu, FileText, FolderOpen, Plus, Sparkles, X } from "lucide-react";
import { api, type AgentForm, type AgentView } from "../lib/api";
import { Card, EmptyState, Logo, Switch } from "./ui";

const LAYER: Record<string, string> = {
  builtin: "Embutido",
  user: "Pessoal",
  project: "Do projeto",
  plugin: "De plugin",
};

const BLANK: AgentForm = { name: "", description: "", instructions: "", tools: "", model: "" };

export function AgentsTab() {
  const [list, setList] = useState<AgentView[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [form, setForm] = useState<AgentForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .agentsList()
      .then((l) => {
        setList(l);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(load, [load]);

  const act = async (job: () => Promise<AgentView[]>) => {
    setBusy(true);
    try {
      setList(await job());
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const current = list?.find((a) => a.name === selected) ?? list?.[0] ?? null;
  const set = (patch: Partial<AgentForm>) =>
    setForm((prev) => ({ ...(prev ?? BLANK), ...patch }));

  const create = async () => {
    if (!form) return;
    setBusy(true);
    try {
      const l = await api.agentCreate(form);
      setList(l);
      setNotice("Agente criado.");
      setError(null);
      setForm(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page pn">
      <div className="pn-toolbar">
        <span className="pn-hint pn-spacer">
          Um agente é um arquivo .md: o nome do arquivo é o nome, o texto é a instrução.
        </span>
        <button className="btn" onClick={() => void api.agentReveal().catch((e) => setError(String(e)))}>
          <FolderOpen size={16} strokeWidth={1.75} /> Abrir pasta
        </button>
        <button
          className={"btn" + (form ? " btn-ghost" : " btn-primary")}
          disabled={busy}
          onClick={() => setForm(form ? null : BLANK)}
        >
          {form ? <X size={16} strokeWidth={1.75} /> : <Plus size={16} strokeWidth={1.75} />}
          {form ? "Cancelar" : "Novo agente"}
        </button>
      </div>
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
        </div>
      ) : null}
      {notice ? (
        <div className="toast" role="status">
          <CircleCheck size={16} strokeWidth={1.75} />
          <span>{notice}</span>
        </div>
      ) : null}

      {form ? (
        <Card title="Novo agente" icon={Bot} description="Vira um arquivo .md na pasta de agentes.">
          <div className="pn-form">
            <div className="pn-row2">
              <label className="field">
                <span className="field-label">Nome</span>
                <input
                  className="input mono"
                  value={form.name}
                  placeholder="revisor-rust"
                  onChange={(e) => set({ name: e.target.value })}
                />
                <span className="field-hint">Letras sem acento, números, “-” e “_”.</span>
              </label>
              <label className="field">
                <span className="field-label">Modelo</span>
                <span className="pn-affix">
                  <span className="pn-affix-icon">
                    {form.model.trim() ? <Logo id={form.model} size={18} /> : <Cpu size={16} strokeWidth={1.75} />}
                  </span>
                  <input
                    className="input"
                    value={form.model}
                    placeholder="Opcional; vazio herda o da conversa"
                    onChange={(e) => set({ model: e.target.value })}
                  />
                </span>
              </label>
            </div>
            <label className="field">
              <span className="field-label">Descrição</span>
              <input
                className="input"
                value={form.description}
                placeholder="Uma linha sobre o que ele faz"
                onChange={(e) => set({ description: e.target.value })}
              />
            </label>
            <label className="field">
              <span className="field-label">Ferramentas</span>
              <input
                className="input mono"
                value={form.tools}
                placeholder="read_file, grep"
                onChange={(e) => set({ tools: e.target.value })}
              />
              <span className="field-hint">Separadas por vírgula. Vazio herda todas.</span>
            </label>
            <label className="field">
              <span className="field-label">Instruções</span>
              <textarea
                className="input"
                rows={8}
                value={form.instructions}
                placeholder="O que ele deve fazer, e como."
                onChange={(e) => set({ instructions: e.target.value })}
              />
            </label>
            <div className="pn-detail-foot">
              <button className="btn btn-ghost" disabled={busy} onClick={() => setForm(null)}>
                Cancelar
              </button>
              <button className="btn btn-primary" disabled={busy} onClick={() => void create()}>
                <Check size={16} strokeWidth={1.75} /> Criar agente
              </button>
            </div>
          </div>
        </Card>
      ) : (
        <>
          {list === null ? (
            <div className="pn-loading">
              <span className="spinner" /> Lendo os agentes…
            </div>
          ) : null}
          {list?.length === 0 ? (
            <div className="card">
              <EmptyState icon={Bot} title="Nenhum agente" text="Crie um em “Novo agente” ou solte um .md na pasta." />
            </div>
          ) : null}
          <div className="pn-grid wide">
            {list?.map((a) => (
              <button
                key={a.name}
                className={"pn-tile" + (current?.name === a.name ? " on" : "") + (a.disabled ? " dim" : "")}
                aria-pressed={current?.name === a.name}
                onClick={() => setSelected(a.name)}
              >
                <span className="pn-tile-head">
                  <span className="pn-ico accent">
                    <Bot size={16} strokeWidth={1.75} />
                  </span>
                  <span className="pn-body">
                    <span className="pn-title">{a.name}</span>
                    <span className="pn-sub">{LAYER[a.layer] ?? a.layer}</span>
                  </span>
                  {a.disabled ? <span className="badge">Desligado</span> : null}
                </span>
                <span className="pn-desc">{a.description || "Sem descrição."}</span>
                <span className="pn-tile-foot">
                  {a.model ? (
                    <span className="badge mono">
                      <Logo id={a.model} size={12} /> {a.model}
                    </span>
                  ) : (
                    <span className="badge">Modelo herdado</span>
                  )}
                  {a.createdByAi ? (
                    <span className="badge accent">
                      <Sparkles size={12} strokeWidth={2} /> Criado pela IA
                    </span>
                  ) : null}
                </span>
              </button>
            ))}
          </div>

          {current ? (
            <Card
              title={current.name}
              icon={Bot}
              description={LAYER[current.layer] ?? current.layer}
              actions={
                current.layer === "user" && (current.disabled || current.createdByAi) ? (
                  <label className="pn-inline">
                    <span className="pn-hint">{current.disabled ? "Desligado" : "Ativo"}</span>
                    <Switch
                      label={`Ativar ${current.name}`}
                      checked={!current.disabled}
                      disabled={busy}
                      onChange={() => void act(() => api.agentToggle(current.name, !current.disabled))}
                    />
                  </label>
                ) : null
              }
            >
              <dl className="pn-dl">
                <dt>Modelo</dt>
                <dd>
                  {current.model ? (
                    <span className="badge mono">
                      <Logo id={current.model} size={12} /> {current.model}
                    </span>
                  ) : (
                    <span className="badge">Herdado da conversa</span>
                  )}
                </dd>
                <dt>Ferramentas</dt>
                <dd className="pn-inline wrap">
                  {current.tools ? (
                    current.tools.map((t) => (
                      <span key={t} className="badge mono">
                        {t}
                      </span>
                    ))
                  ) : (
                    <span className="badge">Todas</span>
                  )}
                </dd>
              </dl>
              <div className="pn-code">
                <div className="pn-code-head">
                  <FileText size={12} strokeWidth={2} />
                  <span className="mono-path" title={current.path}>
                    {current.path}
                  </span>
                </div>
                <pre>{current.instructions.slice(0, 4000)}</pre>
              </div>
            </Card>
          ) : null}
        </>
      )}
    </div>
  );
}
