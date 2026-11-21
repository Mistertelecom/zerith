// Active interactive terminals (`terminal_open`, `net_connect`) with live tail
// inspection, client/VPN/transport badges, command history, quick keys, and log copy.
import { useCallback, useEffect, useState } from "react";
import { Check, Copy, Eye, EyeOff, Send, Square, Terminal, X } from "lucide-react";
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
  const [copied, setCopied] = useState(false);
  const [historyByTerm, setHistoryByTerm] = useState<Record<string, string[]>>({});
  const [histCursor, setHistCursor] = useState<number>(-1);

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
      .tail(id, 16384)
      .then(setTailText)
      .catch(() => setTailText(""));
  }, []);

  useEffect(() => {
    if (!openTail) return;
    setHistCursor(-1);
    pullTail(openTail);
    const t = setInterval(() => pullTail(openTail), 1000);
    return () => clearInterval(t);
  }, [openTail, pullTail]);

  const sendRaw = useCallback(
    (id: string, raw: string) => {
      void terminalsApi
        .send(id, raw)
        .then(() => setTimeout(() => pullTail(id), 150))
        .catch(() => undefined);
    },
    [pullTail],
  );

  if (!items.length) return null;

  return (
    <section className="jobs-strip terms-strip" aria-label="Terminais interativos abertos">
      {items.map((t) => {
        const label = t.device
          ? `${t.device}${t.host ? ` (${t.host})` : ""}`
          : t.command;
        const expanded = openTail === t.id;
        const transportLabel = (t.transport || (t.command.startsWith("telnet://") ? "telnet" : "ssh")).toUpperCase();
        const termHist = historyByTerm[t.id] ?? [];
        return (
          <div key={t.id} className="term-block">
            <div className="job-row">
              <span className="job-icon" aria-hidden>
                <Terminal size={14} strokeWidth={2} />
              </span>
              <span className="term-badge">{transportLabel}</span>
              <code className="job-cmd" title={t.command}>
                {label}
              </code>
              {t.client ? (
                <span className="term-badge" title={tr(lang, "Cliente", "Client")}>
                  {t.client}
                </span>
              ) : null}
              {t.vpn ? (
                <span className="term-badge" title="WireGuard VPN">
                  VPN: {t.vpn}
                </span>
              ) : null}
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
                    ? tr(lang, "Ocultar console", "Hide console")
                    : tr(lang, "Abrir console ao vivo", "Open live console")
                }
                onClick={() => setOpenTail(expanded ? null : t.id)}
              >
                {expanded ? <EyeOff size={13} /> : <Eye size={13} />}
                Console
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
                    const trimmedCmd = cmd.trim();
                    const text = cmd + "\n";
                    setSending(true);
                    setHistoryByTerm((prev) => {
                      const prevList = prev[t.id] ?? [];
                      const nextList = [trimmedCmd, ...prevList.filter((x) => x !== trimmedCmd)].slice(0, 40);
                      return { ...prev, [t.id]: nextList };
                    });
                    setHistCursor(-1);
                    void terminalsApi
                      .send(t.id, text)
                      .then(() => {
                        setCmd("");
                        setTimeout(() => pullTail(t.id), 120);
                      })
                      .catch(() => undefined)
                      .finally(() => setSending(false));
                  }}
                >
                  <input
                    value={cmd}
                    onChange={(e) => setCmd(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "ArrowUp" && termHist.length > 0) {
                        e.preventDefault();
                        const nextIdx = Math.min(histCursor + 1, termHist.length - 1);
                        setHistCursor(nextIdx);
                        setCmd(termHist[nextIdx]);
                      } else if (e.key === "ArrowDown") {
                        e.preventDefault();
                        if (histCursor <= 0) {
                          setHistCursor(-1);
                          setCmd("");
                        } else {
                          const nextIdx = histCursor - 1;
                          setHistCursor(nextIdx);
                          setCmd(termHist[nextIdx]);
                        }
                      }
                    }}
                    placeholder={tr(
                      lang,
                      "Comando + Enter (↑/↓ histórico)…",
                      "Command + Enter (↑/↓ history)…",
                    )}
                    aria-label={tr(lang, "Enviar entrada ao terminal", "Send input to terminal")}
                  />
                  <button
                    className="btn btn-sm"
                    type="button"
                    title={tr(lang, "Enviar ? (ajuda de CLI Cisco/Huawei)", "Send ? (CLI help)")}
                    onClick={() => sendRaw(t.id, "?")}
                  >
                    ?
                  </button>
                  <button
                    className="btn btn-sm"
                    type="button"
                    title={tr(lang, "Enviar Espaço (avançar página --More--)", "Send Space (next page)")}
                    onClick={() => sendRaw(t.id, " ")}
                  >
                    Space
                  </button>
                  <button
                    className="btn btn-sm"
                    type="button"
                    title={tr(lang, "Copiar log do console", "Copy console log")}
                    onClick={() => {
                      if (!tailText) return;
                      void navigator.clipboard?.writeText(tailText);
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1500);
                    }}
                  >
                    {copied ? <Check size={12} /> : <Copy size={12} />}
                    {copied ? tr(lang, "Copiado", "Copied") : "Log"}
                  </button>
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
