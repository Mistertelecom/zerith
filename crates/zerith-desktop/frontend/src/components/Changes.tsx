// What one answer did to the files: the bar under it, the diff per file and
// the undo. Data comes lazily from `turn_changes` once the answer is stored.
import { useEffect, useState } from "react";
import { AlertTriangle, Check, ChevronRight, ExternalLink, FilePen, Undo2 } from "lucide-react";
import { api, changesApi, type FileChange } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { FileLangBadge } from "./Transcript";

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
    else if (line.startsWith("\\")) out.push({ kind: "ctx", text: line });
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

type SplitCell = { text: string; ln?: number; kind: "add" | "del" | "ctx" | "empty" };
type SplitRow = { hunk: string } | { left: SplitCell; right: SplitCell };

/** Unified rows paired side by side: a run of deletions sits next to the
 * additions that follow it; context shows on both sides. */
export function splitRows(list: Row[]): SplitRow[] {
  const out: SplitRow[] = [];
  const empty: SplitCell = { text: "", kind: "empty" };
  let i = 0;
  while (i < list.length) {
    const r = list[i];
    if (r.kind === "hunk") {
      out.push({ hunk: r.text });
      i++;
    } else if (r.kind === "ctx") {
      out.push({
        left: { text: r.text, ln: r.old, kind: "ctx" },
        right: { text: r.text, ln: r.now, kind: "ctx" },
      });
      i++;
    } else {
      const dels: Row[] = [];
      const adds: Row[] = [];
      while (i < list.length && list[i].kind === "del") dels.push(list[i++]);
      while (i < list.length && list[i].kind === "add") adds.push(list[i++]);
      const n = Math.max(dels.length, adds.length);
      for (let k = 0; k < n; k++) {
        const d = dels[k];
        const a = adds[k];
        out.push({
          left: d ? { text: d.text, ln: d.old, kind: "del" } : empty,
          right: a ? { text: a.text, ln: a.now, kind: "add" } : empty,
        });
      }
    }
  }
  return out;
}

type DiffMode = "unified" | "split";
const DIFF_MODE_KEY = "zerith.diffMode";
const readMode = (): DiffMode => {
  try {
    return localStorage.getItem(DIFF_MODE_KEY) === "split" ? "split" : "unified";
  } catch {
    return "unified";
  }
};

function Diff({ file, mode }: { file: FileChange; mode: DiffMode }) {
  if (file.diff == null) return <p className="changes-diff-note">{file.note ?? "Sem diff"}</p>;
  const list = rows(file.diff);
  return (
    <div className="changes-diff" role="region" aria-label={"Diff de " + file.rel}>
      {file.truncated ? (
        <p className="changes-diff-note">Arquivo grande demais para comparar linha a linha: trecho alterado inteiro.</p>
      ) : null}
      {mode === "split" ? (
        <div className="diff-split">
          {splitRows(list).map((r, i) =>
            "hunk" in r ? (
              <div key={i} className="diff-split-hunk">
                {r.hunk}
              </div>
            ) : (
              <div key={i} className="diff-split-row">
                <span className={"diff-ln " + r.left.kind}>{r.left.ln ?? ""}</span>
                <span className={"diff-split-cell " + r.left.kind}>{r.left.text}</span>
                <span className={"diff-ln " + r.right.kind}>{r.right.ln ?? ""}</span>
                <span className={"diff-split-cell " + r.right.kind}>{r.right.text}</span>
              </div>
            ),
          )}
        </div>
      ) : (
        <div className="diff-lines">
          {list.map((r, i) => (
            <div key={i} className={"diff-line " + r.kind}>
              <span className="diff-ln">{r.old ?? ""}</span>
              <span className="diff-ln">{r.now ?? ""}</span>
              <span className="diff-sign">{r.kind === "add" ? "+" : r.kind === "del" ? "-" : ""}</span>
              <span className="diff-text">{r.text}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function TurnChanges({ sessionId, messageId }: { sessionId: string; messageId: number }) {
  const lang = useLang();
  const key = sessionId + ":" + messageId;
  const [files, setFiles] = useState<FileChange[] | null>(null);
  const [open, setOpen] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(undone.get(key) ?? null);
  const [error, setError] = useState<string | null>(null);
  const [mode, setModeState] = useState<DiffMode>(readMode);
  const setMode = (m: DiffMode) => {
    setModeState(m);
    try {
      localStorage.setItem(DIFF_MODE_KEY, m);
    } catch {
      /* private mode: keep it for this session only */
    }
  };

  useEffect(() => {
    let live = true;
    setFiles(null);
    setDone(undone.get(key) ?? null);
    setError(null);
    setOpen(false);
    setShown(null);
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

  const undo = async (only?: FileChange) => {
    const pick = only ? [only] : files;
    const list = pick.map((f) => `• ${f.rel} — ${UNDO_EFFECT[f.status]}`).join("\n");
    const ask = only ? "Desfazer a alteração deste arquivo?" : "Desfazer as alterações desta resposta?";
    if (!window.confirm(`${ask}\n\n${list}\n\nA conversa continua como está.`)) return;
    setBusy(true);
    setError(null);
    try {
      let r = await changesApi.undo(sessionId, messageId, false, only?.path);
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
        r = await changesApi.undo(sessionId, messageId, true, only?.path);
      }
      const n = r.filesRestored + r.filesRemoved;
      const text = `Alterações desfeitas: ${n} arquivo(s)`;
      if (r.errors.length) {
        setError(r.errors.join("; "));
      } else if (only && files.length > 1) {
        // The rest of the answer stays: refresh the list instead of closing it.
        const fresh = changesApi.list(sessionId, messageId);
        cache.set(key, fresh);
        setFiles(await fresh);
        setShown(null);
      } else {
        undone.set(key, text);
        setDone(text);
      }
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
          {files.length}{" "}
          {files.length === 1
            ? tr(lang, "arquivo alterado", "file changed")
            : tr(lang, "arquivos alterados", "files changed")}
        </span>
        <span className={"changes-count add" + (added ? "" : " zero")}>+{added}</span>
        <span className={"changes-count del" + (removed ? "" : " zero")}>−{removed}</span>
        {done ? <span className="badge">{tr(lang, "desfeito", "undone")}</span> : null}
        <ChevronRight size={14} strokeWidth={2} className="chev" />
      </button>
      {open ? (
        <div className="changes-body">
          {files.map((f) => {
            const on = shown === f.path;
            return (
              <div key={f.path} className={"change" + (on ? " on" : "")}>
                <div className="tool-row">
                <button
                  type="button"
                  className="change-row"
                  onClick={() => setShown(on ? null : f.path)}
                  aria-expanded={on}
                  title={f.path}
                >
                  <FileLangBadge path={f.rel} />
                  <span className="change-path">{f.rel}</span>
                  {f.changedSince ? (
                    <span
                      className="badge warn"
                      title={tr(lang, "O arquivo mudou depois desta resposta", "File changed after this turn")}
                    >
                      {tr(lang, "mudou depois", "changed later")}
                    </span>
                  ) : null}
                  {f.diff == null ? (
                    <span className="changes-count bin">
                      {f.note?.toLowerCase().includes("grande")
                        ? tr(lang, "grande", "large")
                        : tr(lang, "binário", "binary")}
                    </span>
                  ) : (
                    <>
                      <span className={"changes-count add" + (f.added ? "" : " zero")}>+{f.added}</span>
                      <span className={"changes-count del" + (f.removed ? "" : " zero")}>−{f.removed}</span>
                    </>
                  )}
                  <span className={"badge change-status " + STATUS_TONE[f.status]}>{f.status}</span>
                </button>
                {!done ? (
                  <button
                    type="button"
                    className="tool-open"
                    title={tr(lang, "Desfazer só este arquivo", "Undo this file only")}
                    aria-label={tr(lang, `Desfazer ${f.rel}`, `Undo ${f.rel}`)}
                    disabled={busy}
                    onClick={() => void undo(f)}
                  >
                    <Undo2 size={13} strokeWidth={2} />
                  </button>
                ) : null}
                {f.status !== "apagado" ? (
                  <button
                    type="button"
                    className="tool-open"
                    title={tr(lang, "Abrir no editor (VS Code)", "Open in editor (VS Code)")}
                    aria-label={tr(lang, `Abrir ${f.rel} no editor`, `Open ${f.rel} in editor`)}
                    onClick={() => void api.openInEditor(f.path, sessionId).catch(() => undefined)}
                  >
                    <ExternalLink size={13} strokeWidth={2} />
                  </button>
                ) : null}
                </div>
                {on ? (
                  <>
                    {f.diff != null ? (
                      <div className="diff-mode" role="group" aria-label={tr(lang, "Modo do diff", "Diff view mode")}>
                        <button
                          type="button"
                          className={"diff-mode-btn" + (mode === "unified" ? " on" : "")}
                          aria-pressed={mode === "unified"}
                          onClick={() => setMode("unified")}
                        >
                          {tr(lang, "Unificado", "Unified")}
                        </button>
                        <button
                          type="button"
                          className={"diff-mode-btn" + (mode === "split" ? " on" : "")}
                          aria-pressed={mode === "split"}
                          onClick={() => setMode("split")}
                        >
                          {tr(lang, "Lado a lado", "Side by side")}
                        </button>
                      </div>
                    ) : null}
                    <Diff file={f} mode={mode} />
                  </>
                ) : null}
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
                {tr(lang, "Desfazer alterações", "Undo changes")}
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
