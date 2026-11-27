import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
  type ReactNode,
  type RefObject,
} from "react";
import {
  ArrowUp,
  BookOpen,
  Bot,
  Check,
  Layers,
  Brain,
  ChevronDown,
  Cpu,
  Eye,
  EyeOff,
  FileText,
  Gauge,
  History,
  ListChecks,
  MessageCircleQuestionMark,
  Minimize2,
  Paperclip,
  RotateCcw,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Square,
  SquarePen,
  Target,
  X,
  type LucideIcon,
} from "lucide-react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { measurePillPopover } from "./FolderPill";
import { Logo } from "./ui";

/// An attachment waiting to go with the next message: images, videos, or any
/// file (configs `.conf`/`.cfg`/`.rsc`, logs, PDFs, `.pcap`, code, archives).
type Attached = {
  name: string;
  path: string;
  url?: string;
  kind: "image" | "video" | "file";
};
type PastedSnippet = {
  id: string;
  text: string;
  lines: number;
  chars: number;
  preview: string;
};
const IMAGE_CAP = 20 * 1024 * 1024;
const FILE_CAP = 50 * 1024 * 1024;
const PASTE_LINE_THRESHOLD = 40;
const PASTE_CHAR_THRESHOLD = 4000;
const isImagePath = (p: string) => /\.(png|jpe?g|webp|gif)$/i.test(p);
const isVideoPath = (p: string) => /\.(mp4|m4v|webm|mov)$/i.test(p);

function shouldChipPaste(text: string): boolean {
  if (text.length >= PASTE_CHAR_THRESHOLD) return true;
  let lines = 1;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) {
      lines++;
      if (lines >= PASTE_LINE_THRESHOLD) return true;
    }
  }
  return false;
}

function makePastedSnippet(text: string): PastedSnippet {
  const split = text.split(/\r?\n/);
  const lines = split.length;
  const preview = split
    .slice(0, 3)
    .map((l) => l.trimEnd())
    .join("\n")
    .slice(0, 260);
  return {
    id: `paste-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    text,
    lines,
    chars: text.length,
    preview,
  };
}

/// What `/` offers. The names are the old window's, so muscle memory carries
/// over; `run` is handled by App, which owns the state each one touches.
export const COMMANDS = [
  ["goal", "Trabalhar até um juiz dar o objetivo como cumprido"],
  ["btw", "Pergunta paralela, sem entrar na conversa (funciona com o turno rodando)"],
  ["compact", "Resumir o histórico para caber no contexto"],
  ["context", "Quanto do contexto já foi usado"],
  ["rewind", "Voltar ao último ponto de restauração"],
  ["plan", "Entrar em modo plano"],
  ["mode", "Trocar o modo de permissões"],
  ["model", "Escolher o modelo"],
  ["clear", "Começar uma conversa nova"],
  ["resume", "Abrir o histórico"],
  ["agents", "Ver os agentes"],
  ["skills", "Ver as skills"],
  ["memory", "Ver a memória"],
] as const;

export type CommandName = (typeof COMMANDS)[number][0];

const COMMAND_ICONS: Record<CommandName, LucideIcon> = {
  goal: Target,
  btw: MessageCircleQuestionMark,
  compact: Minimize2,
  context: Gauge,
  rewind: RotateCcw,
  plan: ListChecks,
  mode: ShieldCheck,
  model: Cpu,
  clear: SquarePen,
  resume: History,
  agents: Bot,
  skills: Sparkles,
  memory: Brain,
};

type Trigger = {
  kind: "slash" | "file";
  start: number;
  end: number;
  text: string;
};

/// The token under the caret, if it is something a popup should offer: a
/// leading `/word` at the start of the draft, or an `@word` anywhere.
function tokenAt(draft: string, caret: number): Trigger | null {
  let start = caret;
  while (start > 0 && !/\s/.test(draft[start - 1])) start--;
  const word = draft.slice(start, caret);
  if (word.startsWith("/") && (start === 0 || draft[start - 1] === "\n")) {
    return { kind: "slash", start, end: caret, text: word.slice(1) };
  }
  if (word.startsWith("@")) {
    return {
      kind: "file",
      start,
      end: caret,
      text: word.slice(1),
    };
  }
  return null;
}

/** Compact display name for a model pill: strips redundant `org/` prefix
 * since the provider logo is already rendered right beside it. */
function shortModelName(name: string): string {
  const slash = name.lastIndexOf("/");
  if (slash > 0 && slash < name.length - 1) return name.slice(slash + 1);
  return name;
}

export const Composer = memo(function Composer(props: {
  draft: string;
  onDraft: (v: string) => void;
  onSend: () => void;
  onSteer?: (text: string) => void;
  onRun: (name: CommandName) => void;
  busy: boolean;
  onCancel: () => void;
  mode: string;
  modes: readonly (readonly [string, string])[];
  onMode: (v: string) => void;
  effort?: string;
  onEffort?: (v: string) => void;
  lastUserText?: string;
  userHistory?: string[];
  contextGauge?: { used: number; limit: number; cacheHitPct?: number } | null;
  model: string;
  models: { value: string; label: string }[];
  onModel: (v: string) => void;
  orchestrator?: boolean;
  onToggleOrchestrator?: () => void;
  executorModel?: string;
  onExecutorModel?: (v: string) => void;
  hidden: string[];
  onToggleHidden: (v: string) => void;
  hint: string;
  files: () => Promise<string[]>;
  /** Extra pill at the head of the toolbar (the conversation's folder). */
  folder?: ReactNode;
  /** Optional ref forwarded to the underlying draft textarea. */
  textareaRef?: RefObject<HTMLTextAreaElement | null>;
}) {
  const lang = useLang();
  // Every level is offered; a provider that lacks one gets the strongest it
  // accepts (`max` becomes `high` on OpenAI-compatible servers).
  const effortOptions = [
    { value: "auto", label: tr(lang, "Auto (ajusta esforço conforme a tarefa sem quebrar cache)", "Auto (adapts effort to task without breaking cache)"), short: "Auto" },
    { value: "off", label: tr(lang, "Off (padrão)", "Off (default)"), short: "Off" },
    { value: "low", label: tr(lang, "Baixo", "Low"), short: tr(lang, "Baixo", "Low") },
    { value: "medium", label: tr(lang, "Médio", "Medium"), short: tr(lang, "Médio", "Medium") },
    { value: "high", label: tr(lang, "Alto", "High"), short: tr(lang, "Alto", "High") },
    { value: "xhigh", label: tr(lang, "Extra alto", "Extra high"), short: "XHigh" },
    { value: "max", label: tr(lang, "Máximo (rebaixa se o provedor não aceitar)", "Max (lowered if the provider lacks it)"), short: "Max" },
  ];
  const box = useRef<HTMLTextAreaElement | null>(null);
  const attachTextareaRef = useCallback(
    (el: HTMLTextAreaElement | null) => {
      box.current = el;
      if (props.textareaRef) {
        (props.textareaRef as MutableRefObject<HTMLTextAreaElement | null>).current = el;
      }
    },
    [props.textareaRef],
  );
  const [caret, setCaret] = useState(0);
  const [list, setList] = useState<string[] | null>(null);
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [images, setImages] = useState<Attached[]>([]);
  const [pasted, setPasted] = useState<PastedSnippet[]>([]);
  const [note, setNote] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);
  const [queued, setQueued] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const histIdxRef = useRef<number>(-1);
  const savedDraftRef = useRef<string>("");

  // Saved on attach, not on send: a bad file is refused while the user is
  // still looking at it, and sending stays a plain text prompt.
  const attachImage = (name: string, dataUrl: string) =>
    api.saveAttachment(dataUrl).then(
      (path) => {
        setNote(null);
        setImages((l) =>
          l.some((i) => i.path === path)
            ? l
            : [...l, { name, path, url: dataUrl, kind: "image" }],
        );
      },
      (e) => setNote(String(e)),
    );
  const attachFile = (name: string, dataUrl: string, isVideo: boolean) =>
    api.saveFileAttachment(name, dataUrl).then(
      (path) => {
        setNote(null);
        setImages((l) =>
          l.some((i) => i.path === path)
            ? l
            : [
                ...l,
                {
                  name,
                  path,
                  url: isVideo ? dataUrl : undefined,
                  kind: isVideo ? "video" : "file",
                },
              ],
        );
      },
      (e) => setNote(String(e)),
    );
  const attachExistingPath = (rawPath: string) => {
    const norm = rawPath.replace(/\\/g, "/");
    const name = norm.split("/").pop() ?? "arquivo";
    const kind: Attached["kind"] = isImagePath(norm)
      ? "image"
      : isVideoPath(norm)
        ? "video"
        : "file";
    setNote(null);
    setImages((l) =>
      l.some((i) => i.path === norm) ? l : [...l, { name, path: norm, kind }],
    );
  };
  const addFiles = (files: Iterable<File>) => {
    for (const f of files) {
      const isImg = f.type.startsWith("image/") || isImagePath(f.name);
      const isVid = f.type.startsWith("video/") || isVideoPath(f.name);
      const cap = isImg ? IMAGE_CAP : FILE_CAP;
      if (f.size > cap) {
        setNote(`${f.name}: maior que ${isImg ? "20 MB" : "50 MB"}`);
        continue;
      }
      const r = new FileReader();
      r.onload = () => {
        const res = String(r.result);
        if (isImg) void attachImage(f.name || "imagem", res);
        else void attachFile(f.name || "arquivo", res, isVid);
      };
      r.readAsDataURL(f);
    }
  };
  /** Pasted files: images, videos, configs, logs, PDFs — all attach cleanly. */
  const pasteFiles = (list: FileList) => {
    addFiles([...list]);
  };

  // Ctrl+V with the focus elsewhere (a conversation just picked in the list,
  // a suggestion card, the transcript) went nowhere: it lands in the box.
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const el = box.current;
      const at = e.target as HTMLElement | null;
      if (!el || !e.clipboardData) return;
      if (at?.closest?.("input, textarea, [contenteditable], [role=dialog]")) return;
      e.preventDefault();
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
      const text = e.clipboardData.getData("text/plain");
      if (text) {
        if (shouldChipPaste(text)) {
          setPasted((prev) => [...prev, makePastedSnippet(text)]);
        } else {
          // insertText keeps Ctrl+Z working and fires React's onChange
          document.execCommand("insertText", false, text);
        }
      } else {
        pasteFiles(e.clipboardData.files);
      }
    };
    document.addEventListener("paste", onPaste);
    return () => document.removeEventListener("paste", onPaste);
  }, []);

  // The window takes OS file drops itself (Tauri), so the HTML drop below
  // never sees those files: the webview event hands over paths instead.
  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    try {
      void getCurrentWebview()
        .onDragDropEvent(({ payload: p }) => {
          if (p.type === "leave") return setDropping(false);
          if (document.querySelector('[role="dialog"]')) {
            setDropping(false);
            return;
          }
          if (p.type !== "drop") return setDropping(true);
          setDropping(false);
          for (const path of p.paths) {
            if (isImagePath(path) || isVideoPath(path)) {
              api
                .attachDroppedPath(path)
                .then(({ path: savedPath, data_url }) => {
                  const name =
                    savedPath.split(/[\\/]/).pop() ??
                    (isImagePath(savedPath) ? "imagem" : "vídeo");
                  const kind: Attached["kind"] = isImagePath(savedPath)
                    ? "image"
                    : "video";
                  setNote(null);
                  setImages((l) =>
                    l.some((i) => i.path === savedPath)
                      ? l
                      : [
                          ...l,
                          {
                            name,
                            path: savedPath,
                            kind,
                            url: data_url ?? undefined,
                          },
                        ],
                  );
                })
                .catch(() => {
                  if (isImagePath(path)) {
                    api
                      .readImage(path)
                      .then((url) =>
                        attachImage(path.split(/[\\/]/).pop() ?? "imagem", url),
                      )
                      .catch(() => attachExistingPath(path));
                  } else {
                    attachExistingPath(path);
                  }
                });
            } else {
              attachExistingPath(path);
            }
          }
        })
        .then((u) => (live ? (off = u) : u()));
    } catch {
      // the browser preview has no Tauri window; the HTML drop covers it
    }
    return () => {
      live = false;
      off?.();
    };
  }, []);

  // Images ride as `![nome](<path>)` lines: the providers turn them into
  // image input and the transcript shows them, with no change to the store.
  // App sends its own draft, so the lines go in first and the send waits one
  // render for them.
  const send = () => {
    histIdxRef.current = -1;
    savedDraftRef.current = "";
    if (props.busy) {
      const t = [props.draft.trim(), ...pasted.map((p) => p.text)]
        .filter(Boolean)
        .join("\n\n");
      // `/btw` não direciona o turno: vai à parte, com o turno rodando.
      if (/^\/btw(\s|$)/.test(t)) {
        if (pasted.length) {
          props.onDraft(t);
          setPasted([]);
          setQueued(true);
          return;
        }
        return props.onSend();
      }
      if (t && props.onSteer) {
        if (pasted.length) setPasted([]);
        props.onSteer(t);
      }
      return;
    }
    if (!images.length && !pasted.length) return props.onSend();
    const lines = images.map((i) => {
      const cleanName = i.name.replace(/[[\]\n]/g, "");
      if (i.kind === "image" || i.kind === "video") {
        return `![${cleanName}](<${i.path}>)`;
      }
      return `[Arquivo anexado: ${cleanName}](<${i.path}>)`;
    });
    const textPart = [props.draft.trim(), ...pasted.map((p) => p.text)]
      .filter(Boolean)
      .join("\n\n");
    props.onDraft([textPart, ...lines].filter(Boolean).join("\n"));
    setImages([]);
    setPasted([]);
    setNote(null);
    setQueued(true);
  };
  useEffect(() => {
    if (!queued) return;
    setQueued(false);
    props.onSend();
  }, [queued, props]);

  const trigger = useMemo(
    () => (props.draft === dismissed ? null : tokenAt(props.draft, caret)),
    [props.draft, caret, dismissed],
  );

  // Skills for `/`: asked on the first slash of the window, then kept.
  const [skills, setSkills] = useState<[string, string][] | null>(null);
  useEffect(() => {
    if (trigger?.kind !== "slash" || skills) return;
    void api
      .slashSkills()
      .then(setSkills)
      .catch(() => setSkills([]));
  }, [trigger?.kind, skills]);

  // The file list is asked once, on the first `@`; the workspace walk is
  // cheap but not free, and the popup closes and reopens constantly.
  const prevFilesRef = useRef(props.files);
  const fetchingFilesRef = useRef(false);
  useEffect(() => {
    if (prevFilesRef.current !== props.files) {
      prevFilesRef.current = props.files;
      setList(null);
    }
  }, [props.files]);
  useEffect(() => {
    if (trigger?.kind !== "file") {
      setList(null);
      fetchingFilesRef.current = false;
      return;
    }
    if (list || fetchingFilesRef.current) return;
    let live = true;
    fetchingFilesRef.current = true;
    void props
      .files()
      .then((res) => {
        if (!live) return;
        fetchingFilesRef.current = false;
        setList(res);
      })
      .catch(() => {
        if (!live) return;
        fetchingFilesRef.current = false;
        setList([]);
      });
    return () => {
      live = false;
      fetchingFilesRef.current = false;
    };
  }, [trigger?.kind, list, props.files]);

  useEffect(() => {
    setActive(0);
  }, [trigger?.text]);

  const rows = useMemo(() => {
    if (!trigger) return [];
    const q = trigger.text.toLowerCase();
    if (trigger.kind === "slash") {
      const builtins = COMMANDS.filter(([n]) => n.startsWith(q)).map(
        ([n, d]) => `${n}\u0000${d}`,
      );
      const isCmd = new Set<string>(COMMANDS.map(([n]) => n));
      const fromSkills = (skills ?? [])
        .filter(([n]) => !isCmd.has(n) && n.toLowerCase().includes(q))
        .slice(0, 30)
        .map(([n, d]) => `${n}\u0000${d}\u0000skill`);
      return [...builtins, ...fromSkills];
    }
    return (list ?? [])
      .filter((f) => f.toLowerCase().includes(q))
      .slice(0, 40)
      .map((f) => `${f}\u0000`);
  }, [trigger, list, skills]);

  const accept = (i: number) => {
    if (!trigger || !rows[i]) return;
    const [name, , kind] = rows[i].split("\u0000");
    // Naming the skill in the prompt is what loads it for the model; `/goal`
    // and `/btw` need their text, so they stay in the box for the person to write.
    const typed = name === "goal" || name === "btw";
    if (trigger.kind === "slash" && (kind === "skill" || typed)) {
      const next = typed && kind !== "skill" ? `/${name} ` : `use a skill ${name} `;
      props.onDraft(next);
      setCaret(next.length);
      box.current?.focus();
      requestAnimationFrame(() => box.current?.setSelectionRange(next.length, next.length));
      return;
    }
    if (trigger.kind === "slash") {
      props.onDraft("");
      setCaret(0);
      props.onRun(name as CommandName);
      box.current?.focus();
      return;
    }
    const next =
      props.draft.slice(0, trigger.start) +
      "@" +
      name +
      " " +
      props.draft.slice(trigger.end);
    const nextCaret = trigger.start + name.length + 2;
    props.onDraft(next);
    setCaret(nextCaret);
    box.current?.focus();
    requestAnimationFrame(() =>
      box.current?.setSelectionRange(nextCaret, nextCaret),
    );
  };

  // The draft grows with its text up to the CSS max-height, then scrolls.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [props.draft]);

  const modeLabel =
    props.modes.find(([v]) => v === props.mode)?.[1] ?? props.mode;
  const current = props.models.find((m) => m.value === props.model);
  const provider = current?.label.split(" · ")[0] ?? props.model;
  const brainName =
    current?.label.split(" · ")[1] ??
    (props.model || tr(lang, "Escolha um modelo", "Choose a model"));
  const execVal = props.executorModel || props.model;
  const execCurrent = props.models.find((m) => m.value === execVal);
  const execProvider = execCurrent?.label.split(" · ")[0] ?? execVal;
  const execName =
    execCurrent?.label.split(" · ")[1] ??
    (execVal || tr(lang, "Escolha o executor", "Choose executor"));
  const historyList = props.userHistory?.length
    ? props.userHistory
    : props.lastUserText
      ? [props.lastUserText]
      : [];
  const isBtw = /^\/btw(\s|$)/.test(props.draft.trim());
  const canSend =
    !props.busy && (!!props.draft.trim() || images.length > 0 || pasted.length > 0);

  return (
    <div className="composer">
      <div className="composer-inner">
        <div
          className={"draft" + (dropping ? " dropping" : "")}
          onDragOver={(e) => {
            if (document.querySelector('[role="dialog"]')) return;
            if (!e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            setDropping(true);
          }}
          onDragLeave={() => setDropping(false)}
          onDrop={(e) => {
            if (document.querySelector('[role="dialog"]')) return;
            if (!e.dataTransfer.files.length) return;
            e.preventDefault();
            setDropping(false);
            addFiles(e.dataTransfer.files);
          }}
        >
          {images.length || pasted.length ? (
            <div className="attach-row" aria-label="Anexos">
              {images.map((im, i) => {
                const isImage = im.kind === "image" && Boolean(im.url);
                return (
                  <div
                    className={"attach-chip" + (isImage ? "" : " is-file")}
                    key={im.path}
                    title={`${im.name} (${im.path})`}
                  >
                    {isImage ? (
                      <img src={im.url} alt={im.name} />
                    ) : (
                      <span className="attach-file-label">
                        <FileText size={14} strokeWidth={2} />
                        <span>{im.name}</span>
                      </span>
                    )}
                    <button
                      type="button"
                      className="attach-x"
                      aria-label={`Remover ${im.name}`}
                      title="Remover"
                      onClick={() =>
                        setImages((l) => l.filter((_, j) => j !== i))
                      }
                    >
                      <X size={12} strokeWidth={2.25} />
                    </button>
                  </div>
                );
              })}
              {pasted.map((snip) => {
                const label = tr(
                  lang,
                  `Trecho colado · ${snip.lines} linhas`,
                  `Pasted snippet · ${snip.lines} lines`,
                );
                return (
                  <div
                    className="attach-chip is-file is-paste"
                    key={snip.id}
                    title={snip.preview || label}
                  >
                    <span className="attach-file-label">
                      <FileText size={14} strokeWidth={2} />
                      <span>{label}</span>
                    </span>
                    <button
                      type="button"
                      className="attach-x"
                      aria-label={tr(lang, `Remover ${label}`, `Remove ${label}`)}
                      title={tr(lang, "Remover", "Remove")}
                      onClick={() =>
                        setPasted((l) => l.filter((x) => x.id !== snip.id))
                      }
                    >
                      <X size={12} strokeWidth={2.25} />
                    </button>
                  </div>
                );
              })}
            </div>
          ) : null}
          {trigger && rows.length > 0 ? (
            <div
              className="popover popup"
              role="listbox"
              aria-label={trigger.kind === "slash" ? "Comandos" : "Arquivos"}
            >
              <div className="popup-head">
                {trigger.kind === "slash"
                  ? "Comandos"
                  : "Arquivos do workspace"}
              </div>
              {rows.map((r, i) => {
                const [name, desc, kind] = r.split("\u0000");
                const Icon =
                  kind === "skill"
                    ? BookOpen
                    : trigger.kind === "slash"
                      ? COMMAND_ICONS[name as CommandName]
                      : FileText;
                const cut = name.lastIndexOf("/");
                return (
                  <button
                    key={name + i}
                    role="option"
                    aria-selected={i === active}
                    ref={
                      i === active
                        ? (el) => el?.scrollIntoView({ block: "nearest" })
                        : undefined
                    }
                    className={"popup-row" + (i === active ? " on" : "")}
                    onMouseEnter={() => setActive(i)}
                    onClick={() => accept(i)}
                  >
                    <span className="popup-icon">
                      <Icon size={14} strokeWidth={2} />
                    </span>
                    {trigger.kind === "slash" ? (
                      <>
                        <span className="popup-key">/{name}</span>
                        <span className="popup-desc">{desc}</span>
                      </>
                    ) : (
                      <span className="popup-file">
                        <span className="popup-base">
                          {name.slice(cut + 1)}
                        </span>
                        {cut > 0 ? (
                          <span className="popup-dir">
                            {name.slice(0, cut)}
                          </span>
                        ) : null}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          ) : null}
          <textarea
            ref={attachTextareaRef}
            rows={1}
            value={props.draft}
            aria-label={tr(lang, "Mensagem", "Message")}
            placeholder={
              props.busy
                ? tr(
                    lang,
                    "Direcionar resposta atual ou /btw para pergunta paralela…",
                    "Steer current turn or /btw for a side question…",
                  )
                : tr(lang, "Pergunte algo ao Zerith…", "Ask Zerith anything…")
            }
            onChange={(e) => {
              histIdxRef.current = -1;
              props.onDraft(e.target.value);
              setCaret(e.target.selectionStart ?? e.target.value.length);
              setDismissed(null);
            }}
            onSelect={(e) =>
              setCaret(
                e.currentTarget.selectionStart ?? e.currentTarget.value.length,
              )
            }
            onClick={(e) =>
              setCaret(
                e.currentTarget.selectionStart ?? e.currentTarget.value.length,
              )
            }
            onKeyUp={(e) =>
              setCaret(
                e.currentTarget.selectionStart ?? e.currentTarget.value.length,
              )
            }
            onPaste={(e) => {
              const data = e.clipboardData;
              const text = data.getData("text/plain");
              if (text) {
                if (shouldChipPaste(text)) {
                  e.preventDefault();
                  setPasted((prev) => [...prev, makePastedSnippet(text)]);
                }
                return;
              }
              // Word/Excel put a picture of the copied text beside it: text wins
              if (!data.files.length) return;
              e.preventDefault();
              pasteFiles(data.files);
            }}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              if (rows.length && trigger) {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setActive((i) => (i + 1) % rows.length);
                  return;
                }
                if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setActive((i) => (i - 1 + rows.length) % rows.length);
                  return;
                }
                if (e.key === "Enter" || e.key === "Tab") {
                  e.preventDefault();
                  accept(active);
                  return;
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  setDismissed(props.draft);
                  return;
                }
              }
              if (e.key === "Tab" && e.shiftKey) {
                e.preventDefault();
                const i = props.modes.findIndex(([v]) => v === props.mode);
                const next = props.modes[(i + 1) % props.modes.length][0];
                if (
                  next === "bypass" &&
                  !window.confirm(
                    tr(
                      lang,
                      "Ativar Bypass? O assistente vai rodar ferramentas sem pedir aprovação.",
                      "Enable Bypass? The assistant will run tools without asking.",
                    ),
                  )
                )
                  return;
                props.onMode(next);
                return;
              }
              if (
                e.key === "ArrowUp" &&
                historyList.length > 0 &&
                !(rows.length && trigger)
              ) {
                const el = e.currentTarget;
                const pos = el.selectionStart ?? 0;
                const onFirstLine =
                  el.selectionEnd === pos && !el.value.slice(0, pos).includes("\n");
                if (onFirstLine && (histIdxRef.current >= 0 || !props.draft.trim())) {
                  e.preventDefault();
                  if (histIdxRef.current < 0) {
                    savedDraftRef.current = props.draft;
                    histIdxRef.current = historyList.length - 1;
                  } else if (histIdxRef.current > 0) {
                    histIdxRef.current = Math.min(
                      histIdxRef.current - 1,
                      historyList.length - 1,
                    );
                  } else {
                    return;
                  }
                  const next = historyList[histIdxRef.current];
                  const targetCaret = next.includes("\n") ? 0 : next.length;
                  props.onDraft(next);
                  setCaret(targetCaret);
                  requestAnimationFrame(() =>
                    box.current?.setSelectionRange(targetCaret, targetCaret),
                  );
                  return;
                }
              }
              if (
                e.key === "ArrowDown" &&
                histIdxRef.current >= 0 &&
                !(rows.length && trigger)
              ) {
                const el = e.currentTarget;
                const pos = el.selectionEnd ?? el.value.length;
                const onLastLine =
                  el.selectionStart === pos && !el.value.slice(pos).includes("\n");
                if (onLastLine) {
                  e.preventDefault();
                  if (histIdxRef.current < historyList.length - 1) {
                    histIdxRef.current += 1;
                    const next = historyList[histIdxRef.current];
                    props.onDraft(next);
                    setCaret(next.length);
                    requestAnimationFrame(() =>
                      box.current?.setSelectionRange(next.length, next.length),
                    );
                  } else {
                    histIdxRef.current = -1;
                    const restored = savedDraftRef.current;
                    props.onDraft(restored);
                    setCaret(restored.length);
                    requestAnimationFrame(() =>
                      box.current?.setSelectionRange(restored.length, restored.length),
                    );
                  }
                  return;
                }
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
          />
          {props.orchestrator && props.onExecutorModel ? (
            <div className="orch-bar" role="group" aria-label={tr(lang, "Modelos do Orquestrador", "Orchestrator Models")}>
              <span className="orch-tag">
                <Brain size={13} strokeWidth={2} />
                {tr(lang, "Pensamento", "Thinking")}
              </span>
              <PillMenu
                className="pill-model"
                label={tr(lang, "Modelo de Pensamento (Cérebro)", "Thinking Model (Brain)")}
                titleText={brainName}
                icon={
                  provider === "Perfil" ? (
                    <Layers size={15} strokeWidth={1.75} />
                  ) : (
                    <Logo id={provider || null} size={15} />
                  )
                }
                text={shortModelName(brainName)}
                value={props.model}
                options={props.models}
                onPick={props.onModel}
                hidden={props.hidden}
                onToggleHidden={props.onToggleHidden}
                search
                wide
              />
              <span className="orch-arrow" aria-hidden>→</span>
              <span className="orch-tag">
                <Cpu size={13} strokeWidth={2} />
                {tr(lang, "Execução", "Execution")}
              </span>
              <PillMenu
                className="pill-model"
                label={tr(
                  lang,
                  "Modelo de Execução (Subagentes / Ferramentas)",
                  "Execution Model (Subagents / Tools)",
                )}
                titleText={execName}
                icon={
                  execProvider === "Perfil" ? (
                    <Cpu size={15} strokeWidth={1.75} />
                  ) : (
                    <Logo id={execProvider || null} size={15} />
                  )
                }
                text={shortModelName(execName)}
                value={execVal}
                options={props.models}
                onPick={props.onExecutorModel}
                hidden={props.hidden}
                onToggleHidden={props.onToggleHidden}
                search
                wide
              />
              {props.onToggleOrchestrator ? (
                <button
                  type="button"
                  className="icon-btn sm orch-close"
                  onClick={props.onToggleOrchestrator}
                  aria-label={tr(lang, "Sair do Modo Orquestrador", "Exit Orchestrator Mode")}
                  title={tr(lang, "Sair do Modo Orquestrador", "Exit Orchestrator Mode")}
                >
                  <X size={13} strokeWidth={2} />
                </button>
              ) : null}
            </div>
          ) : null}
          <div className="draft-row">
            <div className="draft-left">
              <button
                type="button"
                className="icon-btn sm draft-attach"
                aria-label={tr(lang, "Anexar arquivo, imagem ou vídeo", "Attach file, image or video")}
                title={tr(
                  lang,
                  "Anexar imagem, vídeo, config (.conf/.cfg/.rsc), log ou arquivo (ou cole / arraste)",
                  "Attach image, video, config, log or file (or paste / drag)",
                )}
                onClick={() => picker.current?.click()}
              >
                <Paperclip size={16} strokeWidth={2} />
              </button>
              <input
                ref={picker}
                type="file"
                multiple
                hidden
                onChange={(e) => {
                  addFiles(e.target.files ?? []);
                  e.target.value = "";
                }}
              />
              {!props.orchestrator ? (
                <PillMenu
                  className="pill-model"
                  label={tr(lang, "Modelo", "Model")}
                  titleText={brainName}
                  icon={
                    provider === "Perfil" ? (
                      <Layers size={15} strokeWidth={1.75} />
                    ) : (
                      <Logo id={provider || null} size={15} />
                    )
                  }
                  text={shortModelName(brainName)}
                  value={props.model}
                  options={props.models}
                  onPick={props.onModel}
                  hidden={props.hidden}
                  onToggleHidden={props.onToggleHidden}
                  search
                  wide
                />
              ) : null}
              {props.onEffort || props.onToggleOrchestrator ? (
                <TurnOptionsMenu
                  lang={lang}
                  effort={props.effort ?? "off"}
                  effortOptions={effortOptions}
                  onEffort={props.onEffort}
                  orchestrator={!!props.orchestrator}
                  onToggleOrchestrator={props.onToggleOrchestrator}
                />
              ) : null}
            </div>
            <div className="draft-right">
              <PillMenu
                className={props.mode === "bypass" ? "pill-mode-bypass" : undefined}
                label={tr(lang, "Permissões", "Permissions")}
                icon={<ShieldCheck size={14} strokeWidth={2} />}
                text={modeLabel}
                value={props.mode}
                options={props.modes.map(([v, label]) => ({ value: v, label }))}
                onPick={props.onMode}
              />
              {props.busy ? (
                <div className="send-busy-group">
                  {(props.draft.trim() || pasted.length > 0) && (isBtw || props.onSteer) ? (
                    <button
                      type="button"
                      className="btn btn-primary send ready steer"
                      onClick={send}
                      aria-label={
                        isBtw
                          ? tr(lang, "Enviar pergunta paralela (/btw)", "Send side question (/btw)")
                          : tr(lang, "Direcionar", "Steer")
                      }
                      title={
                        isBtw
                          ? tr(lang, "Enviar pergunta paralela (/btw)", "Send side question (/btw)")
                          : tr(lang, "Direcionar o turno atual", "Steer current turn")
                      }
                    >
                      {isBtw ? (
                        <MessageCircleQuestionMark size={16} strokeWidth={2} />
                      ) : (
                        <ArrowUp size={16} strokeWidth={2.25} />
                      )}
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="send stop"
                    onClick={props.onCancel}
                    aria-label={tr(lang, "Parar", "Stop")}
                    title={tr(lang, "Parar", "Stop")}
                  >
                    <Square size={12} strokeWidth={2} fill="currentColor" />
                  </button>
                </div>
              ) : (
                <button
                  className={"btn btn-primary send" + (canSend ? " ready" : "")}
                  disabled={!canSend}
                  onClick={send}
                  aria-label={tr(lang, "Enviar", "Send")}
                  title={tr(lang, "Enviar", "Send")}
                >
                  <ArrowUp size={16} strokeWidth={2.25} />
                </button>
              )}
            </div>
          </div>
        </div>
        <div className="hint composer-meta">
          <div className="composer-meta-left">
            {props.folder ? <div className="composer-folder">{props.folder}</div> : null}
            {props.folder && (note ?? props.hint) ? (
              <span className="composer-meta-dot" aria-hidden>·</span>
            ) : null}
            <span className={"hint-text" + (note ? " is-note" : "")}>{note ?? props.hint}</span>
          </div>
          <div className="composer-meta-right">
            {props.contextGauge && props.contextGauge.used > 0 ? (() => {
              const pct = Math.min(
                100,
                Math.round((100 * props.contextGauge.used) / Math.max(1, props.contextGauge.limit)),
              );
              const cls = pct >= 85 ? "danger" : pct >= 70 ? "warn" : "";
              return (
                <button
                  type="button"
                  className={`ctx-gauge${cls ? " " + cls : ""}`}
                  disabled={props.busy}
                  onClick={() => props.onRun("compact")}
                  title={
                    `${props.contextGauge.used.toLocaleString("pt-BR")} / ${props.contextGauge.limit.toLocaleString("pt-BR")} tokens` +
                    tr(lang, " — clique para compactar (/compact)", " — click to compact (/compact)")
                  }
                >
                  <Gauge size={12} strokeWidth={2} />
                  {pct}% ({Math.round(props.contextGauge.used / 1000)}k/{Math.round(props.contextGauge.limit / 1000)}k)
                  {props.contextGauge.cacheHitPct ? ` · cache ${props.contextGauge.cacheHitPct}%` : ""}
                </button>
              );
            })() : null}
            <span className="hint-keys">
              <kbd className="kbd">Shift</kbd>
              <kbd className="kbd">Tab</kbd> {tr(lang, "modo", "mode")}
              <span className="hint-sep" />
              <kbd className="kbd">Ctrl</kbd>
              <kbd className="kbd">K</kbd> {tr(lang, "comandos", "commands")}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
});

/** Compact popover grouping turn-execution controls (reasoning effort +
 * 100% Orchestrator mode) so the composer bar stays single-row and uncluttered. */
function TurnOptionsMenu(props: {
  lang: ReturnType<typeof useLang>;
  effort: string;
  effortOptions: { value: string; label: string; short: string }[];
  onEffort?: (v: string) => void;
  orchestrator: boolean;
  onToggleOrchestrator?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [alignEnd, setAlignEnd] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) =>
      !root.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) =>
      e.key === "Escape" && (e.stopPropagation(), setOpen(false));
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", esc, true);
    };
  }, [open]);

  const activeEffort =
    props.effort && props.effort !== "off"
      ? props.effortOptions.find((o) => o.value === props.effort)?.short ?? props.effort
      : null;
  const badges: string[] = [];
  if (activeEffort) badges.push(activeEffort);
  if (props.orchestrator) badges.push(tr(props.lang, "Orq", "Orch"));
  const hasActive = badges.length > 0;
  const label = tr(props.lang, "Opções de raciocínio e orquestrador", "Reasoning and orchestrator options");

  return (
    <div className="pill-menu" ref={root}>
      <button
        type="button"
        className={
          "pill turn-opts-trigger" +
          (open ? " open" : "") +
          (hasActive ? " has-active" : "")
        }
        title={label}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={(e) => {
          if (!open) {
            const layout = measurePillPopover(e.currentTarget, false);
            setAlignEnd(layout.alignEnd);
          }
          setOpen((o) => !o);
        }}
      >
        <SlidersHorizontal size={14} strokeWidth={2} />
        {hasActive ? (
          <span className="pill-text">{badges.join(" · ")}</span>
        ) : null}
      </button>
      {open ? (
        <div
          className={"popover turn-opts-pop" + (alignEnd ? " align-end" : "")}
          role="dialog"
          aria-label={label}
        >
          {props.onEffort ? (
            <div className="turn-opts-sec">
              <div className="turn-opts-head">
                <Brain size={13} strokeWidth={2} />
                <span>{tr(props.lang, "Esforço de raciocínio", "Reasoning effort")}</span>
              </div>
              <div className="turn-opts-grid" role="radiogroup" aria-label={tr(props.lang, "Esforço", "Effort")}>
                {props.effortOptions.map((o) => {
                  const on = o.value === props.effort;
                  return (
                    <button
                      key={o.value}
                      type="button"
                      role="radio"
                      aria-checked={on}
                      title={o.label}
                      className={
                        "turn-opts-chip" +
                        (on ? " on" : "") +
                        (o.value === "auto" ? " span-all" : "")
                      }
                      onClick={() => props.onEffort?.(o.value)}
                    >
                      {o.short}
                    </button>
                  );
                })}
              </div>
            </div>
          ) : null}
          {props.onToggleOrchestrator ? (
            <button
              type="button"
              className={"turn-opts-toggle" + (props.orchestrator ? " on" : "")}
              role="switch"
              aria-checked={props.orchestrator}
              onClick={props.onToggleOrchestrator}
            >
              <span className="turn-opts-toggle-info">
                <span className="turn-opts-toggle-title">
                  <Layers size={13} strokeWidth={2} />
                  {tr(props.lang, "Modo Orquestrador", "Orchestrator Mode")}
                </span>
                <span className="turn-opts-toggle-desc">
                  {tr(
                    props.lang,
                    "Um modelo planeja e outro executa ferramentas",
                    "One model plans and another executes tools",
                  )}
                </span>
              </span>
              <span className={"switch" + (props.orchestrator ? " on" : "")} aria-hidden>
                <span className="switch-knob" />
              </span>
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** A pill that opens a list above itself. Search filters long lists (models). */
function PillMenu(props: {
  className?: string;
  label: string;
  titleText?: string;
  icon: ReactNode;
  text: string;
  value: string;
  options: { value: string; label: string }[];
  onPick: (v: string) => void;
  search?: boolean;
  wide?: boolean;
  /** Values kept out of the list; the picked one always stays. */
  hidden?: string[];
  onToggleHidden?: (v: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [showHidden, setShowHidden] = useState(false);
  // The list opens upward; on the empty-chat screen the composer sits mid
  // window, so its height is capped by the room above the pill.
  const [room, setRoom] = useState<number | undefined>();
  const [alignEnd, setAlignEnd] = useState(false);
  const hidden = props.hidden ?? [];
  const hiddenCount = props.options.filter(
    (o) => hidden.includes(o.value) && o.value !== props.value,
  ).length;
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) =>
      !root.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) =>
      e.key === "Escape" && (e.stopPropagation(), setOpen(false));
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", esc, true);
    };
  }, [open]);
  const needle = q.trim().toLowerCase();
  const shown = props.options.filter(
    (o) =>
      (showHidden || !hidden.includes(o.value) || o.value === props.value) &&
      (!needle || o.label.toLowerCase().includes(needle)),
  );
  return (
    <div className={"pill-menu" + (props.className ? " " + props.className : "")} ref={root}>
      <button
        type="button"
        className={"pill" + (open ? " open" : "")}
        title={props.titleText ? `${props.label}: ${props.titleText}` : props.label}
        aria-label={props.label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={(e) => {
          // the conversation pane clips the popup at its own top (under the
          // top bar); 96px is the search field, the footer and the gap
          if (!open) {
            const layout = measurePillPopover(e.currentTarget, Boolean(props.wide));
            setRoom(layout.room);
            setAlignEnd(layout.alignEnd);
            setQ("");
          }
          setOpen((o) => !o);
        }}
      >
        {props.icon}
        <span className="pill-text">{props.text}</span>
        <ChevronDown size={14} strokeWidth={2} className="pill-chev" />
      </button>
      {open ? (
        <div
          className={
            "popover pill-list" +
            (props.wide ? " wide" : "") +
            (alignEnd ? " align-end" : "")
          }
          role="listbox"
          aria-label={props.label}
        >
          {props.search ? (
            <input
              className="input pill-search"
              autoFocus
              placeholder="Buscar modelo ou conta…"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
          ) : null}
          <div
            className="pill-options"
            style={
              room !== undefined
                ? { maxHeight: `min(360px, 55vh, ${room}px)` }
                : undefined
            }
          >
            {shown.length === 0 ? (
              <div className="pill-empty">Nada encontrado</div>
            ) : null}
            {shown.map((o) => {
              const [who, what] = o.label.includes(" · ")
                ? o.label.split(" · ")
                : ["", o.label];
              const off = hidden.includes(o.value);
              return (
                <div className={"pill-row" + (off ? " off" : "")} key={o.value}>
                <button
                  type="button"
                  role="option"
                  aria-selected={o.value === props.value}
                  className={
                    "pill-option" + (o.value === props.value ? " on" : "")
                  }
                  onClick={() => {
                    props.onPick(o.value);
                    setOpen(false);
                  }}
                >
                  {who === "Perfil" ? (
                    <Layers
                      size={16}
                      strokeWidth={1.75}
                      className="pill-option-icon"
                    />
                  ) : who ? (
                    <Logo id={who} size={16} />
                  ) : null}
                  <span className="pill-option-text">{what}</span>
                  {who ? <span className="pill-option-who">{who}</span> : null}
                  {o.value === props.value ? (
                    <Check size={14} strokeWidth={2} />
                  ) : null}
                </button>
                {props.onToggleHidden ? (
                  <button
                    type="button"
                    className="pill-hide"
                    title={off ? "Mostrar no chat" : "Ocultar do chat"}
                    aria-label={
                      (off ? "Mostrar no chat: " : "Ocultar do chat: ") + what
                    }
                    onClick={() => props.onToggleHidden?.(o.value)}
                  >
                    {off ? (
                      <Eye size={14} strokeWidth={2} />
                    ) : (
                      <EyeOff size={14} strokeWidth={2} />
                    )}
                  </button>
                ) : null}
                </div>
              );
            })}
          </div>
          {props.onToggleHidden && (hiddenCount > 0 || showHidden) ? (
            <button
              type="button"
              className="pill-foot"
              aria-pressed={showHidden}
              onClick={() => setShowHidden((s) => !s)}
            >
              {showHidden ? (
                <EyeOff size={14} strokeWidth={2} />
              ) : (
                <Eye size={14} strokeWidth={2} />
              )}
              {showHidden
                ? "Esconder ocultos"
                : `Mostrar ocultos (${hiddenCount})`}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
