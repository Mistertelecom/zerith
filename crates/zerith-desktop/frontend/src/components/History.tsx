import { useCallback, useEffect, useState } from "react";
import {
  Archive,
  ArchiveRestore,
  CalendarRange,
  CircleAlert,
  CircleCheck,
  CopyCheck,
  Database,
  Download,
  FolderSearch,
  Import,
  List,
  MessageSquare,
  MessagesSquare,
  Search,
  Trash2,
} from "lucide-react";
import { api, type ReportView, type SessionRow, type SourceView } from "../lib/api";
import { Card, EmptyState, Logo, Segmented, Stat } from "./ui";

const when = (ts: number) => {
  const d = new Date(ts < 1e12 ? ts * 1000 : ts);
  return d.toLocaleString("pt-BR", { dateStyle: "short", timeStyle: "short" });
};

const day = (ts: number) => new Date(ts < 1e12 ? ts * 1000 : ts).toLocaleDateString("pt-BR");

const FILTERS = [
  ["active", "Ativas"],
  ["all", "Todas"],
] as const;

export function HistoryTab() {
  const [rows, setRows] = useState<SessionRow[] | null>(null);
  const [archived, setArchived] = useState(false);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sources, setSources] = useState<SourceView[] | null>(null);
  const [source, setSource] = useState<SourceView | null>(null);
  const [report, setReport] = useState<ReportView | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    api
      .sessionsHistory(archived)
      .then((r) => {
        setRows(r);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, [archived]);

  useEffect(load, [load]);

  const search = () => {
    const q = query.trim();
    if (!q) return load();
    api
      .searchSessions(q)
      .then((ids) =>
        setRows((prev) => (prev ?? []).filter((r) => ids.includes(r.id))),
      )
      .catch((e) => setError(String(e)));
  };

  const scan = () => {
    setBusy(true);
    api
      .hermesDetect()
      .then((s) => {
        setSources(s);
        setSource(s[0] ?? null);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  const run = (path: string) => {
    setBusy(true);
    api
      .hermesImport(path)
      .then((r) => {
        setReport(r);
        setNotice(
          `${r.sessionsImported} conversa(s) importada(s), ${r.messagesImported} mensagem(ns).`,
        );
        setError(null);
        load();
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="page pn">
      <div className="pn-toolbar">
        <label className="pn-affix">
          <span className="pn-affix-icon">
            <Search size={16} strokeWidth={1.75} />
          </span>
          <input
            className="input"
            value={query}
            placeholder="Buscar no título e nas mensagens"
            aria-label="Buscar conversas"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") search();
            }}
          />
          <span className="kbd">Enter</span>
        </label>
        <Segmented
          label="Filtro de conversas"
          value={archived ? "all" : "active"}
          options={FILTERS}
          onPick={(v) => setArchived(v === "all")}
        />
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

      <Card
        title="Conversas"
        icon={MessagesSquare}
        description={
          rows
            ? `${rows.length} ${rows.length === 1 ? "conversa" : "conversas"}${archived ? ", incluindo arquivadas" : ""}`
            : undefined
        }
      >
        {rows === null ? (
          <div className="pn-loading">
            <span className="spinner" /> Lendo o histórico…
          </div>
        ) : null}
        {rows?.length === 0 ? (
          <EmptyState icon={MessageSquare} title="Nenhuma conversa" text="As conversas aparecem aqui assim que você começa uma." />
        ) : null}
        {rows?.length ? (
          <div className="pn-list">
            {rows.map((r) => (
              <div key={r.id} className={"pn-item" + (r.archived ? " dim" : "")}>
                <Logo id={r.provider ?? r.model} size={28} />
                <span className="pn-body">
                  <span className="pn-title">{r.title}</span>
                  <span className="pn-sub">
                    {r.model ? <span className="mono">{r.model}</span> : null}
                    {r.model ? " · " : ""}
                    {when(r.updatedAt)}
                  </span>
                </span>
                <span className="pn-meta">
                  {r.source === "hermes" ? (
                    <span className="badge" title="Importada do Hermes">
                      <Import size={12} strokeWidth={2} /> hermes
                    </span>
                  ) : null}
                  {r.archived ? (
                    <span className="badge">
                      <Archive size={12} strokeWidth={2} /> Arquivada
                    </span>
                  ) : null}
                </span>
                <span className="pn-actions">
                  <button
                    className="icon-btn sm"
                    title="Exportar"
                    aria-label={`Exportar ${r.title}`}
                    onClick={() =>
                      void api
                        .exportSession(r.id)
                        .then((p) => setNotice(`Exportado em ${p}`))
                        .catch((e) => setError(String(e)))
                    }
                  >
                    <Download size={14} strokeWidth={2} />
                  </button>
                  <button
                    className="icon-btn sm"
                    title={r.archived ? "Restaurar" : "Arquivar"}
                    aria-label={`${r.archived ? "Restaurar" : "Arquivar"} ${r.title}`}
                    onClick={() =>
                      void api
                        .setArchived(r.id, !r.archived)
                        .then(() => load())
                        .catch((e) => setError(String(e)))
                    }
                  >
                    {r.archived ? <ArchiveRestore size={14} strokeWidth={2} /> : <Archive size={14} strokeWidth={2} />}
                  </button>
                  <button
                    className="icon-btn sm danger"
                    title="Excluir"
                    aria-label={`Excluir ${r.title}`}
                    onClick={() =>
                      void api
                        .deleteSession(r.id)
                        .then(() => load())
                        .catch((e) => setError(String(e)))
                    }
                  >
                    <Trash2 size={14} strokeWidth={2} />
                  </button>
                </span>
              </div>
            ))}
          </div>
        ) : null}
      </Card>

      <Card
        title="Importar do Hermes"
        icon={Import}
        description="O banco de origem é aberto só para leitura. Rodar de novo não duplica: cada conversa é casada pelo id de origem."
        actions={
          <button className="btn btn-sm" disabled={busy} onClick={scan}>
            {busy ? <span className="spinner" /> : <FolderSearch size={14} strokeWidth={2} />} Procurar
          </button>
        }
      >
        {sources === null ? (
          <p className="pn-hint">Clique em “Procurar” para achar os bancos do Hermes neste computador.</p>
        ) : null}
        {sources?.length === 0 ? (
          <EmptyState
            icon={FolderSearch}
            title="Nenhum banco do Hermes encontrado"
            text="Instale o Hermes ou confira a pasta de dados dele."
          />
        ) : null}
        {sources?.length ? (
          <div className="pn-list">
            {sources.map((s) => (
              <button
                key={s.path}
                className={"pn-item" + (source?.path === s.path ? " on" : "")}
                aria-pressed={source?.path === s.path}
                onClick={() => setSource(s)}
              >
                <span className="pn-ico">
                  <Database size={16} strokeWidth={1.75} />
                </span>
                <span className="pn-body">
                  <span className="pn-title">{s.profile}</span>
                  <span className="pn-sub mono">{s.path}</span>
                </span>
                <span className="pn-meta">
                  <span className="badge" title="Conversas">
                    <MessagesSquare size={12} strokeWidth={2} /> {s.sessions.length.toLocaleString("pt-BR")}
                  </span>
                  <span className="badge" title="Mensagens">
                    <MessageSquare size={12} strokeWidth={2} /> {s.totalMessages.toLocaleString("pt-BR")}
                  </span>
                </span>
              </button>
            ))}
          </div>
        ) : null}
        {source ? (
          <>
            <div className="pn-stats">
              <Stat icon={MessagesSquare} value={String(source.sessions.length)} label="Conversas" tone="accent" />
              <Stat icon={MessageSquare} value={source.totalMessages.toLocaleString("pt-BR")} label="Mensagens" />
              <Stat
                icon={CalendarRange}
                value={source.minDate !== null ? day(source.minDate) : "—"}
                label={source.maxDate !== null ? `Até ${day(source.maxDate)}` : "Período"}
              />
            </div>
            {source.sessions.length ? (
              <div className="pn-code">
                <div className="pn-code-head">
                  <List size={12} strokeWidth={2} /> Prévia · {Math.min(40, source.sessions.length)} de{" "}
                  {source.sessions.length}
                </div>
                <pre>
                  {source.sessions
                    .slice(0, 40)
                    .map((s) => `${when(s.updatedAt)} · ${s.title} (${s.messageCount})`)
                    .join("\n")}
                </pre>
              </div>
            ) : null}
            <div className="pn-detail-foot">
              <button className="btn btn-primary" disabled={busy} onClick={() => run(source.path)}>
                <Import size={16} strokeWidth={1.75} /> Importar tudo
              </button>
            </div>
          </>
        ) : null}
        {report ? (
          <div className="pn-stats">
            <Stat icon={CircleCheck} value={String(report.sessionsImported)} label="Importadas" tone="ok" />
            <Stat icon={MessageSquare} value={report.messagesImported.toLocaleString("pt-BR")} label="Mensagens" />
            <Stat icon={CopyCheck} value={String(report.sessionsSkipped)} label="Já existiam" />
          </div>
        ) : null}
      </Card>
    </div>
  );
}
