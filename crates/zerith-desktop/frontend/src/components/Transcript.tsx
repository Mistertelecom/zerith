import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api } from "../lib/api";
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
  Globe,
  ImageOff,
  Info,
  ListTodo,
  Search,
  Terminal,
  Wrench,
  X,
  type LucideIcon,
} from "lucide-react";
import type { Block, ToolRow } from "../lib/turn";
import { ZerithMark } from "./ui";
import { parseBlocks, type Inline, type MdBlock } from "./markdown";

const isWeb = (src: string) => /^(https?:|data:)/i.test(src);
const USER_IMG = /^!\[([^\]\n]*)\]\(<([^>\n]+)>\)\s*$/gm;

/** Full-window view of one image; Esc or a click outside closes it. */
function Lightbox({ src, url, alt, onClose }: { src: string; url: string; alt: string; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const open = () => (isWeb(src) ? window.open(src, "_blank", "noopener") : void api.openImage(src));
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
  const [url, setUrl] = useState<string | null>(isWeb(src) ? src : null);
  const [failed, setFailed] = useState<string | null>(null);
  const [zoom, setZoom] = useState(false);
  useEffect(() => {
    if (isWeb(src)) return;
    let live = true;
    api
      .readImage(src)
      .then((u) => live && setUrl(u))
      .catch((e) => live && setFailed(String(e)));
    return () => {
      live = false;
    };
  }, [src]);
  if (failed) {
    return (
      <span className="md-img-fail" title={failed}>
        <ImageOff size={14} strokeWidth={2} />
        {alt || src}
      </span>
    );
  }
  if (!url) return <span className="md-img loading" aria-label="Carregando imagem" />;
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
        return (
          <div key={i} className="md-code">
            <div className="md-code-bar">
              <span>{b.lang || "texto"}</span>
              <CopyButton text={b.text} label="Copiar" />
            </div>
            <pre>
              <code>{b.text}</code>
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

function Markdown({ text, streaming }: { text: string; streaming: boolean }) {
  const tree = useMemo(() => parseBlocks(text), [text]);
  return <div className={"md" + (streaming ? " streaming" : "")}>{blocks(tree)}</div>;
}

const STATE_LABEL = { run: "executando", ok: "concluída", fail: "falhou" } as const;

function ToolLine({ row }: { row: ToolRow }) {
  const [open, setOpen] = useState(false);
  const Icon = toolIcon(row.name);
  const arg = argSummary(row.args);
  const args = parseArgs(row.args);
  const body = row.preview ?? (args ? JSON.stringify(args, null, 2) : row.args);
  const state = row.ok === undefined ? "run" : row.ok ? "ok" : "fail";
  return (
    <div className={`tool ${state}` + (open ? " open" : "")}>
      <button className="tool-head" onClick={() => setOpen(!open)} aria-expanded={open} disabled={!body}>
        <span className="tool-icon">
          <Icon size={14} strokeWidth={2} />
        </span>
        <span className="tool-name">{row.name}</span>
        <span className="tool-arg" title={arg}>
          {arg}
        </span>
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
      {open && body ? <pre className="tool-body">{body}</pre> : null}
    </div>
  );
}

function AssistantBlock(props: { block: Block & { kind: "assistant" } }) {
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
        {b.text && !b.streaming ? (
          <div className="msg-actions">
            <CopyButton text={b.text} label="Copiar" />
          </div>
        ) : null}
      </div>
    </div>
  );
}

export function Transcript(props: { blocks: Block[]; tail?: React.ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

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
          if (b.kind === "user") {
            // Attached images arrive as `![nome](<path>)` lines (Composer).
            const imgs = [...b.text.matchAll(USER_IMG)];
            const text = imgs.length ? b.text.replace(USER_IMG, "").trim() : b.text;
            return (
              <div key={i} className="msg-user">
                {imgs.length ? (
                  <div className="msg-user-imgs">
                    {imgs.map((m, j) => (
                      <MdImage key={j} src={m[2]} alt={m[1]} />
                    ))}
                  </div>
                ) : null}
                {text}
              </div>
            );
          }
          if (b.kind === "note") {
            return (
              <div key={i} className="msg-note">
                <Info size={14} strokeWidth={2} />
                <span>{b.text}</span>
              </div>
            );
          }
          return <AssistantBlock key={i} block={b} />;
        })}
        {props.tail}
      </div>
    </div>
  );
}
