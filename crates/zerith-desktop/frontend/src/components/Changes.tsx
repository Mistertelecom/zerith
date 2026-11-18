// What one answer did to the files: the bar under it, the diff per file and
// the undo. Data comes lazily from `turn_changes` once the answer is stored.
import { useEffect, useState } from "react";
import { AlertTriangle, Check, ChevronRight, FileDiff, FilePen, Undo2 } from "lucide-react";
import { changesApi, type FileChange } from "../lib/api";

// Per answer, for the whole window session: a remount (switching
// conversations, a new turn) never refetches or forgets an undo.
const cache = new Map<string, Promise<FileChange[]>>();
const undone = new Map<string, string>();

type Row = { kind: "hunk" | "add" | "del" | "ctx"; text: string; old?: number; now?: number };

/** Unified hunks into rows with old/new line numbers. */
function rows(diff: string): Row[] {
  const out: Row[] = [];
  let o = 0;
  let n = 0;
  for (const line of diff.split("\n")) {
    if (!line) continue;
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (h) {
      o = Number(h[1]);
      n = Number(h[2]);
      out.push({ kind: "hunk", text: line });
    } else if (line[0] === "+") out.push({ kind: "add", text: line.slice(1), now: n++ });
    else if (line[0] === "-") out.push({ kind: "del", text: line.slice(1), old: o++ });
    else out.push({ kind: "ctx", text: line.slice(1), old: o++, now: n++ });
  }
  return out;
}

const STATUS_TONE = { criado: "ok", alterado: "", apagado: "danger" } as const;
const UNDO_EFFECT = {
  criado: "será apagado (foi criado nesta resposta)",
  alterado: "volta ao conteúdo de antes",
  apagado: "será recriado",
} as const;

function Diff({ file }: { file: FileChange }) {
  if (file.diff == null) return <p className="changes-diff-note">{file.note ?? "Sem diff"}</p>;
  return (
    <div className="changes-diff" role="region" aria-label={"Diff de " + file.rel}>
      {file.truncated ? (
        <p className="changes-diff-note">Arquivo grande demais para comparar linha a linha: trecho alterado inteiro.</p>
      ) : null}
      <div className="diff-lines">
        {rows(file.diff).map((r, i) => (
          <div key={i} className={"diff-line " + r.kind}>
            <span className="diff-ln">{r.old ?? ""}</span>
            <span className="diff-ln">{r.now ?? ""}</span>
            <span className="diff-sign">{r.kind === "add" ? "+" : r.kind === "del" ? "-" : ""}</span>
            <span className="diff-text">{r.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function TurnChanges({ sessionId, messageId }: { sessionId: string; messageId: number }) {
  const key = sessionId + ":" + messageId;
  const [files, setFiles] = useState<FileChange[] | null>(null);
  const [open, setOpen] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(undone.get(key) ?? null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    let req = cache.get(key);
    if (!req) {
      req = changesApi.list(sessionId, messageId);
      cache.set(key, req);
    }
    req
      .then((f) => live && setFiles(f))
      .catch(() => {
        cache.delete(key); // retried on the next mount
        if (live) setFiles([]);
      });
    return () => {
      live = false;
    };
  }, [key, sessionId, messageId]);

  if (!files?.length) return null;
  const added = files.reduce((s, f) => s + f.added, 0);
  const removed = files.reduce((s, f) => s + f.removed, 0);

  const undo = async () => {
    const list = files.map((f) => `• ${f.rel} — ${UNDO_EFFECT[f.status]}`).join("\n");
    if (!window.confirm(`Desfazer as alterações desta resposta?\n\n${list}\n\nA conversa continua como está.`)) return;
    setBusy(true);
    setError(null);
    try {
      let r = await changesApi.undo(sessionId, messageId);
      if (!r.done) {
        const again = window.confirm(
          "Estes arquivos mudaram depois desta resposta:\n\n" +
            r.conflicts.map((c) => "• " + c).join("\n") +
            "\n\nDesfazer mesmo assim apaga essas mudanças mais novas. Continuar?",
        );
        if (!again) {
          setError("Nada foi desfeito: " + r.conflicts.join(", ") + " mudou depois desta resposta.");
          return;
        }
        r = await changesApi.undo(sessionId, messageId, true);
      }
      const n = r.filesRestored + r.filesRemoved;
      const text = `Alterações desfeitas: ${n} arquivo(s)`;
      if (r.errors.length) setError(r.errors.join("; "));
      undone.set(key, text);
      setDone(text);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={"changes" + (open ? " open" : "") + (done ? " undone" : "")}>
      <button type="button" className="changes-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        <FilePen size={14} strokeWidth={2} aria-hidden />
        <span className="changes-title">
          {files.length} {files.length === 1 ? "arquivo alterado" : "arquivos alterados"}
        </span>
        <span className={"changes-count add" + (added ? "" : " zero")}>+{added}</span>
        <span className={"changes-count del" + (removed ? "" : " zero")}>−{removed}</span>
        {done ? <span className="badge">desfeito</span> : null}
        <ChevronRight size={14} strokeWidth={2} className="chev" />
      </button>
      {open ? (
        <div className="changes-body">
          {files.map((f) => {
            const on = shown === f.path;
            return (
              <div key={f.path} className={"change" + (on ? " on" : "")}>
                <button
                  type="button"
                  className="change-row"
                  onClick={() => setShown(on ? null : f.path)}
                  aria-expanded={on}
                  title={f.path}
                >
                  <FileDiff size={14} strokeWidth={2} aria-hidden />
                  <span className="change-path">{f.rel}</span>
                  {f.changedSince ? (
                    <span className="badge warn" title="O arquivo mudou depois desta resposta">
                      mudou depois
                    </span>
                  ) : null}
                  {f.diff == null ? (
                    <span className="changes-count bin">binário</span>
                  ) : (
                    <>
                      <span className={"changes-count add" + (f.added ? "" : " zero")}>+{f.added}</span>
                      <span className={"changes-count del" + (f.removed ? "" : " zero")}>−{f.removed}</span>
                    </>
                  )}
                  <span className={"badge change-status " + STATUS_TONE[f.status]}>{f.status}</span>
                </button>
                {on ? <Diff file={f} /> : null}
              </div>
            );
          })}
          <div className="changes-foot">
            {done ? (
              <span className="changes-note">
                <Check size={14} strokeWidth={2} aria-hidden />
                {done}
              </span>
            ) : (
              <button type="button" className="btn btn-sm" onClick={() => void undo()} disabled={busy}>
                {busy ? <span className="spinner" /> : <Undo2 size={14} strokeWidth={2} aria-hidden />}
                Desfazer alterações
              </button>
            )}
          </div>
          {error ? (
            <p className="changes-error" role="alert">
              <AlertTriangle size={14} strokeWidth={2} aria-hidden />
              {error}
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
