// Active interactive terminals (`terminal_open`, `net_connect`) with live tail
// inspection, Ctrl+C interrupt (including in-flight commands), and close actions.
import { useCallback, useEffect, useState } from "react";
import { Eye, EyeOff, Send, Square, Terminal, X } from "lucide-react";
import {
  onTerminalsChanged,
  terminalsApi,
  type TerminalSnapshot,
} from "../lib/api";
import { tr, useLang, type Lang } from "../lib/i18n";

function ago(secs: number, lang: Lang): string {
  if (secs < 60) return tr(lang, `ocioso ${secs}s`, `idle ${secs}s`);
  if (secs < 3600) return tr(lang, `ocioso ${Math.floor(secs / 60)}m`, `idle ${Math.floor(secs / 60)}m`);
  return tr(lang, `ocioso ${Math.floor(secs / 3600)}h`, `idle ${Math.floor(secs / 3600)}h`);
}

export function ActiveTerminals({ active }: { active: boolean }) {
  const lang = useLang();
  const [items, setItems] = useState<TerminalSnapshot[]>([]);
  const [openTail, setOpenTail] = useState<string | null>(null);
  const [tailText, setTailText] = useState<string>("");
  const [cmd, setCmd] = useState("");
  const [sending, setSending] = useState(false);

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
    let cancel = false;
    let unlisten: (() => void) | undefined;
    void onTerminalsChanged(() => {
      if (!cancel) refresh();
    }).then((fn) => {
      if (cancel) fn();
      else unlisten = fn;
    });
    const t = setInterval(refresh, active || items.length ? 5000 : 15000);
    return () => {
      cancel = true;
      unlisten?.();
      clearInterval(t);
    };
  }, [active, items.length > 0, refresh]);

  const pullTail = useCallback((id: string) => {
    void terminalsApi
      .tail(id, 4096)
      .then(setTailText)
      .catch(() => setTailText(""));
  }, []);

  useEffect(() => {
    if (!openTail) return;
    pullTail(openTail);
    const t = setInterval(() => pullTail(openTail), 1500);
    return () => clearInterval(t);
  }, [openTail, pullTail]);

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
                {t.id} · {t.busy ? tr(lang, "executando", "running") : ago(t.idleSecs, lang)}
              </span>
              <button
                className="btn btn-sm"
                title={
                  expanded
                    ? tr(lang, "Ocultar saída", "Hide output")
                    : tr(lang, "Ver tail ao vivo", "View live tail")
                }
                onClick={() => setOpenTail(expanded ? null : t.id)}
              >
                {expanded ? <EyeOff size={13} /> : <Eye size={13} />}
                Tail
              </button>
              <button
                className="btn btn-sm"
                title={tr(lang, "Enviar Ctrl+C (0x03)", "Send Ctrl+C (0x03)")}
                onClick={() => void terminalsApi.interrupt(t.id).catch(() => undefined)}
              >
                <Square size={12} />
                Ctrl+C
              </button>
              <button
                className="btn btn-sm btn-danger"
                title={tr(lang, "Fechar sessão de terminal", "Close terminal session")}
                onClick={() =>
                  void terminalsApi
                    .close(t.id)
                    .catch(() => undefined)
                    .finally(refresh)
                }
              >
                <X size={13} />
                {tr(lang, "Fechar", "Close")}
              </button>
            </div>
            {expanded ? (
              <>
                <pre className="term-tail">
                  {tailText || tr(lang, "(sem saída recente)", "(no recent output)")}
                </pre>
                <form
                  className="term-input"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (!cmd.trim() || sending) return;
                    const text = cmd + "\n";
                    setSending(true);
                    void terminalsApi
                      .send(t.id, text)
                      .then(() => {
                        setCmd("");
                        pullTail(t.id);
                      })
                      .catch(() => undefined)
                      .finally(() => setSending(false));
                  }}
                >
                  <input
                    value={cmd}
                    onChange={(e) => setCmd(e.target.value)}
                    placeholder={tr(lang, "Enviar entrada + Enter…", "Send input + Enter…")}
                    aria-label={tr(lang, "Enviar entrada ao terminal", "Send input to terminal")}
                  />
                  <button
                    className="btn btn-sm"
                    type="submit"
                    disabled={!cmd.trim() || sending}
                  >
                    <Send size={12} />
                    {tr(lang, "Enviar", "Send")}
                  </button>
                </form>
              </>
            ) : null}
          </div>
        );
      })}
    </section>
  );
}
