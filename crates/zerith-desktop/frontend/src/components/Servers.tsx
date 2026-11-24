// Ajustes > Servidores: `zerith server` pareados pelo código `zerith-pair:`.
import { useCallback, useEffect, useState } from "react";
import { CircleAlert, FlaskConical, Plus, Server, Trash2, X } from "lucide-react";
import { serversApi, type ServerColor, type ServerEntry, type ServerStatus } from "../lib/api";
import { Card, EmptyState } from "./ui";

const DOT: Record<ServerColor, [string, string]> = {
  green: ["ok", "Online"],
  yellow: ["warn", "API responde, daemon parado ou atrasado"],
  red: ["danger", "Sem resposta"],
};

export function agoText(ms?: number | null): string {
  if (!ms) return "nunca";
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "agora";
  if (s < 3600) return `há ${Math.round(s / 60)} min`;
  if (s < 86400) return `há ${Math.round(s / 3600)} h`;
  return new Date(ms).toLocaleString();
}

export function ServerDot({ status }: { status?: ServerStatus }) {
  if (!status) return <span className="dot" title="Ainda não verificado" />;
  const [tone, label] = DOT[status.color];
  return <span className={"dot " + tone} title={status.error ?? label} aria-label={label} />;
}

export function ServersTab() {
  const [servers, setServers] = useState<ServerEntry[] | null>(null);
  const [status, setStatus] = useState<Record<string, ServerStatus>>({});
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const put = (st: ServerStatus) => setStatus((m) => ({ ...m, [st.id]: st }));

  const load = useCallback(async () => {
    try {
      const list = await serversApi.list();
      setServers(list);
      (await serversApi.status()).forEach(put);
      // Atualiza em paralelo; cada um pinta sua bolinha quando responde.
      list.forEach((s) => void serversApi.health(s.id).then(put).catch(() => undefined));
    } catch (e) {
      setError(String(e));
      setServers([]);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const add = async () => {
    setBusy("add");
    setError(null);
    try {
      await serversApi.add(code.trim());
      setCode("");
      setAdding(false);
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const test = async (id: string) => {
    setBusy(id);
    try {
      const st = await serversApi.test(id);
      put(st);
      if (st.error) setError(st.error);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const remove = async (s: ServerEntry) => {
    if (!window.confirm(`Remover "${s.name}"? O token sai do cofre; os workflows no servidor continuam lá.`)) return;
    try {
      await serversApi.remove(s.id);
      await load();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="page pn">
      <div className="pn-toolbar">
        <p className="pn-hint pn-spacer">
          Máquinas com o Zerith que rodam workflows 24 h. No editor, escolha “Executar em”.
        </p>
        <button className="btn btn-primary btn-sm" aria-expanded={adding} onClick={() => setAdding((v) => !v)}>
          <Plus size={14} strokeWidth={2} /> Adicionar servidor
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
        <Card
          title="Adicionar servidor"
          icon={Plus}
          description="Na VPS, rode zerith server init (com --bind no IP da VPN) e cole aqui o código que ele imprime."
        >
          <div className="pn-form">
            <label className="field">
              <span className="field-label">Código de pareamento</span>
              <textarea
                className="input mono"
                rows={3}
                placeholder="zerith-pair:eyJ2IjoxLC…"
                value={code}
                onChange={(e) => setCode(e.target.value)}
                autoFocus
              />
              <span className="field-hint">
                Ex.: <span className="mono">sudo -u zerith env ZERITH_HOME=/var/lib/zerith zerith server init --name vps --bind 100.x.y.z:7878</span>.
                O código contém o token: trate como senha. Ele vai para o cofre do sistema.
              </span>
            </label>
            <div className="pn-inline pn-end">
              <button className="btn btn-ghost btn-sm" onClick={() => setAdding(false)}>
                Cancelar
              </button>
              <button
                className="btn btn-primary btn-sm"
                disabled={busy !== null || !code.trim().startsWith("zerith-pair:")}
                onClick={() => void add()}
              >
                {busy === "add" ? <span className="spinner" /> : <Plus size={14} strokeWidth={2} />} Testar e salvar
              </button>
            </div>
          </div>
        </Card>
      ) : null}

      <Card
        title="Servidores"
        icon={Server}
        description={servers ? `${servers.length} ${servers.length === 1 ? "servidor cadastrado" : "servidores cadastrados"}` : undefined}
      >
        {servers === null ? (
          <div className="pn-loading">
            <span className="spinner" /> Lendo…
          </div>
        ) : null}
        {servers?.length === 0 ? (
          <EmptyState icon={Server} title="Nenhum servidor" text="Adicione um com o código de pareamento do zerith server init." />
        ) : null}
        {servers?.length ? (
          <div className="pn-list">
            {servers.map((s) => {
              const st = status[s.id];
              return (
                <div key={s.id} className="pn-item" data-server={s.id}>
                  <span className="pn-ico">
                    <ServerDot status={st} />
                  </span>
                  <span className="pn-body">
                    <span className="pn-title">{s.name}</span>
                    <span className="pn-sub mono" title={st?.error ?? s.url}>
                      {s.url}
                    </span>
                  </span>
                  <span className="pn-meta">
                    {st?.version ? <span className="badge mono">v{st.version}</span> : null}
                    {st?.daemon ? (
                      <span className="badge" title="Último ciclo do daemon">
                        daemon {agoText(st.daemon.last_cycle_ms)}
                      </span>
                    ) : null}
                    <span className="badge" title="Última resposta da API">
                      contato {agoText(st?.last_contact_ms ?? s.last_contact_ms)}
                    </span>
                  </span>
                  <span className="pn-actions">
                    <button
                      className="btn btn-sm"
                      aria-label={`Testar ${s.name}`}
                      disabled={busy !== null}
                      onClick={() => void test(s.id)}
                    >
                      {busy === s.id ? <span className="spinner" /> : <FlaskConical size={14} strokeWidth={2} />} Testar
                    </button>
                    <button className="icon-btn" aria-label={`Remover ${s.name}`} onClick={() => void remove(s)}>
                      <Trash2 size={14} strokeWidth={2} />
                    </button>
                  </span>
                </div>
              );
            })}
          </div>
        ) : null}
      </Card>
    </div>
  );
}
