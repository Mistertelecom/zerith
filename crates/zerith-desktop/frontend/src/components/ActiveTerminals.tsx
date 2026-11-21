// Active interactive terminals (`terminal_open`, `net_connect`) with live tail
// inspection, Ctrl+C interrupt (including in-flight commands), and close actions.
import { useCallback, useEffect, useState } from "react";
import { Eye, EyeOff, Square, Terminal, X } from "lucide-react";
import { terminalsApi, type TerminalSnapshot } from "../lib/api";

function ago(secs: number): string {
  if (secs < 60) return `ocioso ${secs}s`;
  if (secs < 3600) return `ocioso ${Math.floor(secs / 60)}m`;
  return `ocioso ${Math.floor(secs / 3600)}h`;
}

export function ActiveTerminals({ active }: { active: boolean }) {
  const [items, setItems] = useState<TerminalSnapshot[]>([]);
  const [openTail, setOpenTail] = useState<string | null>(null);
  const [tailText, setTailText] = useState<string>("");

  const refresh = useCallback(() => {
    terminalsApi
      .list()
      .then((list) => {
        setItems(list);
        if (openTail && !list.some((t) => t.id === openTail)) {
          setOpenTail(null);
          setTailText("");
        }
      })
      .catch(() => setItems([]));
  }, [openTail]);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, active || items.length ? 2000 : 8000);
    return () => clearInterval(t);
  }, [active, items.length > 0, refresh]);

  useEffect(() => {
    if (!openTail) return;
    const pull = () =>
      void terminalsApi
        .tail(openTail, 4096)
        .then(setTailText)
        .catch(() => setTailText(""));
    pull();
    const t = setInterval(pull, 1500);
    return () => clearInterval(t);
  }, [openTail]);

  if (!items.length) return null;

  return (
    <section className="jobs-strip terms-strip" aria-label="Terminais interativos abertos">
      {items.map((t) => {
        const label = t.device
          ? `${t.device}${t.host ? ` (${t.host})` : ""}`
          : t.command;
        const expanded = openTail === t.id;
        return (
          <div key={t.id} className="term-block">
            <div className="job-row">
              <span className="job-icon" aria-hidden>
                <Terminal size={14} strokeWidth={2} />
              </span>
              <code className="job-cmd" title={t.command}>
                {label}
              </code>
              {t.vendor || t.role ? (
                <span className="term-badge">
                  {[t.vendor, t.role].filter(Boolean).join(" · ")}
                </span>
              ) : null}
              <span className="job-meta">
                <span className="dot accent live" aria-hidden />
                {t.id} · {t.busy ? "executando" : ago(t.idleSecs)}
              </span>
              <button
                className="btn btn-sm"
                title={expanded ? "Ocultar saída" : "Ver tail ao vivo"}
                onClick={() => setOpenTail(expanded ? null : t.id)}
              >
                {expanded ? <EyeOff size={13} /> : <Eye size={13} />}
                Tail
              </button>
              <button
                className="btn btn-sm"
                title="Enviar Ctrl+C (0x03)"
                onClick={() => void terminalsApi.interrupt(t.id).catch(() => undefined)}
              >
                <Square size={12} />
                Ctrl+C
              </button>
              <button
                className="btn btn-sm btn-danger"
                title="Fechar sessão de terminal"
                onClick={() =>
                  void terminalsApi
                    .close(t.id)
                    .catch(() => undefined)
                    .finally(refresh)
                }
              >
                <X size={13} />
                Fechar
              </button>
            </div>
            {expanded ? (
              <pre className="term-tail">{tailText || "(sem saída recente)"}</pre>
            ) : null}
          </div>
        );
      })}
    </section>
  );
}
