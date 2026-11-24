import { useCallback, useEffect, useMemo, useState } from "react";
import { CircleAlert, CircleCheck, Import, KeyRound, RefreshCw } from "lucide-react";
import { api, type MigrationItem, type MigrationOpts, type MigrationPlan } from "../lib/api";
import { Card } from "./ui";

/** Tipos do plano, na ordem do backend (`hermes_migrate::KINDS`). */
const KINDS: [string, string][] = [
  ["sessao", "Conversas"],
  ["memoria", "Memória"],
  ["persona", "Persona e instruções"],
  ["credencial", "Credenciais → inventário"],
  ["skill", "Skills"],
  ["mcp", "Servidores MCP"],
  ["cron", "Agendamentos"],
  ["telegram", "Telegram"],
];

const STATUS: Record<MigrationItem["status"], [string, string]> = {
  imported: ["importar", "ok"],
  skipped: ["pular", "quiet"],
  conflict: ["conflito", "warn"],
  archived: ["arquivar", "accent"],
  error: ["erro", "bad"],
};

/**
 * "Migrar do Hermes": mostra o plano (nada gravado), deixa escolher os tipos
 * e só aplica com confirmação. Senhas para o cofre começam desmarcadas.
 */
export function HermesMigration(props: { onDone?: () => void }) {
  const [kinds, setKinds] = useState<Set<string>>(() => new Set(KINDS.map(([k]) => k)));
  const [secrets, setSecrets] = useState(false);
  const [pinnedOnly, setPinnedOnly] = useState(false);
  const [plan, setPlan] = useState<MigrationPlan | null>(null);
  const [done, setDone] = useState<MigrationPlan | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const opts = useCallback(
    (): MigrationOpts => ({
      kinds: [...kinds],
      migrateSecrets: secrets,
      onlyPinnedVisible: pinnedOnly,
    }),
    [kinds, secrets, pinnedOnly],
  );

  const preview = useCallback(() => {
    setBusy(true);
    api
      .migrationPlan({ ...opts(), kinds: null })
      .then((p) => {
        setPlan(p);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  }, [opts]);

  useEffect(() => {
    preview();
    // Só na abertura; depois, "Atualizar".
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const byKind = useMemo(() => {
    const m = new Map<string, MigrationItem[]>();
    for (const i of plan?.items ?? []) m.set(i.kind, [...(m.get(i.kind) ?? []), i]);
    return m;
  }, [plan]);

  const pending = (plan?.items ?? []).some((i) => kinds.has(i.kind) && i.status !== "skipped");

  const apply = () => {
    if (!window.confirm("Aplicar a migração do Hermes? Um backup dos dados do Zerith é feito antes.")) return;
    setBusy(true);
    api
      .migrationApply(opts())
      .then((r) => {
        setDone(r);
        setError(null);
        props.onDone?.();
        preview();
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  const flip = (k: string, on: boolean) =>
    setKinds((s) => {
      const n = new Set(s);
      if (on) n.add(k);
      else n.delete(k);
      return n;
    });

  return (
    <Card
      title="Migrar do Hermes"
      icon={Import}
      description="Conversas, memória, persona, credenciais, skills, MCP, agendamentos e Telegram num plano só. Nada é gravado até você aplicar; o Hermes não é alterado."
      actions={
        <button className="btn btn-sm" disabled={busy} onClick={preview}>
          {busy ? <span className="spinner" /> : <RefreshCw size={14} strokeWidth={2} />} Atualizar
        </button>
      }
    >
      {error ? (
        <div className="toast bad" role="alert">
          <CircleAlert size={16} strokeWidth={1.75} />
          <span>{error}</span>
        </div>
      ) : null}
      {done ? (
        <div className="pn-callout note" role="status">
          <CircleCheck size={14} strokeWidth={2} />
          <span>
            <strong>Migração aplicada.</strong> Relatório em <span className="mono">{done.report_dir}</span>; backup
            em <span className="mono">{done.backup_dir}</span>.
          </span>
        </div>
      ) : null}
      {plan === null && busy ? (
        <div className="pn-loading">
          <span className="spinner" /> Montando o plano…
        </div>
      ) : null}
      {plan ? (
        <>
          <p className="pn-hint mono">{plan.source_root}</p>
          <div className="pn-list">
            {KINDS.filter(([k]) => byKind.has(k)).map(([k, label]) => {
              const items = byKind.get(k) ?? [];
              const count = (s: MigrationItem["status"]) => items.filter((i) => i.status === s).length;
              return (
                <details key={k} className="pn-item">
                  <summary>
                    <label className="check" onClick={(e) => e.stopPropagation()}>
                      <input type="checkbox" checked={kinds.has(k)} onChange={(e) => flip(k, e.target.checked)} />
                      {label}
                    </label>{" "}
                    {(Object.keys(STATUS) as MigrationItem["status"][])
                      .filter((s) => count(s) > 0)
                      .map((s) => (
                        <span key={s} className={"badge " + STATUS[s][1]}>
                          {count(s)} {STATUS[s][0]}
                        </span>
                      ))}
                  </summary>
                  <ul className="pn-sub">
                    {items.slice(0, 200).map((i, n) => (
                      <li key={n}>
                        <span className={"badge " + STATUS[i.status][1]}>{STATUS[i.status][0]}</span>{" "}
                        <span className="mono">{i.source}</span> → {i.dest}
                        {i.reason ? ` — ${i.reason}` : ""}
                      </li>
                    ))}
                  </ul>
                </details>
              );
            })}
          </div>
          {plan.stripped_secrets.length ? (
            <div className="pn-callout" role="note">
              <KeyRound size={14} strokeWidth={2} />
              <span>
                {plan.stripped_secrets.length} segredo(s) ficam de fora (só os nomes aparecem aqui):{" "}
                <span className="mono">{plan.stripped_secrets.join(", ")}</span>
              </span>
            </div>
          ) : null}
          <label className="check">
            <input type="checkbox" checked={secrets} onChange={(e) => setSecrets(e.target.checked)} />
            Migrar senhas para o cofre do sistema
          </label>
          <label className="check">
            <input type="checkbox" checked={pinnedOnly} onChange={(e) => setPinnedOnly(e.target.checked)} />
            Deixar visíveis só as conversas fixadas (o resto entra arquivado e continua buscável)
          </label>
          <div className="pn-detail-foot">
            <button className="btn btn-primary" disabled={busy || !pending} onClick={apply}>
              <Import size={16} strokeWidth={1.75} /> {pending ? "Aplicar migração" : "Nada a migrar"}
            </button>
          </div>
        </>
      ) : null}
    </Card>
  );
}
