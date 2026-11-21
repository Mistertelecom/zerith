import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  ArrowUp,
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
  Minimize2,
  Paperclip,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  Square,
  SquarePen,
  X,
  type LucideIcon,
} from "lucide-react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "../lib/api";
import { tr, useLang } from "../lib/i18n";
import { Logo } from "./ui";

/// An attachment waiting to go with the next message: images, videos, or any
/// file (configs `.conf`/`.cfg`/`.rsc`, logs, PDFs, `.pcap`, code, archives).
type Attached = {
  name: string;
  path: string;
  url?: string;
  kind: "image" | "video" | "file";
};
const IMAGE_CAP = 20 * 1024 * 1024;
const FILE_CAP = 50 * 1024 * 1024;
const isImagePath = (p: string) => /\.(png|jpe?g|webp|gif)$/i.test(p);
const isVideoPath = (p: string) => /\.(mp4|m4v|webm|mov|mkv)$/i.test(p);

/// What `/` offers. The names are the old window's, so muscle memory carries
/// over; `run` is handled by App, which owns the state each one touches.
export const COMMANDS = [
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
  const before = draft.slice(0, caret);
  const at = before.lastIndexOf(" ");
  const start = at + 1;
  const word = before.slice(start);
  if (word.startsWith("/") && start === 0) {
    return { kind: "slash", start, end: caret, text: word.slice(1) };
  }
  const atSign = word.lastIndexOf("@");
  if (atSign >= 0) {
    return {
      kind: "file",
      start: start + atSign,
      end: caret,
      text: word.slice(atSign + 1),
    };
  }
  return null;
}

export function Composer(props: {
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
}) {
  const lang = useLang();
  const box = useRef<HTMLTextAreaElement>(null);
  const [caret, setCaret] = useState(0);
  const [list, setList] = useState<string[] | null>(null);
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [images, setImages] = useState<Attached[]>([]);
  const [note, setNote] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);
  const [queued, setQueued] = useState(false);
  const picker = useRef<HTMLInputElement>(null);

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
      // insertText keeps Ctrl+Z working and fires React's onChange
      if (text) document.execCommand("insertText", false, text);
      else pasteFiles(e.clipboardData.files);
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
          if (p.type !== "drop") return setDropping(true);
          setDropping(false);
          for (const path of p.paths) {
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
    if (props.busy) {
      const t = props.draft.trim();
      if (t && props.onSteer) props.onSteer(t);
      return;
    }
    if (!images.length) return props.onSend();
    const lines = images.map((i) => {
      const cleanName = i.name.replace(/[[\]\n]/g, "");
      if (i.kind === "image" || i.kind === "video") {
        return `![${cleanName}](<${i.path}>)`;
      }
      return `[Arquivo anexado: ${cleanName}](<${i.path}>)`;
    });
    props.onDraft([props.draft.trim(), ...lines].filter(Boolean).join("\n"));
    setImages([]);
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

  // The file list is asked once, on the first `@`; the workspace walk is
  // cheap but not free, and the popup closes and reopens constantly.
  useEffect(() => {
    if (trigger?.kind !== "file") {
      setList(null);
      return;
    }
    if (list) return;
    void props
      .files()
      .then(setList)
      .catch(() => setList([]));
  }, [trigger?.kind, list, props]);

  useEffect(() => {
    setActive(0);
  }, [trigger?.text]);

  const rows = useMemo(() => {
    if (!trigger) return [];
    const q = trigger.text.toLowerCase();
    if (trigger.kind === "slash") {
      return COMMANDS.filter(([n]) => n.startsWith(q)).map(
        ([n, d]) => `${n}\u0000${d}`,
      );
    }
    return (list ?? [])
      .filter((f) => f.toLowerCase().includes(q))
      .slice(0, 40)
      .map((f) => `${f}\u0000`);
  }, [trigger, list]);

  const accept = (i: number) => {
    if (!trigger || !rows[i]) return;
    const [name] = rows[i].split("\u0000");
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
    props.onDraft(next);
    setCaret(trigger.start + name.length + 2);
    box.current?.focus();
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
  const canSend = !props.busy && (!!props.draft.trim() || images.length > 0);

  return (
    <div className="composer">
      <div className="composer-inner">
        <div
          className={"draft" + (dropping ? " dropping" : "")}
          onDragOver={(e) => {
            if (!e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            setDropping(true);
          }}
          onDragLeave={() => setDropping(false)}
          onDrop={(e) => {
            if (!e.dataTransfer.files.length) return;
            e.preventDefault();
            setDropping(false);
            addFiles(e.dataTransfer.files);
          }}
        >
          {images.length ? (
            <div className="attach-row" aria-label="Anexos">
              {images.map((im, i) => (
                <div className="attach-chip" key={im.path} title={`${im.name} (${im.path})`}>
                  {im.kind === "image" && im.url ? (
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
              ))}
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
                const [name, desc] = r.split("\u0000");
                const Icon =
                  trigger.kind === "slash"
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
            ref={box}
            rows={1}
            value={props.draft}
            aria-label={tr(lang, "Mensagem", "Message")}
            placeholder={tr(lang, "Pergunte algo ao Zerith…", "Ask Zerith anything…")}
            onChange={(e) => {
              props.onDraft(e.target.value);
              setCaret(e.target.selectionStart ?? e.target.value.length);
              setDismissed(null);
            }}
            onPaste={(e) => {
              // Word/Excel put a picture of the copied text beside it: text wins
              const data = e.clipboardData;
              if (!data.files.length || data.getData("text/plain")) return;
              e.preventDefault();
              pasteFiles(data.files);
            }}
            onKeyDown={(e) => {
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
                props.onMode(props.modes[(i + 1) % props.modes.length][0]);
                return;
              }
              if (
                e.key === "ArrowUp" &&
                !props.draft.trim() &&
                props.lastUserText &&
                !(rows.length && trigger)
              ) {
                e.preventDefault();
                props.onDraft(props.lastUserText);
                return;
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
          />
          <div className="draft-row">
            <button
              type="button"
              className="icon-btn sm"
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
            {/* Native selects under a pill: the pill shows the choice, the
                transparent select on top keeps the real control and its menu. */}
            {/* Own menus, not native <select>: WebView2 draws that popup as a
                white OS window that spills out of the frame (and down). */}
            {props.folder}
            <PillMenu
              label={tr(lang, "Permissões", "Permissions")}
              icon={<ShieldCheck size={14} strokeWidth={2} />}
              text={modeLabel}
              value={props.mode}
              options={props.modes.map(([v, label]) => ({ value: v, label }))}
              onPick={props.onMode}
            />
            {props.onEffort ? (
              <PillMenu
                label={tr(lang, "Esforço", "Effort")}
                icon={<Brain size={14} strokeWidth={2} />}
                text={
                  props.effort === "low"
                    ? tr(lang, "Baixo", "Low")
                    : props.effort === "medium"
                      ? tr(lang, "Médio", "Medium")
                      : props.effort === "high"
                        ? tr(lang, "Alto", "High")
                        : "Off"
                }
                value={props.effort ?? "off"}
                options={[
                  { value: "off", label: tr(lang, "Off (padrão)", "Off (default)") },
                  { value: "low", label: tr(lang, "Baixo", "Low") },
                  { value: "medium", label: tr(lang, "Médio", "Medium") },
                  { value: "high", label: tr(lang, "Alto", "High") },
                ]}
                onPick={props.onEffort}
              />
            ) : null}
            {props.onToggleOrchestrator ? (
              <button
                type="button"
                className={"pill" + (props.orchestrator ? " open orch-on" : "")}
                aria-pressed={!!props.orchestrator}
                title={tr(
                  lang,
                  "Modo Orquestrador 100%: o modelo de Pensamento planeja/coordena e o modelo de Execução roda todas as ferramentas",
                  "100% Orchestrator Mode: Thinking model plans/coordinates and Execution model runs all tools",
                )}
                onClick={props.onToggleOrchestrator}
              >
                <Layers size={14} strokeWidth={2} />
                <span className="pill-text">
                  {tr(lang, "Orquestrador", "Orchestrator")}
                </span>
              </button>
            ) : null}
            {!props.orchestrator ? (
              <PillMenu
                label={tr(lang, "Modelo", "Model")}
                icon={
                  provider === "Perfil" ? (
                    <Layers size={16} strokeWidth={1.75} />
                  ) : (
                    <Logo id={provider || null} size={16} />
                  )
                }
                text={brainName}
                value={props.model}
                options={props.models}
                onPick={props.onModel}
                hidden={props.hidden}
                onToggleHidden={props.onToggleHidden}
                search
                wide
              />
            ) : null}
            <span className="spacer" />
            {props.busy ? (
              <div className="send-busy-group">
                {props.draft.trim() && props.onSteer ? (
                  <button
                    className="btn btn-primary send ready steer"
                    onClick={send}
                    aria-label={tr(lang, "Direcionar", "Steer")}
                    title={tr(lang, "Direcionar o turno atual", "Steer current turn")}
                  >
                    <ArrowUp size={16} strokeWidth={2.25} />
                  </button>
                ) : null}
                <button
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
          {props.orchestrator && props.onExecutorModel ? (
            <div className="orch-bar" role="group" aria-label={tr(lang, "Modelos do Orquestrador", "Orchestrator Models")}>
              <span className="orch-tag">
                <Brain size={13} strokeWidth={2} />
                {tr(lang, "Pensamento:", "Thinking:")}
              </span>
              <PillMenu
                label={tr(lang, "Modelo de Pensamento (Cérebro)", "Thinking Model (Brain)")}
                icon={
                  provider === "Perfil" ? (
                    <Layers size={15} strokeWidth={1.75} />
                  ) : (
                    <Logo id={provider || null} size={15} />
                  )
                }
                text={brainName}
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
                {tr(lang, "Execução:", "Execution:")}
              </span>
              <PillMenu
                label={tr(
                  lang,
                  "Modelo de Execução (Subagentes / Ferramentas)",
                  "Execution Model (Subagents / Tools)",
                )}
                icon={
                  execProvider === "Perfil" ? (
                    <Cpu size={15} strokeWidth={1.75} />
                  ) : (
                    <Logo id={execProvider || null} size={15} />
                  )
                }
                text={execName}
                value={execVal}
                options={props.models}
                onPick={props.onExecutorModel}
                hidden={props.hidden}
                onToggleHidden={props.onToggleHidden}
                search
                wide
              />
            </div>
          ) : null}
        </div>
        <div className="hint">
          <span className="hint-text">{note ?? props.hint}</span>
          {props.contextGauge && props.contextGauge.used > 0 ? (() => {
            const pct = Math.min(
              100,
              Math.round((100 * props.contextGauge.used) / Math.max(1, props.contextGauge.limit)),
            );
            const cls = pct >= 85 ? "danger" : pct >= 70 ? "warn" : "";
            return (
              <span
                className={`ctx-gauge ${cls}`}
                title={`${props.contextGauge.used.toLocaleString("pt-BR")} / ${props.contextGauge.limit.toLocaleString("pt-BR")} tokens`}
              >
                <Gauge size={12} strokeWidth={2} />
                {pct}% ({Math.round(props.contextGauge.used / 1000)}k/{Math.round(props.contextGauge.limit / 1000)}k)
                {props.contextGauge.cacheHitPct ? ` · cache ${props.contextGauge.cacheHitPct}%` : ""}
              </span>
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
  );
}

/** A pill that opens a list above itself. Search filters long lists (models). */
function PillMenu(props: {
  label: string;
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
    <div className="pill-menu" ref={root}>
      <button
        type="button"
        className={"pill" + (open ? " open" : "")}
        title={props.label}
        aria-label={props.label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={(e) => {
          // the conversation pane clips the popup at its own top (under the
          // top bar); 96px is the search field, the footer and the gap
          const pane = e.currentTarget.closest(".convo") ?? document.body;
          setRoom(
            e.currentTarget.getBoundingClientRect().top -
              pane.getBoundingClientRect().top -
              96,
          );
          setOpen((o) => !o);
          setQ("");
        }}
      >
        {props.icon}
        <span className="pill-text">{props.text}</span>
        <ChevronDown size={14} strokeWidth={2} className="pill-chev" />
      </button>
      {open ? (
        <div
          className={"popover pill-list" + (props.wide ? " wide" : "")}
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
            style={room ? { maxHeight: `min(360px, 55vh, ${room}px)` } : undefined}
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
