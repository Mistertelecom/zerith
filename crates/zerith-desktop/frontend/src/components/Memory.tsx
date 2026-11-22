import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Brain,
  Check,
  CircleAlert,
  CircleCheck,
  FolderGit2,
  Heart,
  Import,
  Lightbulb,
  ListChecks,
  Pencil,
  Plus,
  RefreshCw,
  Scale,
  Search,
  SearchX,
  ShieldCheck,
  Sparkles,
  Trash2,
  User,
  X,
  type LucideIcon,
} from "lucide-react";
import { api, type Fact, type FactKind } from "../lib/api";
import { Card, EmptyState, Row, Stat, Switch } from "./ui";

const KINDS: [FactKind, string, LucideIcon, string][] = [
  ["instruction", "Instruções", ListChecks, "Regras que o agente sempre segue."],
  ["preference", "Preferências", Heart, "Como você gosta que ele trabalhe."],
  ["fact", "Fatos", Lightbulb, "Coisas que são verdade sobre você."],
  ["decision", "Decisões", Scale, "Escolhas já tomadas; ele não as reabre."],
  ["project", "Projetos", FolderGit2, "O que você está construindo."],
  ["entity", "Entidades", User, "Pessoas, empresas e lugares citados."],
];

/** `paused`/`onPause` come from the window when it wires them; alone, the tab
 * reads and writes the flag itself (the window rereads it when Ajustes closes). */
export function MemoryTab(props: { paused?: boolean; onPause?: (paused: boolean) => void } = {}) {
  const [facts, setFacts] = useState<Fact[] | null>(null);
  const [hits, setHits] = useState<number[] | null>(null);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<{ id: number; text: string } | null>(null);
  const [adding, setAdding] = useState(false);
  const [newKind, setNewKind] = useState<FactKind>("fact");
  const [newText, setNewText] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ownPaused, setOwnPaused] = useState(false);

  const load = useCallback(() => {
    setError(null);
    api
      .memoryFacts()
      .then((list) => {
        setFacts(list);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(load, [load]);

  useEffect(() => {
    if (props.paused === undefined) api.memoryPaused().then(setOwnPaused).catch(() => undefined);
  }, [props.paused]);

  const paused = props.paused ?? ownPaused;
  const pause = (v: boolean) => {
    if (props.onPause) return props.onPause(v);
    setOwnPaused(v);
    api.setMemoryPaused(v).catch((e) => setError(String(e)));
  };

  // The store keeps the order the search returned; the groups only decide
  // where each row is drawn.
  const groups = useMemo(() => {
    if (!facts) return [];
    const shown =
      hits === null ? facts : (hits.map((id) => facts.find((f) => f.id === id)).filter(Boolean) as Fact[]);
    return KINDS.map(([kind, label, icon, hint]) => ({
      kind,
      label,
      icon,
      hint,
      rows: shown.filter((f) => f.kind === kind),
    })).filter((g) => g.rows.length);
  }, [facts, hits]);

  const search = async () => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    try {
      setHits(await api.memorySearch(q));
    } catch (e) {
      setError(String(e));
    }
  };

  const save = async () => {
    if (!editing) return;
    const text = editing.text.trim();
    if (!text) return;
    try {
      await api.memoryUpdate(editing.id, text);
      setFacts((prev) =>
        prev ? prev.map((f) => (f.id === editing.id ? { ...f, text } : f)) : prev,
      );
      setEditing(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const forget = async (id: number) => {
    try {
      await api.memoryForget(id);
      setFacts((prev) => (prev ? prev.filter((f) => f.id !== id) : prev));
      setHits((prev) => (prev ? prev.filter((h) => h !== id) : prev));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const addFact = async () => {
    const clean = newText.trim();
    if (!clean) return;
    try {
      const created = await api.memoryAdd(newKind, clean);
      setFacts((prev) => [created, ...(prev ?? []).filter((f) => f.id !== created.id)]);
      setNewText("");
      setAdding(false);
      setNotice("Fato salvo na memória.");
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const importHermes = async () => {
    try {
      const count = await api.memoryImportHermes();
      load();
      setNotice(
        count > 0
          ? `${count} fatos importados da memória do Hermes (MEMORY.md / USER.md).`
          : "Nenhum arquivo MEMORY.md / USER.md encontrado em ~/.hermes/memories.",
      );
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  if (!facts && error)
    return (
      <div className="page pn">
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
          <button className="btn btn-sm" onClick={load}>
            <RefreshCw size={14} strokeWidth={2} /> Tentar de novo
          </button>
        </div>
      </div>
    );
  if (!facts)
    return (
      <div className="pn-loading">
        <span className="spinner" /> Lendo a memória…
      </div>
    );

  const confirmed = facts.filter((f) => f.confidence === "confirmed").length;

  return (
    <div className="page pn">
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
          <button className="icon-btn" aria-label="Fechar aviso" onClick={() => setError(null)}>
            <X size={14} strokeWidth={2} />
          </button>
        </div>
      ) : null}
      <div className="pn-stats">
        <Stat icon={Brain} value={String(facts.length)} label="Fatos guardados" tone="accent" />
        <Stat icon={CircleCheck} value={String(confirmed)} label="Confirmados por você" />
        <Stat icon={Sparkles} value={String(facts.length - confirmed)} label="Inferidos pela IA" />
      </div>

      <Card
        title="Gravação"
        icon={ShieldCheck}
        description="O agente aprende com as conversas e usa o que sabe nas próximas. Tudo fica neste computador."
        actions={
          <span className={"badge" + (paused ? " warn" : " ok")}>
            <span className={"dot" + (paused ? " warn" : " ok")} />
            {paused ? "Pausada" : "Ativa"}
          </span>
        }
      >
        <Row label="Pausar memória" hint="Enquanto ligado, as conversas não leem nem gravam lembranças.">
          <Switch label="Pausar memória" checked={paused} onChange={pause} />
        </Row>
      </Card>

      <div className="pn-toolbar">
        <label className="pn-affix">
          <span className="pn-affix-icon">
            <Search size={16} strokeWidth={1.75} />
          </span>
          <input
            className="input"
            value={query}
            placeholder="Buscar na memória"
            aria-label="Buscar na memória"
            onChange={(e) => {
              setQuery(e.target.value);
              if (!e.target.value.trim()) setHits(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void search();
            }}
          />
          <span className="kbd">Enter</span>
        </label>
        {hits === null ? null : (
          <span className="pn-hint">
            {hits.length} {hits.length === 1 ? "resultado" : "resultados"}
          </span>
        )}
        <span className="pn-spacer" />
        <button className="btn btn-sm" onClick={() => void importHermes()}>
          <Import size={14} strokeWidth={2} /> Importar do Hermes
        </button>
        <button
          className={"btn btn-sm " + (adding ? "btn-ghost" : "btn-primary")}
          onClick={() => setAdding((v) => !v)}
        >
          {adding ? <X size={14} strokeWidth={2} /> : <Plus size={14} strokeWidth={2} />}
          {adding ? "Cancelar" : "Novo fato"}
        </button>
      </div>

      {notice ? (
        <div className="toast" role="status">
          <CircleCheck size={16} strokeWidth={1.75} />
          <span>{notice}</span>
        </div>
      ) : null}

      {adding ? (
        <Card title="Novo fato" icon={Brain} description="Adicione uma instrução, preferência, cliente/entidade ou fato durável.">
          <div className="pn-item on" style={{ gap: 8 }}>
            <select
              className="select"
              style={{ width: 150 }}
              value={newKind}
              onChange={(e) => setNewKind(e.target.value as FactKind)}
            >
              {KINDS.map(([k, lbl]) => (
                <option key={k} value={k}>
                  {lbl}
                </option>
              ))}
            </select>
            <input
              className="input"
              style={{ flex: 1 }}
              autoFocus
              placeholder="Ex.: O usuário se chama João e gerencia a Mistertelecom"
              value={newText}
              onChange={(e) => setNewText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void addFact();
                if (e.key === "Escape") {
                  // only this input, not the whole Settings (window keydown)
                  e.stopPropagation();
                  e.nativeEvent.stopImmediatePropagation();
                  setAdding(false);
                }
              }}
            />
            <button className="btn btn-primary btn-sm" onClick={() => void addFact()}>
              <Check size={14} strokeWidth={2} /> Salvar
            </button>
          </div>
        </Card>
      ) : null}

      {groups.length === 0 ? (
        <div className="card">
          {hits === null ? (
            <EmptyState
              icon={Brain}
              title="Memória vazia"
              text='Diga "lembre que …" numa conversa para guardar um fato.'
            />
          ) : (
            <EmptyState icon={SearchX} title="Nada encontrado" text="Nenhum fato corresponde à busca." />
          )}
        </div>
      ) : null}

      {groups.map((g) => (
        <Card
          key={g.kind}
          title={g.label}
          icon={g.icon}
          description={g.hint}
          actions={<span className="badge mono">{g.rows.length}</span>}
        >
          <div className="pn-list">
            {g.rows.map((f) =>
              editing?.id === f.id ? (
                <div key={f.id} className="pn-item on">
                  <input
                    className="input"
                    value={editing.text}
                    autoFocus
                    aria-label="Texto do fato"
                    onChange={(e) => setEditing({ id: f.id, text: e.target.value })}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void save();
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        e.nativeEvent.stopImmediatePropagation();
                        setEditing(null);
                      }
                    }}
                  />
                  <button className="btn btn-primary btn-sm" onClick={() => void save()}>
                    <Check size={14} strokeWidth={2} /> Salvar
                  </button>
                  <button className="btn btn-ghost btn-sm" onClick={() => setEditing(null)}>
                    <X size={14} strokeWidth={2} /> Cancelar
                  </button>
                </div>
              ) : (
                <div key={f.id} className="pn-item">
                  <span className="pn-body pn-text">{f.text}</span>
                  <span className="pn-meta">
                    {f.confidence === "confirmed" ? (
                      <span className="badge" title="Você confirmou este fato">
                        <CircleCheck size={12} strokeWidth={2} /> Confirmado
                      </span>
                    ) : (
                      <span className="badge" title="A IA deduziu da conversa">
                        <Sparkles size={12} strokeWidth={2} /> Inferido
                      </span>
                    )}
                  </span>
                  <span className="pn-actions">
                    <button
                      className="icon-btn sm"
                      title="Editar"
                      aria-label="Editar fato"
                      onClick={() => setEditing({ id: f.id, text: f.text })}
                    >
                      <Pencil size={14} strokeWidth={2} />
                    </button>
                    <button
                      className="icon-btn sm danger"
                      title="Apagar"
                      aria-label="Apagar fato"
                      onClick={() => {
                        if (window.confirm(`Apagar este fato da memória?

"${f.text}"`)) void forget(f.id);
                      }}
                    >
                      <Trash2 size={14} strokeWidth={2} />
                    </button>
                  </span>
                </div>
              ),
            )}
          </div>
        </Card>
      ))}
    </div>
  );
}
