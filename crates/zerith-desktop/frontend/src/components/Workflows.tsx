import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Bell,
  Bot,
  Clock,
  Copy,
  Download,
  FileText,
  GitBranch,
  Globe,
  Hourglass,
  LayoutGrid,
  Maximize2,
  PanelRight,
  Play,
  Plus,
  Redo2,
  RefreshCw,
  Repeat,
  Save,
  Send,
  Server,
  Sliders,
  Sparkles,
  Terminal,
  Trash2,
  Undo2,
  Upload,
  Wand2,
  Webhook,
  Workflow as WorkflowIcon,
  X,
  ZoomIn,
  ZoomOut,
  type LucideIcon,
} from "lucide-react";
import {
  api,
  channelsApi,
  onWorkflowDone,
  onWorkflowEvent,
  onWorkflowsChanged,
  onServerEvent,
  onServerStatus,
  serversApi,
  workflowsApi,
  type AgentView,
  type ServerEntry,
  type ServerStatus,
  type ChannelConfig,
  type ChannelGroup,
  type ChannelKind,
  type NodeEvent,
  type NodeKind,
  type Workflow,
  type WorkflowEdge,
  type WorkflowNode,
  type WorkflowRun,
} from "../lib/api";
import { tr, useLang, type Lang } from "../lib/i18n";
import { BOT_EMBLEMS, BotEmblem } from "./BotEmblem";
import { agoText, secretRefs, ServerDot, ServerSecrets } from "./Servers";
import "../styles/workflows.css";

const NODE_W = 222;
const NODE_META: Record<
  NodeKind,
  {
    pt: string;
    en: string;
    cat: "trigger" | "collect" | "logic" | "deliver";
    color: string;
    icon: LucideIcon;
    defaultCfg: () => Record<string, unknown>;
  }
> = {
  trigger_cron: {
    pt: "Agenda (Cron)",
    en: "Schedule (Cron)",
    cat: "trigger",
    color: "#fbbf24",
    icon: Clock,
    // No tz_offset_minutes: the scheduler follows the OS local offset (DST-aware) per tick.
    defaultCfg: () => ({ cron: "0 8 * * *" }),
  },
  trigger_manual: {
    pt: "Manual",
    en: "Manual Trigger",
    cat: "trigger",
    color: "#fbbf24",
    icon: Play,
    defaultCfg: () => ({}),
  },
  trigger_webhook: {
    pt: "Webhook HTTP",
    en: "HTTP Webhook",
    cat: "trigger",
    color: "#f59e0b",
    icon: Webhook,
    defaultCfg: () => ({ slug: "", secret: "" }),
  },
  shell: {
    pt: "Shell / Script",
    en: "Shell / Script",
    cat: "collect",
    color: "#34d399",
    icon: Terminal,
    defaultCfg: () => ({ command: "echo ok", timeout_secs: 30 }),
  },
  ssh_command: {
    pt: "SSH / NOC",
    en: "SSH / Network",
    cat: "collect",
    color: "#22d3ee",
    icon: Server,
    defaultCfg: () => ({ host: "", user: "root", port: 22, command: "uptime", timeout_secs: 30 }),
  },
  http_request: {
    pt: "HTTP / API",
    en: "HTTP / API",
    cat: "collect",
    color: "#38bdf8",
    icon: Globe,
    defaultCfg: () => ({
      method: "GET",
      url: "https://",
      headers: {},
      body: "",
      allow_error_status: true,
    }),
  },
  transform: {
    pt: "Transformar",
    en: "Transform",
    cat: "logic",
    color: "#a5b4fc",
    icon: Sliders,
    defaultCfg: () => ({ mode: "template", pattern: "", template: "{{prev}}" }),
  },
  condition: {
    pt: "Condição (If)",
    en: "Condition (If)",
    cat: "logic",
    color: "#fdba74",
    icon: GitBranch,
    defaultCfg: () => ({ mode: "not_empty", pattern: "" }),
  },
  ai_agent: {
    pt: "Agente IA",
    en: "AI Agent",
    cat: "logic",
    color: "#c4b5fd",
    icon: Bot,
    defaultCfg: () => ({
      bot_name: "",
      model: "",
      system: "Summarize concisely and highlight actionable issues.",
      prompt: "{{prev}}",
    }),
  },
  delay: {
    pt: "Aguardar (Delay)",
    en: "Delay",
    cat: "logic",
    color: "#94a3b8",
    icon: Hourglass,
    defaultCfg: () => ({ seconds: 5 }),
  },
  loop: {
    pt: "Loop / ForEach",
    en: "Loop / ForEach",
    cat: "logic",
    color: "#60a5fa",
    icon: Repeat,
    defaultCfg: () => ({ mode: "template", items: "", template: "{{item}}", separator: "\n" }),
  },
  notify_channel: {
    pt: "Notificar Canal",
    en: "Notify Channel",
    cat: "deliver",
    color: "#2dd4bf",
    icon: Bell,
    defaultCfg: () => ({ channel_id: "", target: "", message: "{{prev}}" }),
  },
  save_file: {
    pt: "Salvar Arquivo",
    en: "Save File",
    cat: "deliver",
    color: "#cbd5e1",
    icon: FileText,
    defaultCfg: () => ({ path: "reports/report-{{date}}.md", append: false }),
  },
};

const NODE_CATEGORIES: { cat: "trigger" | "collect" | "logic" | "deliver"; pt: string; en: string; kinds: NodeKind[] }[] = [
  { cat: "trigger", pt: "Gatilhos", en: "Triggers", kinds: ["trigger_cron", "trigger_manual", "trigger_webhook"] },
  { cat: "collect", pt: "Coleta & Ação", en: "Collect & Act", kinds: ["shell", "ssh_command", "http_request"] },
  { cat: "logic", pt: "IA & Lógica", en: "AI & Logic", kinds: ["ai_agent", "condition", "transform", "loop", "delay"] },
  { cat: "deliver", pt: "Entrega", en: "Delivery", kinds: ["notify_channel", "save_file"] },
];

const NODE_KINDS: NodeKind[] = [
  "trigger_cron",
  "trigger_manual",
  "trigger_webhook",
  "shell",
  "ssh_command",
  "http_request",
  "transform",
  "condition",
  "loop",
  "delay",
  "ai_agent",
  "notify_channel",
  "save_file",
];

const CHANNEL_KINDS: { kind: ChannelKind; label: string }[] = [
  { kind: "evolution_api", label: "Evolution API v2 (WhatsApp)" },
  { kind: "waha", label: "WAHA (WhatsApp HTTP API)" },
  { kind: "whatsapp_cloud", label: "WhatsApp Cloud API (Meta)" },
  { kind: "telegram", label: "Telegram Bot" },
  { kind: "discord_webhook", label: "Discord Webhook" },
  { kind: "slack_webhook", label: "Slack Webhook" },
  { kind: "generic_webhook", label: "Generic HTTP Webhook" },
  { kind: "desktop_notify", label: "Desktop Notification" },
];

function workflowScheduleLabel(wf: Workflow, lang: Lang): string {
  const cronNode = wf.nodes.find((n) => n.kind === "trigger_cron");
  if (!cronNode) return tr(lang, "Manual", "Manual");
  const cron = String(cronNode.config?.cron ?? "").trim();
  if (!cron) return "Cron";
  if (cron.startsWith("*/")) {
    const step = cron.split(/\s+/)[0]?.slice(2);
    return step ? `${step}m` : cron;
  }
  const mDaily = /^(\d{1,2})\s+(\d{1,2})\s+\*\s+\*\s+\*$/.exec(cron);
  if (mDaily) {
    return `${mDaily[2].padStart(2, "0")}:${mDaily[1].padStart(2, "0")}`;
  }
  return cron;
}

function nodeSubPreview(n: WorkflowNode, channels: ChannelConfig[]): string {
  const c = n.config ?? {};
  switch (n.kind) {
    case "trigger_cron":
      return String(c.cron ?? "0 8 * * *");
    case "trigger_manual":
      return "on-demand";
    case "trigger_webhook":
      return `/webhook/${String(c.slug || n.id)}`;
    case "delay":
      return `${Number(c.seconds ?? 5)}s`;
    case "loop":
      return `${String(c.mode ?? "template")} · foreach`;
    case "shell":
      return String(c.command ?? "");
    case "ssh_command":
      return `${String(c.user ?? "root")}@${String(c.host ?? "host")}: ${String(c.command ?? "")}`;
    case "http_request":
      return `${String(c.method ?? "GET")} ${String(c.url ?? "")}`;
    case "transform":
      return `${String(c.mode ?? "template")} ${String(c.pattern ?? "")}`.trim();
    case "condition":
      return `${String(c.mode ?? "not_empty")} ${String(c.pattern ?? "")}`.trim();
    case "ai_agent": {
      const base = c.bot_name ? `@${String(c.bot_name)}` : String(c.model || "default model");
      return c.use_tools ? `${base} · ⚡ loop` : base;
    }
    case "notify_channel": {
      const ch = channels.find((x) => x.id === c.channel_id);
      return ch ? `${ch.name}${c.target ? " → " + String(c.target) : ""}` : "select channel";
    }
    case "save_file":
      return String(c.path ?? "report.md");
  }
}

function cubicPath(x1: number, y1: number, x2: number, y2: number): string {
  const dx = Math.max(58, Math.abs(x2 - x1) * 0.45);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

function autoArrange(wf: Workflow): Workflow {
  const indeg = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const n of wf.nodes) indeg.set(n.id, 0);
  for (const e of wf.edges) {
    if (indeg.has(e.from) && indeg.has(e.to)) {
      adj.set(e.from, [...(adj.get(e.from) ?? []), e.to]);
      indeg.set(e.to, (indeg.get(e.to) ?? 0) + 1);
    }
  }
  const depth = new Map<string, number>();
  const q: string[] = [];
  for (const [id, d] of indeg.entries()) {
    if (d === 0) {
      q.push(id);
      depth.set(id, 0);
    }
  }
  while (q.length > 0) {
    const u = q.shift()!;
    const d = depth.get(u) ?? 0;
    for (const v of adj.get(u) ?? []) {
      if ((depth.get(v) ?? 0) < d + 1) depth.set(v, d + 1);
      const next = (indeg.get(v) ?? 1) - 1;
      indeg.set(v, next);
      if (next === 0) q.push(v);
    }
  }
  const colCounts = new Map<number, number>();
  const nodes = wf.nodes.map((n) => {
    const col = depth.get(n.id) ?? 0;
    const row = colCounts.get(col) ?? 0;
    colCounts.set(col, row + 1);
    return { ...n, x: 40 + col * 280, y: 120 + row * 150 };
  });
  return { ...wf, nodes };
}

export function WorkflowsStudio(props: {
  models: { value: string; label: string }[];
  onClose: () => void;
  inline?: boolean;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const lang = useLang();
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [channels, setChannels] = useState<ChannelConfig[]>([]);
  const [agents, setAgents] = useState<AgentView[]>([]);
  const [webhookBase, setWebhookBase] = useState("http://127.0.0.1:47891/webhook/");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<Workflow | null>(null);
  const [dirty, setDirty] = useState(false);
  const onDirtyChangeRef = useRef(props.onDirtyChange);
  onDirtyChangeRef.current = props.onDirtyChange;
  useEffect(() => {
    onDirtyChangeRef.current?.(dirty);
  }, [dirty]);
  useEffect(() => () => onDirtyChangeRef.current?.(false), []);
  const lastTextPatchRef = useRef<{ key: string; at: number } | null>(null);
  const pastRef = useRef<Workflow[]>([]);
  const futureRef = useRef<Workflow[]>([]);
  const [histLen, setHistLen] = useState({ undo: 0, redo: 0 });
  const importRef = useRef<HTMLInputElement>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [inspOpen, setInspOpen] = useState(true);
  const [inspTab, setInspTab] = useState<"insp" | "runs">("insp");
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [liveNodes, setLiveNodes] = useState<Record<string, NodeEvent>>({});
  const [connectingFrom, setConnectingFrom] = useState<{
    id: string;
    branch?: string;
    x: number;
    y: number;
  } | null>(null);
  const [mouseWorld, setMouseWorld] = useState<{ x: number; y: number } | null>(null);
  const [quickMenuNode, setQuickMenuNode] = useState<{ id: string; branch?: string } | null>(null);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [emblemMenuOpen, setEmblemMenuOpen] = useState(false);

  const [zoom, setZoom] = useState(0.95);
  const [pan, setPan] = useState<{ x: number; y: number }>({ x: 24, y: 24 });
  const [isPanning, setIsPanning] = useState(false);
  const viewportRef = useRef<HTMLDivElement>(null);

  const [aiPrompt, setAiPrompt] = useState("");
  const [aiBusy, setAiBusy] = useState(false);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [banner, setBanner] = useState<{ text: string; tone: "ok" | "err" | "" } | null>(null);
  const [channelsOpen, setChannelsOpen] = useState(false);
  const [templatesOpen, setTemplatesOpen] = useState(false);
  const [templates, setTemplates] = useState<Workflow[]>([]);
  const [templateErr, setTemplateErr] = useState<string | null>(null);
  const [nodeTestInput, setNodeTestInput] = useState("");
  const [nodeTestOutput, setNodeTestOutput] = useState<string | null>(null);
  const [nodeTestBusy, setNodeTestBusy] = useState(false);
  const [servers, setServers] = useState<ServerEntry[]>([]);
  // 409 do servidor: a versão dele, para recarregar ou sobrescrever.
  const [conflict, setConflict] = useState<Workflow | null>(null);
  // Trocou "Executar em": servidor de onde remover depois de salvar, ou
  // manter uma cópia local ao mandar para um servidor.
  const moveRef = useRef<{ removeFrom?: string; keepLocal?: boolean }>({});

  const loadedIdRef = useRef<string | null>(null);
  const selectedIdRef = useRef(selectedId);
  selectedIdRef.current = selectedId;
  const selectedNodeIdRef = useRef(selectedNodeId);
  selectedNodeIdRef.current = selectedNodeId;
  const langRef = useRef(lang);
  langRef.current = lang;
  const zoomRef = useRef(zoom);
  zoomRef.current = zoom;
  const panRef = useRef(pan);
  panRef.current = pan;
  const dragUndoPushedRef = useRef(false);
  const beforeDragDraftRef = useRef<Workflow | null>(null);

  const dragNodeRef = useRef<{
    id: string;
    startX: number;
    startY: number;
    origX: number;
    origY: number;
  } | null>(null);

  const dragPanRef = useRef<{
    startX: number;
    startY: number;
    origPanX: number;
    origPanY: number;
  } | null>(null);

  const resetPointerDrag = useCallback(() => {
    dragNodeRef.current = null;
    dragPanRef.current = null;
    dragUndoPushedRef.current = false;
    beforeDragDraftRef.current = null;
    setIsPanning(false);
  }, []);

  const fitToView = useCallback((wf: Workflow | null) => {
    const vp = viewportRef.current;
    if (!wf || !wf.nodes.length || !vp) {
      setZoom(0.95);
      setPan({ x: 24, y: 24 });
      return;
    }
    const rect = vp.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of wf.nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + NODE_W);
      maxY = Math.max(maxY, n.y + 100);
    }
    const contentW = Math.max(300, maxX - minX);
    const contentH = Math.max(200, maxY - minY);
    const pad = 64;
    const z = Math.max(
      0.45,
      Math.min(1.05, Math.min((rect.width - pad * 2) / contentW, (rect.height - pad * 2) / contentH)),
    );
    const cx = (rect.width - contentW * z) / 2 - minX * z;
    const cy = (rect.height - contentH * z) / 2 - minY * z;
    setZoom(Number(z.toFixed(2)));
    setPan({ x: Math.round(cx), y: Math.round(cy) });
  }, []);

  const loadAll = useCallback(async (keepSelection = true) => {
    try {
      const [wfs, chs, ags, srvs] = await Promise.all([
        workflowsApi.list(),
        channelsApi.list(),
        api.agentsList().catch(() => [] as AgentView[]),
        serversApi.list().catch(() => [] as ServerEntry[]),
      ]);
      setWorkflows(wfs);
      setServers(srvs);
      setChannels(chs);
      setAgents(ags.filter((a) => !a.disabled));
      setSelectedId((prev) => {
        if (keepSelection && prev && wfs.some((w) => w.id === prev)) return prev;
        return wfs[0]?.id ?? null;
      });
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    }
  }, []);

  useEffect(() => {
    void loadAll(false);
    void workflowsApi.webhookBaseUrl().then(setWebhookBase).catch(() => undefined);
  }, [loadAll]);

  useEffect(() => {
    if (!selectedId) {
      loadedIdRef.current = null;
      setDraft(null);
      setRuns([]);
      return;
    }
    const found = workflows.find((w) => w.id === selectedId);
    if (found && loadedIdRef.current !== selectedId) {
      loadedIdRef.current = selectedId;
      const cloned = structuredClone(found);
      setDraft(cloned);
      pastRef.current = [];
      futureRef.current = [];
      setHistLen({ undo: 0, redo: 0 });
      setSelectedNodeId((prev) =>
        prev && found.nodes.some((n) => n.id === prev) ? prev : found.nodes[0]?.id ?? null,
      );
      requestAnimationFrame(() => fitToView(cloned));
    }
  }, [selectedId, workflows, fitToView]);

  // Servidores: status (bolinha, falhas seguidas, próxima execução) e a lista
  // de lá (última execução, ativo) dos que têm workflows remotos.
  const [srvStatus, setSrvStatus] = useState<Record<string, ServerStatus>>({});
  const [remoteWfs, setRemoteWfs] = useState<Record<string, Workflow>>({});
  const workflowsRef = useRef(workflows);
  workflowsRef.current = workflows;
  const remoteIds = useMemo(
    () => [...new Set(workflows.map((w) => w.run_on).filter((s): s is string => Boolean(s)))].join(","),
    [workflows],
  );
  const refreshRemote = useCallback(async () => {
    const ids = remoteIds ? remoteIds.split(",") : [];
    await Promise.all(
      ids.map(async (sid) => {
        await serversApi
          .health(sid)
          .then((st) => setSrvStatus((m) => ({ ...m, [sid]: st })))
          .catch(() => undefined);
        await serversApi
          .workflows(sid)
          .then((list) =>
            setRemoteWfs((m) => {
              const next = { ...m };
              list.forEach((w) => (next[`${sid}/${w.id}`] = w));
              return next;
            }),
          )
          .catch(() => undefined);
      }),
    );
  }, [remoteIds]);
  useEffect(() => {
    void serversApi
      .status()
      .then((l) => setSrvStatus(Object.fromEntries(l.map((s) => [s.id, s]))))
      .catch(() => undefined);
    void refreshRemote();
  }, [refreshRemote]);

  const fetchRuns = useCallback((id: string) => {
    const sid = workflowsRef.current.find((w) => w.id === id)?.run_on;
    return sid ? serversApi.runs(sid, id, 25) : workflowsApi.runs(id, 25);
  }, []);

  useEffect(() => {
    if (!selectedId) return;
    let live = true;
    void fetchRuns(selectedId)
      .then((r) => {
        if (live) setRuns(r);
      })
      .catch((e) => {
        if (live) {
          setRuns([]);
          setBanner({ text: String(e), tone: "err" });
        }
      });
    return () => {
      live = false;
    };
  }, [selectedId, fetchRuns]);

  // SSE dos servidores (reemitido pelo backend): nós ao vivo e fim de execução.
  useEffect(() => {
    let cancelled = false;
    let offEv = () => {};
    let offSt = () => {};
    void onServerEvent((ev) => {
      if (cancelled) return;
      const wfId = String(ev.data.workflow_id ?? "");
      if (ev.event === "node" && wfId === selectedIdRef.current) {
        const ne = ev.data as unknown as NodeEvent;
        setLiveNodes((prev) => ({ ...prev, [ne.node_id]: ne }));
      }
      if (ev.event === "run_finished") {
        setRunningId((cur) => (cur === wfId ? null : cur));
        void refreshRemote();
        if (wfId === selectedIdRef.current) {
          void fetchRuns(wfId)
            .then((r) => {
              if (!cancelled && wfId === selectedIdRef.current) setRuns(r);
            })
            .catch(() => undefined);
          const ok = ev.data.status === "ok";
          setBanner({
            text: ok
              ? tr(langRef.current, "Execução no servidor concluída", "Server run completed")
              : String(ev.data.summary ?? "erro"),
            tone: ok ? "ok" : "err",
          });
        }
      }
    }).then((fn) => {
      if (cancelled) fn();
      else offEv = fn;
    });
    void onServerStatus((st) => {
      if (!cancelled) setSrvStatus((m) => ({ ...m, [st.id]: st }));
    }).then((fn) => {
      if (cancelled) fn();
      else offSt = fn;
    });
    return () => {
      cancelled = true;
      offEv();
      offSt();
    };
  }, [refreshRemote, fetchRuns]);

  useEffect(() => {
    setNodeTestOutput(null);
  }, [selectedId, selectedNodeId]);

  useEffect(() => {
    let cancelled = false;
    let offEv = () => {};
    let offDone = () => {};
    let offChanged = () => {};
    void onWorkflowEvent((ev) => {
      if (cancelled) return;
      if (ev.workflow_id !== selectedIdRef.current) return;
      setLiveNodes((prev) => ({ ...prev, [ev.node_id]: ev }));
    }).then((fn) => {
      if (cancelled) fn();
      else offEv = fn;
    });
    void onWorkflowDone((run) => {
      if (cancelled) return;
      setRunningId((cur) => (cur === run.workflow_id ? null : cur));
      void loadAll(true);
      if (run.workflow_id === selectedIdRef.current) {
        void fetchRuns(run.workflow_id)
          .then((r) => {
            if (!cancelled && run.workflow_id === selectedIdRef.current) setRuns(r);
          })
          .catch(() => undefined);
      }
      setBanner({
        text: run.status === "ok" ? tr(langRef.current, "Execução concluída", "Run completed") : run.summary,
        tone: run.status === "ok" ? "ok" : "err",
      });
    }).then((fn) => {
      if (cancelled) fn();
      else offDone = fn;
    });
    void onWorkflowsChanged(() => {
      if (cancelled) return;
      void loadAll(true);
    }).then((fn) => {
      if (cancelled) fn();
      else offChanged = fn;
    });
    return () => {
      cancelled = true;
      offEv();
      offDone();
      offChanged();
    };
  }, [loadAll, fetchRuns]);

  const handleUndo = useCallback(() => {
    const prev = pastRef.current.pop();
    if (!prev || !draft) return;
    futureRef.current.push(structuredClone(draft));
    setDraft(prev);
    setDirty(true);
    setHistLen({ undo: pastRef.current.length, redo: futureRef.current.length });
  }, [draft]);

  const handleRedo = useCallback(() => {
    const next = futureRef.current.pop();
    if (!next || !draft) return;
    pastRef.current.push(structuredClone(draft));
    setDraft(next);
    setDirty(true);
    setHistLen({ undo: pastRef.current.length, redo: futureRef.current.length });
  }, [draft]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName;
      const inInput = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
      if (!inInput && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) handleRedo();
        else handleUndo();
        return;
      }
      if (!inInput && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
        e.preventDefault();
        handleRedo();
        return;
      }
      if (e.key === "Escape") {
        if (connectingFrom) setConnectingFrom(null);
        else if (addMenuOpen) setAddMenuOpen(false);
        else if (emblemMenuOpen) setEmblemMenuOpen(false);
        else if (!props.inline && !channelsOpen && !templatesOpen) props.onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    props,
    channelsOpen,
    templatesOpen,
    connectingFrom,
    addMenuOpen,
    emblemMenuOpen,
    handleUndo,
    handleRedo,
  ]);

  const selectedNode = useMemo(
    () => draft?.nodes.find((n) => n.id === selectedNodeId) ?? null,
    [draft, selectedNodeId],
  );

  // recordHistory as string = coalesce key: consecutive text edits to the same
  // field within 800ms share one undo step.
  const patchDraft = (fn: (w: Workflow) => Workflow, recordHistory: boolean | string = true) => {
    let snapshot = recordHistory !== false;
    if (typeof recordHistory === "string") {
      const now = Date.now();
      const last = lastTextPatchRef.current;
      if (last && last.key === recordHistory && now - last.at < 800) snapshot = false;
      lastTextPatchRef.current = { key: recordHistory, at: now };
    } else {
      lastTextPatchRef.current = null;
    }
    setDraft((prev) => {
      if (!prev) return prev;
      if (snapshot) {
        pastRef.current = [...pastRef.current.slice(-39), structuredClone(prev)];
        futureRef.current = [];
        setHistLen({ undo: pastRef.current.length, redo: 0 });
      }
      return fn(prev);
    });
    setDirty(true);
  };

  const handleExportJson = () => {
    if (!draft) return;
    const blob = new Blob([JSON.stringify(draft, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${draft.name.toLowerCase().replace(/[^a-z0-9_-]+/g, "-") || draft.id}.workflow.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleImportJson = async (file: File) => {
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as Partial<Workflow>;
      if (!parsed || typeof parsed !== "object" || !Array.isArray(parsed.nodes)) {
        setBanner({ text: tr(lang, "JSON inválido de workflow.", "Invalid workflow JSON."), tone: "err" });
        return;
      }
      const validNodes = parsed.nodes.every(
        (n): n is WorkflowNode =>
          Boolean(n) &&
          typeof n === "object" &&
          typeof n.id === "string" &&
          n.id.length > 0 &&
          typeof n.kind === "string" &&
          Object.prototype.hasOwnProperty.call(NODE_META, n.kind) &&
          typeof n.x === "number" &&
          Number.isFinite(n.x) &&
          typeof n.y === "number" &&
          Number.isFinite(n.y),
      );
      if (!validNodes) {
        setBanner({
          text: tr(
            lang,
            "JSON inválido: todos os blocos precisam ter id, tipo válido e posições numéricas.",
            "Invalid workflow JSON: every node must have a string id, valid kind, and finite numeric x/y.",
          ),
          tone: "err",
        });
        return;
      }
      const normalizedNodes: WorkflowNode[] = parsed.nodes.map((n) => ({
        id: n.id,
        kind: n.kind,
        label:
          typeof n.label === "string" && n.label
            ? n.label
            : lang === "pt-BR"
              ? NODE_META[n.kind].pt
              : NODE_META[n.kind].en,
        x: n.x,
        y: n.y,
        config: n.config && typeof n.config === "object" ? n.config : NODE_META[n.kind].defaultCfg(),
      }));
      const validEdges = Array.isArray(parsed.edges)
        ? parsed.edges.filter(
            (e): e is WorkflowEdge =>
              Boolean(e) &&
              typeof e === "object" &&
              typeof e.id === "string" &&
              typeof e.from === "string" &&
              typeof e.to === "string",
          )
        : [];
      const now = Date.now();
      const fresh: Workflow = {
        id: `wf-${now}`,
        name: String(parsed.name || file.name.replace(/\.json$/i, "")),
        description: String(parsed.description || ""),
        icon: String(parsed.icon || "workflow"),
        enabled: false,
        bot_name: parsed.bot_name ?? null,
        nodes: normalizedNodes,
        edges: validEdges,
        created_at_ms: now,
        updated_at_ms: now,
        last_run_at_ms: null,
        last_status: null,
      };
      const saved = await workflowsApi.save(fresh);
      setDirty(false);
      await loadAll(false);
      setSelectedId(saved.id);
      setBanner({ text: tr(lang, "Workflow importado!", "Workflow imported!"), tone: "ok" });
    } catch (err) {
      setBanner({ text: String(err), tone: "err" });
    }
  };

  const serverName = (id?: string | null) =>
    servers.find((s) => s.id === id)?.name ?? id ?? tr(lang, "este PC", "this PC");

  /** Depois de gravar: tira do servidor antigo e/ou deixa a cópia local. */
  const finishMove = async (saved: Workflow) => {
    const move = moveRef.current;
    moveRef.current = {};
    if (move.removeFrom) {
      await serversApi.deleteWorkflow(move.removeFrom, saved.id).catch((e) =>
        setBanner({
          text: `${tr(lang, "Não removi do servidor", "Not removed from server")}: ${String(e)}`,
          tone: "err",
        }),
      );
    }
    if (move.keepLocal) {
      const now = Date.now();
      await workflowsApi.save({
        ...structuredClone(saved),
        id: `wf-${now}`,
        name: `${saved.name} (${tr(lang, "este PC", "this PC")})`,
        run_on: null,
        created_at_ms: now,
        updated_at_ms: now,
      });
    }
  };

  const afterSave = async (saved: Workflow) => {
    await finishMove(saved);
    setDirty(false);
    setConflict(null);
    setDraft(structuredClone(saved));
    await loadAll(true);
    setBanner({
      text: saved.run_on
        ? tr(lang, `Salvo e enviado para ${serverName(saved.run_on)}`, `Saved and sent to ${serverName(saved.run_on)}`)
        : tr(lang, "Workflow salvo", "Workflow saved"),
      tone: "ok",
    });
    return saved;
  };

  /** `base`: sobrescrever a versão do servidor depois de um 409. */
  const handleSave = async (base?: number): Promise<Workflow | null> => {
    if (!draft) return null;
    try {
      if (!draft.run_on) return await afterSave(await workflowsApi.save(draft));
      const r = await serversApi.pushWorkflow(draft, base);
      if (r.kind === "conflict") {
        setConflict(r.workflow);
        return null;
      }
      return await afterSave(r.workflow);
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
      return null;
    }
  };

  const reloadFromServer = async () => {
    if (!draft?.run_on) return;
    try {
      const wf = await serversApi.pullWorkflow(draft.run_on, draft.id);
      setConflict(null);
      setDirty(false);
      setDraft(structuredClone(wf));
      await loadAll(true);
      setBanner({ text: tr(lang, "Recarregado do servidor", "Reloaded from server"), tone: "ok" });
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    }
  };

  const changeRunOn = (next: string | null) => {
    if (!draft || (draft.run_on ?? null) === next) return;
    const from = draft.run_on ?? null;
    // Só pergunta quando a origem já tem o workflow salvo.
    const savedHere = workflows.find((w) => w.id === draft.id);
    moveRef.current = {};
    if (savedHere && (savedHere.run_on ?? null) === from) {
      if (from) {
        const msg = tr(
          lang,
          `Remover de ${serverName(from)} ao salvar? (Cancelar = deixa a cópia lá)`,
          `Remove from ${serverName(from)} on save? (Cancel = leave it there)`,
        );
        if (window.confirm(msg)) moveRef.current.removeFrom = from;
      } else {
        const msg = tr(
          lang,
          "Parar de rodar neste PC ao salvar? (Cancelar = mantém uma cópia rodando aqui também)",
          "Stop running it on this PC on save? (Cancel = keep a local copy too)",
        );
        if (!window.confirm(msg)) moveRef.current.keepLocal = true;
      }
    }
    patchDraft((w) => ({ ...w, run_on: next }));
  };

  const handleRunNow = async () => {
    if (!draft) return;
    const saved = dirty ? await handleSave() : draft;
    if (!saved) return;
    setLiveNodes({});
    setRunningId(saved.id);
    setBanner({ text: tr(lang, "Executando fluxo...", "Running workflow..."), tone: "" });
    try {
      // No servidor: o fim chega por `server:event` (run_finished).
      if (saved.run_on) await serversApi.run(saved.run_on, saved.id);
      else await workflowsApi.runNow(saved.id);
    } catch (e) {
      setRunningId(null);
      setBanner({ text: String(e), tone: "err" });
    }
  };

  const handleCreateNew = async () => {
    const now = Date.now();
    const fresh: Workflow = {
      id: `wf-${now}`,
      name: tr(lang, "Nova Automação", "New Automation"),
      description: "",
      icon: "rocket",
      enabled: false,
      nodes: [
        {
          id: "n1",
          kind: "trigger_cron",
          label: tr(lang, "Todo dia 08:00", "Daily at 08:00"),
          x: 60,
          y: 140,
          config: NODE_META.trigger_cron.defaultCfg(),
        },
        {
          id: "n2",
          kind: "shell",
          label: tr(lang, "Coletar dados", "Collect data"),
          x: 350,
          y: 140,
          config: NODE_META.shell.defaultCfg(),
        },
      ],
      edges: [{ id: "e1", from: "n1", to: "n2", branch: null }],
      created_at_ms: now,
      updated_at_ms: now,
    };
    try {
      const saved = await workflowsApi.save(fresh);
      setDirty(false);
      await loadAll(false);
      setSelectedId(saved.id);
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    }
  };

  const handleGenerateAi = async () => {
    const q = aiPrompt.trim();
    if (!q || aiBusy) return;
    setAiBusy(true);
    setBanner({
      text: tr(lang, "IA construindo seu workflow...", "AI is designing your workflow..."),
      tone: "",
    });
    try {
      const wf = await workflowsApi.generateFromPrompt(q);
      setAiPrompt("");
      setDirty(false);
      await loadAll(false);
      setSelectedId(wf.id);
      setBanner({
        text: tr(
          lang,
          "Workflow criado pela IA! Revise os blocos e ative.",
          "Workflow created by AI! Review blocks and activate.",
        ),
        tone: "ok",
      });
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    } finally {
      setAiBusy(false);
    }
  };

  const addStandaloneNode = (kind: NodeKind) => {
    if (!draft) return;
    const id = `n${Date.now().toString(36).slice(-4)}`;
    const tail = draft.nodes[draft.nodes.length - 1];
    const x = tail ? tail.x + 280 : 80;
    const y = tail ? tail.y : 140;
    const meta = NODE_META[kind];
    const node: WorkflowNode = {
      id,
      kind,
      label: lang === "pt-BR" ? meta.pt : meta.en,
      x,
      y,
      config: meta.defaultCfg(),
    };
    patchDraft((w) => ({ ...w, nodes: [...w.nodes, node] }));
    setSelectedNodeId(id);
    setInspOpen(true);
    setAddMenuOpen(false);
  };

  const duplicateNode = (src: WorkflowNode) => {
    const id = `n${Date.now().toString(36).slice(-4)}`;
    const clone: WorkflowNode = {
      ...structuredClone(src),
      id,
      label: `${src.label} (cópia)`,
      x: src.x + 40,
      y: src.y + 125,
    };
    patchDraft((w) => ({ ...w, nodes: [...w.nodes, clone] }));
    setSelectedNodeId(id);
  };

  const quickAddChild = (parentId: string, kind: NodeKind, branch?: string) => {
    if (!draft) return;
    const parent = draft.nodes.find((n) => n.id === parentId);
    if (!parent) return;
    const id = `n${Date.now().toString(36).slice(-4)}`;
    const siblings = draft.edges.filter((e) => e.from === parentId).length;
    const meta = NODE_META[kind];
    const node: WorkflowNode = {
      id,
      kind,
      label: lang === "pt-BR" ? meta.pt : meta.en,
      x: parent.x + 280,
      y: parent.y + (branch === "false" ? 135 : siblings * 115),
      config: meta.defaultCfg(),
    };
    const edge: WorkflowEdge = {
      id: `e${Date.now().toString(36).slice(-4)}`,
      from: parentId,
      to: id,
      branch: branch ?? null,
    };
    patchDraft((w) => ({
      ...w,
      nodes: [...w.nodes, node],
      edges: [...w.edges, edge],
    }));
    setSelectedNodeId(id);
    setInspOpen(true);
    setQuickMenuNode(null);
  };

  const deleteNode = (nodeId: string) => {
    patchDraft((w) => ({
      ...w,
      nodes: w.nodes.filter((n) => n.id !== nodeId),
      edges: w.edges.filter((e) => e.from !== nodeId && e.to !== nodeId),
    }));
    if (selectedNodeId === nodeId) setSelectedNodeId(null);
  };

  const connectEdge = (toId: string) => {
    if (!connectingFrom || connectingFrom.id === toId) {
      setConnectingFrom(null);
      return;
    }
    const exists = draft?.edges.some(
      (e) =>
        e.from === connectingFrom.id &&
        e.to === toId &&
        (e.branch ?? null) === (connectingFrom.branch ?? null),
    );
    if (!exists) {
      const edge: WorkflowEdge = {
        id: `e${Date.now().toString(36).slice(-5)}`,
        from: connectingFrom.id,
        to: toId,
        branch: connectingFrom.branch ?? null,
      };
      patchDraft((w) => ({ ...w, edges: [...w.edges, edge] }));
    }
    setConnectingFrom(null);
  };

  const openTemplates = async () => {
    setTemplateErr(null);
    try {
      const t = await workflowsApi.templates();
      setTemplates(t);
      setTemplatesOpen(true);
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    }
  };

  const instantiateTemplate = async (tpl: Workflow) => {
    setTemplateErr(null);
    const now = Date.now();
    const copy: Workflow = {
      ...structuredClone(tpl),
      id: `wf-${now}`,
      created_at_ms: now,
      updated_at_ms: now,
      enabled: false,
      last_run_at_ms: null,
      last_status: null,
    };
    try {
      const saved = await workflowsApi.save(copy);
      setTemplatesOpen(false);
      setDirty(false);
      await loadAll(false);
      setSelectedId(saved.id);
    } catch (e) {
      const msg = String(e);
      setTemplateErr(msg);
      setBanner({ text: msg, tone: "err" });
    }
  };

  const runSingleNodeTest = async (targetNode?: WorkflowNode) => {
    const nodeToTest = targetNode ?? selectedNode;
    if (!nodeToTest || nodeTestBusy) return;
    const wfId = selectedId;
    selectedNodeIdRef.current = nodeToTest.id;
    setSelectedNodeId(nodeToTest.id);
    setInspOpen(true);
    setInspTab("insp");
    setNodeTestBusy(true);
    setNodeTestOutput(null);
    try {
      const out = await workflowsApi.testNode(nodeToTest, nodeTestInput);
      if (selectedIdRef.current === wfId && selectedNodeIdRef.current === nodeToTest.id) {
        setNodeTestOutput(out || "(sem saída)");
      }
    } catch (e) {
      if (selectedIdRef.current === wfId && selectedNodeIdRef.current === nodeToTest.id) {
        setNodeTestOutput(`Erro: ${String(e)}`);
      }
    } finally {
      setNodeTestBusy(false);
    }
  };

  useEffect(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = vp.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      const curZoom = zoomRef.current;
      const curPan = panRef.current;
      const factor = e.deltaY < 0 ? 1.1 : 0.9;
      const nextZoom = Math.max(0.35, Math.min(1.8, +(curZoom * factor).toFixed(3)));
      const scaleRatio = nextZoom / curZoom;
      const nextPan = {
        x: mx - (mx - curPan.x) * scaleRatio,
        y: my - (my - curPan.y) * scaleRatio,
      };
      zoomRef.current = nextZoom;
      panRef.current = nextPan;
      setPan(nextPan);
      setZoom(nextZoom);
    };
    vp.addEventListener("wheel", onWheel, { passive: false });
    return () => vp.removeEventListener("wheel", onWheel);
  }, []);

  const zoomAtCenter = (nextZ: number) => {
    const vp = viewportRef.current;
    const clamped = Math.max(0.35, Math.min(1.8, +nextZ.toFixed(2)));
    if (!vp) {
      setZoom(clamped);
      return;
    }
    const rect = vp.getBoundingClientRect();
    const cx = rect.width / 2;
    const cy = rect.height / 2;
    const ratio = clamped / zoom;
    setPan({
      x: cx - (cx - pan.x) * ratio,
      y: cy - (cy - pan.y) * ratio,
    });
    setZoom(clamped);
  };

  // Minimap bounds computation
  const minimapData = useMemo(() => {
    if (!draft?.nodes.length) return null;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of draft.nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + NODE_W);
      maxY = Math.max(maxY, n.y + 100);
    }
    minX -= 80;
    minY -= 80;
    maxX += 80;
    maxY += 80;
    const w = Math.max(400, maxX - minX);
    const h = Math.max(250, maxY - minY);
    const scale = Math.min(156 / w, 94 / h);
    return { minX, minY, scale };
  }, [draft]);

  const studio = (
    <div
      className={
        "wf-studio" +
        (props.inline ? " inline" : "") +
        (!inspOpen ? " no-insp" : "")
      }
      role="dialog"
      aria-label={tr(lang, "Estúdio de Workflows", "Workflows Studio")}
      onMouseDown={(e) => e.stopPropagation()}
    >
      {/* Left Roster Rail (OpenMausBot style) */}
      <aside className="wf-rail">
        <div className="wf-rail-head">
          <div className="wf-rail-title">
            <WorkflowIcon size={16} strokeWidth={2} />
            <span>{tr(lang, "Automações", "Workflows")}</span>
          </div>
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void handleCreateNew()}
            title={tr(lang, "Criar novo fluxo", "Create new workflow")}
          >
            <Plus size={14} strokeWidth={2} />
          </button>
        </div>

        <div className="wf-rail-list">
          {workflows.map((local) => {
            // Remoto: status e ativo vêm do servidor (a cópia local é a de edição).
            const sid = local.run_on;
            const remote = sid ? remoteWfs[`${sid}/${local.id}`] : undefined;
            const w = remote ? { ...local, enabled: remote.enabled, last_status: remote.last_status, last_run_at_ms: remote.last_run_at_ms } : local;
            const st = sid ? srvStatus[sid] : undefined;
            const fails = st?.daemon?.consecutive_failures?.[w.id] ?? 0;
            const next = st?.daemon?.next_run?.workflow_id === w.id ? st.daemon.next_run.at_ms : null;
            const isRunning = runningId === w.id || w.last_status === "running";
            const status = isRunning
              ? "running"
              : w.last_status === "error" || fails > 0
                ? "error"
                : w.enabled
                  ? "on"
                  : "idle";
            const select = () => {
              if (
                dirty &&
                !window.confirm(
                  tr(lang, "Descartar alterações não salvas?", "Discard unsaved changes?"),
                )
              ) {
                return;
              }
              setDirty(false);
              setLiveNodes({});
              setSelectedId(w.id);
            };
            return (
              <div
                key={w.id}
                className={"wf-roster-card" + (selectedId === w.id ? " on" : "")}
                role="button"
                tabIndex={0}
                aria-pressed={selectedId === w.id}
                onClick={select}
                onKeyDown={(e) => {
                  if (e.target !== e.currentTarget) return;
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    select();
                  }
                }}
              >
                <BotEmblem icon={w.icon} size={34} status={status} />
                <div className="wf-roster-meta">
                  <span className="wf-roster-name">{w.name}</span>
                  <span className="wf-roster-sub">
                    <span>{workflowScheduleLabel(w, lang)}</span>
                    <span>· {w.nodes.length} {tr(lang, "nós", "nodes")}</span>
                  </span>
                  {sid ? (
                    <span className="wf-roster-sub wf-remote" data-remote={sid}>
                      <ServerDot status={st} />
                      <span>{serverName(sid)}</span>
                      {w.last_run_at_ms ? (
                        <span title={tr(lang, "Última execução", "Last run")}>
                          · {w.last_status ?? "?"} {agoText(w.last_run_at_ms)}
                        </span>
                      ) : null}
                      {fails > 0 ? (
                        <span className="wf-remote-bad">
                          · {fails} {tr(lang, fails === 1 ? "falha" : "falhas seguidas", fails === 1 ? "failure" : "failures in a row")}
                        </span>
                      ) : null}
                      {next ? (
                        <span title={tr(lang, "Próxima execução", "Next run")}>
                          · {tr(lang, "próx.", "next")} {new Date(next).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                        </span>
                      ) : null}
                    </span>
                  ) : null}
                </div>
                <input
                  type="checkbox"
                  className="wf-switch"
                  checked={w.enabled}
                  aria-label={
                    sid
                      ? tr(lang, "Pausar/ativar no servidor", "Pause/enable on server")
                      : tr(lang, "Ativar workflow", "Enable workflow")
                  }
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => {
                    const next = e.target.checked;
                    const call = sid
                      ? serversApi.setEnabled(sid, w.id, next).then(() => refreshRemote())
                      : workflowsApi.toggle(w.id, next);
                    void call
                      .then(() => {
                        setDraft((prev) =>
                          prev && prev.id === w.id ? { ...prev, enabled: next } : prev,
                        );
                        return loadAll(true);
                      })
                      .catch((err) => setBanner({ text: String(err), tone: "err" }));
                  }}
                />
              </div>
            );
          })}
        </div>

        <div className="wf-rail-foot">
          <button type="button" className="btn btn-sm" onClick={() => void openTemplates()}>
            <LayoutGrid size={14} strokeWidth={2} />
            <span>{tr(lang, "Galeria de Templates", "Template Gallery")}</span>
          </button>
          <button type="button" className="btn btn-sm" onClick={() => setChannelsOpen(true)}>
            <Send size={14} strokeWidth={2} />
            <span>
              {tr(lang, "Canais", "Channels")} ({channels.length})
            </span>
          </button>
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => importRef.current?.click()}
            title={tr(lang, "Importar workflow JSON", "Import workflow JSON")}
          >
            <Upload size={14} strokeWidth={2} />
            <span>{tr(lang, "Importar JSON", "Import JSON")}</span>
          </button>
          <input
            ref={importRef}
            type="file"
            accept=".json,application/json"
            style={{ display: "none" }}
            onChange={(e) => {
              const f = e.target.files?.[0];
              e.target.value = "";
              if (f) void handleImportJson(f);
            }}
          />
        </div>
      </aside>

      {/* Center Visual Studio */}
      <section className="wf-main">
        <div className="wf-topbar">
          {draft ? (
            <>
              <div style={{ position: "relative" }}>
                <button
                  type="button"
                  className="wf-emblem-btn"
                  title={tr(lang, "Trocar símbolo do workflow", "Change workflow emblem")}
                  onClick={() => setEmblemMenuOpen((v) => !v)}
                >
                  <BotEmblem
                    icon={draft.icon}
                    size={30}
                    status={runningId === draft.id ? "running" : draft.enabled ? "on" : "idle"}
                  />
                </button>
                {emblemMenuOpen ? (
                  <div className="wf-emblem-pop">
                    {BOT_EMBLEMS.map((em) => (
                      <button
                        key={em.id}
                        type="button"
                        className={"wf-emblem-choice" + (draft.icon === em.id ? " on" : "")}
                        title={em.label}
                        onClick={() => {
                          patchDraft((w) => ({ ...w, icon: em.id }));
                          setEmblemMenuOpen(false);
                        }}
                      >
                        <BotEmblem icon={em.id} size={34} />
                      </button>
                    ))}
                  </div>
                ) : null}
              </div>
              <input
                className="wf-name-input"
                value={draft.name}
                onChange={(e) => patchDraft((w) => ({ ...w, name: e.target.value }), "name")}
                aria-label={tr(lang, "Nome do workflow", "Workflow name")}
              />
              <select
                className="wf-bot-select"
                value={draft.bot_name ?? ""}
                onChange={(e) =>
                  patchDraft((w) => ({
                    ...w,
                    bot_name: e.target.value ? e.target.value : null,
                  }))
                }
                title={tr(
                  lang,
                  "Bot responsável: recebe resumos automáticos no chat e aparece na página do bot",
                  "Owning bot: receives automatic run reports in chat and links on the bot screen",
                )}
              >
                <option value="">{tr(lang, "Sem bot vinculado", "No linked bot")}</option>
                {agents.map((a) => (
                  <option key={a.name} value={a.name}>
                    @{a.name}
                  </option>
                ))}
              </select>
              <select
                className="wf-bot-select wf-run-on"
                value={draft.run_on ?? ""}
                onChange={(e) => changeRunOn(e.target.value || null)}
                aria-label={tr(lang, "Executar em", "Run on")}
                title={tr(
                  lang,
                  "Onde o workflow roda. Servidores: Ajustes > Servidores.",
                  "Where the workflow runs. Servers: Settings > Servers.",
                )}
              >
                <option value="">{tr(lang, "Executar em: Este PC", "Run on: This PC")}</option>
                {servers.map((s) => (
                  <option key={s.id} value={s.id}>
                    {tr(lang, "Executar em", "Run on")}: {s.name}
                  </option>
                ))}
                {draft.run_on && !servers.some((s) => s.id === draft.run_on) ? (
                  <option value={draft.run_on}>
                    {tr(lang, "Servidor removido", "Removed server")} ({draft.run_on})
                  </option>
                ) : null}
              </select>
              <div className="wf-add-pop-wrap">
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => setAddMenuOpen((v) => !v)}
                >
                  <Plus size={14} strokeWidth={2} />
                  <span>{tr(lang, "Bloco", "Add Block")}</span>
                </button>
                {addMenuOpen ? (
                  <div className="wf-add-pop">
                    {NODE_CATEGORIES.map((grp) => (
                      <div key={grp.cat}>
                        <div className="wf-add-cat">{lang === "pt-BR" ? grp.pt : grp.en}</div>
                        {grp.kinds.map((k) => {
                          const m = NODE_META[k];
                          const KIcon = m.icon;
                          return (
                            <button
                              key={k}
                              type="button"
                              className="wf-quick-item"
                              style={{ width: "100%" }}
                              onClick={() => addStandaloneNode(k)}
                            >
                              <KIcon size={14} style={{ color: m.color }} />
                              <span>{lang === "pt-BR" ? m.pt : m.en}</span>
                            </button>
                          );
                        })}
                      </div>
                    ))}
                  </div>
                ) : null}
              </div>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void handleRunNow()}
                disabled={Boolean(runningId)}
              >
                <Play size={14} strokeWidth={2} />
                <span>{tr(lang, "Executar", "Run Now")}</span>
              </button>
              <button
                type="button"
                className={"btn btn-sm" + (dirty ? " btn-primary" : "")}
                onClick={() => void handleSave()}
              >
                <Save size={14} strokeWidth={2} />
                <span>{tr(lang, "Salvar", "Save")}</span>
              </button>
              <button
                type="button"
                className="icon-btn sm"
                title={tr(lang, "Exportar JSON", "Export JSON")}
                onClick={handleExportJson}
              >
                <Download size={14} strokeWidth={2} />
              </button>
              <button
                type="button"
                className="icon-btn sm"
                title={tr(lang, "Excluir workflow", "Delete workflow")}
                onClick={async () => {
                  if (
                    !window.confirm(
                      tr(lang, `Excluir "${draft.name}"?`, `Delete "${draft.name}"?`),
                    )
                  ) {
                    return;
                  }
                  try {
                    if (
                      draft.run_on &&
                      window.confirm(
                        tr(
                          lang,
                          `Excluir também em ${serverName(draft.run_on)}?`,
                          `Also delete on ${serverName(draft.run_on)}?`,
                        ),
                      )
                    ) {
                      await serversApi.deleteWorkflow(draft.run_on, draft.id);
                    }
                    await workflowsApi.delete(draft.id);
                    setDirty(false);
                    await loadAll(false);
                    setBanner({ text: tr(lang, "Workflow excluído", "Workflow deleted"), tone: "ok" });
                  } catch (e) {
                    setBanner({ text: String(e), tone: "err" });
                  }
                }}
              >
                <Trash2 size={14} strokeWidth={2} />
              </button>
            </>
          ) : (
            <span className="wf-name-input">
              {tr(lang, "Selecione ou crie um workflow", "Select or create a workflow")}
            </span>
          )}
          {!props.inline ? (
            <button
              type="button"
              className="icon-btn sm"
              onClick={props.onClose}
              aria-label={tr(lang, "Fechar", "Close")}
            >
              <X size={16} strokeWidth={2} />
            </button>
          ) : null}
        </div>

        <div className="wf-ai-bar">
          <Sparkles size={15} strokeWidth={2} style={{ color: "#c4b5fd", flexShrink: 0 }} />
          <input
            className="wf-ai-input"
            value={aiPrompt}
            onChange={(e) => setAiPrompt(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void handleGenerateAi()}
            placeholder={tr(
              lang,
              "Descreva sua automação: ex. 'Me mande relatório de equipamentos offline no WhatsApp todo dia às 08h'",
              "Describe your automation: e.g. 'Check offline devices daily at 08:00 and send a report to WhatsApp'",
            )}
          />
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void handleGenerateAi()}
            disabled={aiBusy || !aiPrompt.trim()}
          >
            <Sparkles size={13} strokeWidth={2} />
            <span>
              {aiBusy ? tr(lang, "Criando...", "Building...") : tr(lang, "Criar com IA", "Build with AI")}
            </span>
          </button>
        </div>

        {/* Interactive Infinite Canvas */}
        <div
          ref={viewportRef}
          className={"wf-canvas-viewport" + (isPanning ? " panning" : "")}
          style={{
            backgroundPosition: `${pan.x}px ${pan.y}px`,
            backgroundSize: `${22 * zoom}px ${22 * zoom}px`,
          }}
          onClick={() => {
            setConnectingFrom(null);
            setQuickMenuNode(null);
            setAddMenuOpen(false);
            setEmblemMenuOpen(false);
          }}
          onPointerDown={(e) => {
            if ((e.target as HTMLElement).closest(".wf-node, .wf-float-controls, .wf-minimap")) {
              return;
            }
            e.currentTarget.setPointerCapture(e.pointerId);
            setIsPanning(true);
            dragPanRef.current = {
              startX: e.clientX,
              startY: e.clientY,
              origPanX: pan.x,
              origPanY: pan.y,
            };
          }}
          onPointerMove={(e) => {
            if (connectingFrom && viewportRef.current) {
              const r = viewportRef.current.getBoundingClientRect();
              setMouseWorld({
                x: (e.clientX - r.left - pan.x) / zoom,
                y: (e.clientY - r.top - pan.y) / zoom,
              });
            }
            if (dragPanRef.current) {
              const dp = dragPanRef.current;
              setPan({
                x: Math.round(dp.origPanX + (e.clientX - dp.startX)),
                y: Math.round(dp.origPanY + (e.clientY - dp.startY)),
              });
              return;
            }
            const dn = dragNodeRef.current;
            if (!dn) return;
            const dx = e.clientX - dn.startX;
            const dy = e.clientY - dn.startY;
            if (!dragUndoPushedRef.current) {
              if (Math.hypot(dx, dy) <= 2) return;
              dragUndoPushedRef.current = true;
              if (beforeDragDraftRef.current) {
                pastRef.current = [...pastRef.current.slice(-39), beforeDragDraftRef.current];
                futureRef.current = [];
                setHistLen({ undo: pastRef.current.length, redo: 0 });
              }
              beforeDragDraftRef.current = null;
            }
            const nx = Math.round(dn.origX + dx / zoom);
            const ny = Math.round(dn.origY + dy / zoom);
            patchDraft(
              (w) => ({
                ...w,
                nodes: w.nodes.map((n) => (n.id === dn.id ? { ...n, x: nx, y: ny } : n)),
              }),
              false,
            );
          }}
          onPointerUp={resetPointerDrag}
          onPointerCancel={resetPointerDrag}
        >
          {draft ? (
            <div
              className="wf-canvas-world"
              style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` }}
            >
              <svg className="wf-svg-layer" aria-hidden>
                {draft.edges.map((e) => {
                  const from = draft.nodes.find((n) => n.id === e.from);
                  const to = draft.nodes.find((n) => n.id === e.to);
                  if (!from || !to) return null;
                  const yOffset =
                    from.kind === "condition" ? (e.branch === "false" ? 62 : 36) : 49;
                  const x1 = from.x + NODE_W;
                  const y1 = from.y + yOffset;
                  const x2 = to.x;
                  const y2 = to.y + 49;
                  const d = cubicPath(x1, y1, x2, y2);
                  const isActiveRun =
                    liveNodes[from.id]?.status === "running" ||
                    liveNodes[to.id]?.status === "running";
                  const cls =
                    "wf-edge-line" +
                    (e.branch === "true"
                      ? " branch-true"
                      : e.branch === "false"
                        ? " branch-false"
                        : "") +
                    (isActiveRun ? " running" : "");
                  const mx = (x1 + x2) / 2;
                  const my = (y1 + y2) / 2;
                  return (
                    <g
                      key={e.id}
                      className="wf-edge-group"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        patchDraft((w) => ({
                          ...w,
                          edges: w.edges.filter((x) => x.id !== e.id),
                        }));
                      }}
                    >
                      <path className="wf-edge-hit" d={d}>
                        <title>{tr(lang, "Clique para desconectar", "Click to disconnect")}</title>
                      </path>
                      <path className={cls} d={d} />
                      {e.branch ? (
                        <g transform={`translate(${mx - 18}, ${my - 10})`}>
                          <rect className="wf-edge-badge-bg" width={36} height={18} rx={9} />
                          <text
                            className="wf-edge-badge-txt"
                            x={18}
                            y={12}
                            textAnchor="middle"
                          >
                            {e.branch === "true" ? tr(lang, "Sim", "True") : tr(lang, "Não", "False")}
                          </text>
                        </g>
                      ) : null}
                    </g>
                  );
                })}
                {connectingFrom && mouseWorld ? (
                  <path
                    className="wf-edge-draft"
                    d={cubicPath(connectingFrom.x, connectingFrom.y, mouseWorld.x, mouseWorld.y)}
                  />
                ) : null}
              </svg>

              {draft.nodes.map((n) => {
                const meta = NODE_META[n.kind];
                const Icon = meta.icon;
                const ev = liveNodes[n.id];
                const stateCls = ev ? ` state-${ev.status}` : "";
                const latestLog = runs[0]?.node_logs.find((l) => l.node_id === n.id);
                const quickOpen = quickMenuNode?.id === n.id;
                const isSel = selectedNodeId === n.id;

                return (
                  <div
                    key={n.id}
                    className={"wf-node" + (isSel ? " selected" : "") + stateCls}
                    style={{ left: n.x, top: n.y }}
                    onClick={(e) => {
                      e.stopPropagation();
                      setSelectedNodeId(n.id);
                      setInspOpen(true);
                    }}
                    onPointerDown={(e) => {
                      if (
                        (e.target as HTMLElement).closest(
                          ".wf-port, .wf-quick-add, .wf-quick-menu, .wf-node-actions",
                        )
                      ) {
                        return;
                      }
                      e.currentTarget.setPointerCapture(e.pointerId);
                      dragUndoPushedRef.current = false;
                      beforeDragDraftRef.current = draft ? structuredClone(draft) : null;
                      dragNodeRef.current = {
                        id: n.id,
                        startX: e.clientX,
                        startY: e.clientY,
                        origX: n.x,
                        origY: n.y,
                      };
                    }}
                    onPointerCancel={resetPointerDrag}
                  >
                    {isSel ? (
                      <div className="wf-node-actions" onClick={(e) => e.stopPropagation()}>
                        <button
                          type="button"
                          className="wf-node-act-btn"
                          title={tr(lang, "Testar este bloco", "Test this block")}
                          onClick={() => void runSingleNodeTest(n)}
                        >
                          <Play size={11} />
                        </button>
                        <button
                          type="button"
                          className="wf-node-act-btn"
                          title={tr(lang, "Duplicar bloco", "Duplicate block")}
                          onClick={() => duplicateNode(n)}
                        >
                          <Copy size={11} />
                        </button>
                        <button
                          type="button"
                          className="wf-node-act-btn"
                          title={tr(lang, "Remover bloco", "Delete block")}
                          onClick={() => deleteNode(n.id)}
                        >
                          <Trash2 size={11} />
                        </button>
                      </div>
                    ) : null}

                    {!n.kind.startsWith("trigger_") ? (
                      <button
                        type="button"
                        className={"wf-port in" + (connectingFrom ? " connecting" : "")}
                        title={tr(lang, "Conectar entrada aqui", "Connect input here")}
                        onClick={(e) => {
                          e.stopPropagation();
                          connectEdge(n.id);
                        }}
                      />
                    ) : null}

                    <div className={`wf-node-head kind-${n.kind}`}>
                      <Icon size={13} strokeWidth={2.2} />
                      <span>{lang === "pt-BR" ? meta.pt : meta.en}</span>
                      {n.config?.pin_enabled ? (
                        <span className="wf-mock-badge">MOCK</span>
                      ) : null}
                      {latestLog ? (
                        <span className="wf-node-dur">{latestLog.duration_ms}ms</span>
                      ) : null}
                    </div>

                    <div className="wf-node-body">
                      <div className="wf-node-label">{n.label}</div>
                      <div className="wf-node-sub">{nodeSubPreview(n, channels)}</div>
                    </div>

                    {n.kind === "condition" ? (
                      <>
                        <button
                          type="button"
                          className={
                            "wf-port out-true" +
                            (connectingFrom?.id === n.id && connectingFrom.branch === "true"
                              ? " connecting"
                              : "")
                          }
                          title={tr(lang, "Saída Verdadeira (Sim)", "True branch")}
                          onClick={(e) => {
                            e.stopPropagation();
                            setConnectingFrom({
                              id: n.id,
                              branch: "true",
                              x: n.x + NODE_W,
                              y: n.y + 36,
                            });
                          }}
                        />
                        <button
                          type="button"
                          className={
                            "wf-port out-false" +
                            (connectingFrom?.id === n.id && connectingFrom.branch === "false"
                              ? " connecting"
                              : "")
                          }
                          title={tr(lang, "Saída Falsa (Não)", "False branch")}
                          onClick={(e) => {
                            e.stopPropagation();
                            setConnectingFrom({
                              id: n.id,
                              branch: "false",
                              x: n.x + NODE_W,
                              y: n.y + 62,
                            });
                          }}
                        />
                      </>
                    ) : (
                      <button
                        type="button"
                        className={
                          "wf-port out" + (connectingFrom?.id === n.id ? " connecting" : "")
                        }
                        title={tr(
                          lang,
                          "Clique para puxar fio até outro bloco",
                          "Click to wire to target block",
                        )}
                        onClick={(e) => {
                          e.stopPropagation();
                          setConnectingFrom({
                            id: n.id,
                            x: n.x + NODE_W,
                            y: n.y + 49,
                          });
                        }}
                      />
                    )}

                    <button
                      type="button"
                      className={"wf-quick-add" + (quickOpen ? " open" : "")}
                      title={tr(lang, "Encadear próximo bloco", "Append next block")}
                      onClick={(e) => {
                        e.stopPropagation();
                        setQuickMenuNode(
                          quickOpen
                            ? null
                            : {
                                id: n.id,
                                branch: n.kind === "condition" ? "true" : undefined,
                              },
                        );
                      }}
                    >
                      <Plus size={12} strokeWidth={2.5} />
                    </button>

                    {quickOpen ? (
                      <div className="wf-quick-menu" onClick={(e) => e.stopPropagation()}>
                        {NODE_KINDS.filter((k) => !k.startsWith("trigger_")).map((k) => {
                          const m = NODE_META[k];
                          const KIcon = m.icon;
                          return (
                            <button
                              key={k}
                              type="button"
                              className="wf-quick-item"
                              onClick={() => quickAddChild(n.id, k, quickMenuNode?.branch)}
                            >
                              <KIcon size={13} style={{ color: m.color }} />
                              <span>{lang === "pt-BR" ? m.pt : m.en}</span>
                            </button>
                          );
                        })}
                      </div>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ) : null}

          {/* Floating Glass HUD */}
          {draft ? (
            <div className="wf-float-controls" onClick={(e) => e.stopPropagation()}>
              <button
                type="button"
                className="wf-hud-btn"
                onClick={() => zoomAtCenter(zoom - 0.15)}
                aria-label="Zoom out"
              >
                <ZoomOut size={13} />
              </button>
              <span className="mono" style={{ minWidth: 40, textAlign: "center" }}>
                {Math.round(zoom * 100)}%
              </span>
              <button
                type="button"
                className="wf-hud-btn"
                onClick={() => zoomAtCenter(zoom + 0.15)}
                aria-label="Zoom in"
              >
                <ZoomIn size={13} />
              </button>
              <span className="wf-hud-sep" />
              <button
                type="button"
                className="wf-hud-btn"
                disabled={histLen.undo === 0}
                onClick={handleUndo}
                title={tr(lang, "Desfazer (Ctrl+Z)", "Undo (Ctrl+Z)")}
              >
                <Undo2 size={12} />
              </button>
              <button
                type="button"
                className="wf-hud-btn"
                disabled={histLen.redo === 0}
                onClick={handleRedo}
                title={tr(lang, "Refazer (Ctrl+Y)", "Redo (Ctrl+Y)")}
              >
                <Redo2 size={12} />
              </button>
              <span className="wf-hud-sep" />
              <button
                type="button"
                className="wf-hud-btn"
                onClick={() => fitToView(draft)}
                title={tr(lang, "Ajustar todos os blocos na tela", "Fit to view")}
              >
                <Maximize2 size={12} />
                <span>{tr(lang, "Ajustar", "Fit")}</span>
              </button>
              <button
                type="button"
                className="wf-hud-btn"
                onClick={() => {
                  const arranged = autoArrange(draft);
                  patchDraft(() => arranged);
                  requestAnimationFrame(() => fitToView(arranged));
                }}
                title={tr(lang, "Auto-organizar blocos", "Auto-arrange blocks")}
              >
                <Wand2 size={12} />
                <span>{tr(lang, "Auto-Organizar", "Auto-Arrange")}</span>
              </button>
              <span className="wf-hud-sep" />
              <button
                type="button"
                className="wf-hud-btn"
                onClick={() => setInspOpen((v) => !v)}
                title={tr(lang, "Mostrar / ocultar inspetor", "Toggle inspector")}
              >
                <PanelRight size={13} />
              </button>
            </div>
          ) : null}

          {/* Pure-SVG Minimap */}
          {draft && minimapData ? (
            <svg className="wf-minimap" viewBox="0 0 168 106" aria-hidden>
              {draft.edges.map((e) => {
                const from = draft.nodes.find((n) => n.id === e.from);
                const to = draft.nodes.find((n) => n.id === e.to);
                if (!from || !to) return null;
                return (
                  <line
                    key={e.id}
                    x1={6 + (from.x + NODE_W / 2 - minimapData.minX) * minimapData.scale}
                    y1={6 + (from.y + 45 - minimapData.minY) * minimapData.scale}
                    x2={6 + (to.x + NODE_W / 2 - minimapData.minX) * minimapData.scale}
                    y2={6 + (to.y + 45 - minimapData.minY) * minimapData.scale}
                    stroke="rgba(255,255,255,0.28)"
                    strokeWidth={1.2}
                  />
                );
              })}
              {draft.nodes.map((n) => (
                <rect
                  key={n.id}
                  x={6 + (n.x - minimapData.minX) * minimapData.scale}
                  y={6 + (n.y - minimapData.minY) * minimapData.scale}
                  width={Math.max(8, NODE_W * minimapData.scale)}
                  height={Math.max(5, 86 * minimapData.scale)}
                  rx={2}
                  fill={NODE_META[n.kind].color}
                  opacity={n.id === selectedNodeId ? 1 : 0.65}
                />
              ))}
            </svg>
          ) : null}
        </div>

        {banner ? <div className={"wf-banner-msg " + banner.tone}>{banner.text}</div> : null}
      </section>

      {/* Right Collapsible Inspector & Run History */}
      {inspOpen ? (
        <aside className="wf-inspector">
          <div className="wf-insp-tabs">
            <button
              type="button"
              className={"wf-insp-tab" + (inspTab === "insp" ? " on" : "")}
              onClick={() => setInspTab("insp")}
            >
              {tr(lang, "Inspetor do Bloco", "Block Inspector")}
            </button>
            <button
              type="button"
              className={"wf-insp-tab" + (inspTab === "runs" ? " on" : "")}
              onClick={() => setInspTab("runs")}
            >
              {tr(lang, "Execuções", "Runs")} ({runs.length})
            </button>
          </div>

          {inspTab === "insp" ? (
            <div className="wf-insp-body">
              {selectedNode && draft ? (
                <NodeInspector
                  node={selectedNode}
                  runOn={draft.run_on}
                  workflowId={draft.id}
                  webhookBase={webhookBase}
                  allNodes={draft.nodes}
                  channels={channels}
                  agents={agents}
                  models={props.models}
                  lang={lang}
                  onOpenChannels={() => setChannelsOpen(true)}
                  onChange={(next) =>
                    patchDraft(
                      (w) => ({
                        ...w,
                        nodes: w.nodes.map((x) => (x.id === next.id ? next : x)),
                      }),
                      `node:${next.id}`,
                    )
                  }
                  onDelete={() => deleteNode(selectedNode.id)}
                  testInput={nodeTestInput}
                  onTestInput={setNodeTestInput}
                  testOutput={nodeTestOutput}
                  testBusy={nodeTestBusy}
                  onTest={() => void runSingleNodeTest()}
                />
              ) : (
                <>
                  <p className="pn-hint">
                    {tr(
                      lang,
                      "Clique em qualquer bloco no canvas para configurá-lo.",
                      "Click any block on the canvas to edit its settings.",
                    )}
                  </p>
                  {draft ? (
                    <AlertSettings
                      workflow={draft}
                      channels={channels}
                      lang={lang}
                      onChange={(patch) => patchDraft((w) => ({ ...w, ...patch }), "alert")}
                    />
                  ) : null}
                </>
              )}
            </div>
          ) : (
            <div className="wf-insp-body">
              <button
                type="button"
                className="btn btn-sm"
                onClick={async () => {
                  if (!selectedId) return;
                  try {
                    const r = await fetchRuns(selectedId);
                    setRuns(r);
                  } catch (e) {
                    setBanner({ text: String(e), tone: "err" });
                  }
                }}
              >
                <RefreshCw size={13} />
                <span>{tr(lang, "Atualizar histórico", "Refresh history")}</span>
              </button>
              {runs.map((r) => (
                <div key={r.id} className="wf-run-card">
                  <div className="pn-inline">
                    <span
                      className={
                        "badge " + (r.status === "ok" ? "ok" : r.status === "error" ? "danger" : "")
                      }
                    >
                      {r.status}
                    </span>
                    <span className="badge">{r.trigger_source}</span>
                    <span className="pn-spacer" />
                    <span className="pn-hint">{new Date(r.started_at_ms).toLocaleTimeString()}</span>
                  </div>
                  {r.node_logs.map((log) => (
                    <div key={log.node_id} className="wf-run-log">
                      {`[${log.status}] ${log.label} (${log.duration_ms}ms)\n${log.output}`}
                    </div>
                  ))}
                </div>
              ))}
            </div>
          )}
        </aside>
      ) : null}
    </div>
  );

  return (
    <>
      {props.inline ? (
        studio
      ) : (
        <div className="wf-backdrop" onMouseDown={props.onClose}>
          {studio}
        </div>
      )}

      {conflict && draft ? (
        <div className="wf-modal-backdrop" onMouseDown={() => setConflict(null)}>
          <div className="wf-conflict" role="alertdialog" onMouseDown={(e) => e.stopPropagation()}>
            <strong>{tr(lang, "O servidor tem uma versão mais nova", "The server has a newer version")}</strong>
            <p className="pn-hint">
              {tr(
                lang,
                `Outro dispositivo salvou "${conflict.name}" em ${serverName(draft.run_on)} (${new Date(conflict.updated_at_ms).toLocaleString()}).`,
                `Another device saved "${conflict.name}" on ${serverName(draft.run_on)} (${new Date(conflict.updated_at_ms).toLocaleString()}).`,
              )}
            </p>
            <div className="pn-inline pn-end">
              <button type="button" className="btn btn-sm btn-ghost" onClick={() => setConflict(null)}>
                {tr(lang, "Cancelar", "Cancel")}
              </button>
              <button type="button" className="btn btn-sm" onClick={() => void reloadFromServer()}>
                <RefreshCw size={13} /> {tr(lang, "Recarregar do servidor", "Reload from server")}
              </button>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => void handleSave(conflict.updated_at_ms)}
              >
                <Upload size={13} /> {tr(lang, "Sobrescrever", "Overwrite")}
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {channelsOpen ? (
        <ChannelsModal
          channels={channels}
          lang={lang}
          onClose={() => setChannelsOpen(false)}
          onUpdated={() => void loadAll(true)}
        />
      ) : null}

      {templatesOpen ? (
        <div className="wf-modal-backdrop" onMouseDown={() => setTemplatesOpen(false)}>
          <div
            className="wf-modal"
            style={{ gridTemplateColumns: "1fr" }}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <div style={{ display: "flex", flexDirection: "column", minHeight: 0 }}>
              <div className="wf-topbar">
                <strong style={{ flex: 1 }}>
                  {tr(lang, "Templates Prontos de Automação", "Ready-to-Use Automation Templates")}
                </strong>
                <button
                  type="button"
                  className="icon-btn sm"
                  onClick={() => setTemplatesOpen(false)}
                >
                  <X size={16} />
                </button>
              </div>
              <div className="wf-tpl-grid">
                {templates.map((t) => (
                  <div key={t.id} className="wf-tpl-card">
                    <div className="pn-inline">
                      <BotEmblem icon={t.icon} size={30} />
                      <strong>{t.name}</strong>
                    </div>
                    <p className="pn-hint">{t.description}</p>
                    <button
                      type="button"
                      className="btn btn-sm btn-primary"
                      onClick={() => void instantiateTemplate(t)}
                    >
                      <Plus size={13} />
                      <span>{tr(lang, "Usar template", "Use template")}</span>
                    </button>
                  </div>
                ))}
              </div>
              {templateErr ? <div className="wf-banner-msg err">{templateErr}</div> : null}
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}

function NodeInspector(props: {
  node: WorkflowNode;
  workflowId: string;
  webhookBase: string;
  allNodes: WorkflowNode[];
  channels: ChannelConfig[];
  agents: AgentView[];
  models: { value: string; label: string }[];
  lang: Lang;
  onChange: (n: WorkflowNode) => void;
  onDelete: () => void;
  onOpenChannels: () => void;
  testInput: string;
  onTestInput: (v: string) => void;
  testOutput: string | null;
  testBusy: boolean;
  onTest: () => void;
  /** Servidor em "Executar em": segredos viram nomes + "definir no servidor". */
  runOn?: string | null;
}) {
  const { node, lang } = props;
  const cfg = node.config ?? {};
  const setCfg = (patch: Record<string, unknown>) =>
    props.onChange({ ...node, config: { ...cfg, ...patch } });
  const webhookSlug = String(cfg.slug || props.workflowId);
  const webhookUrl = `${props.webhookBase}${webhookSlug}`;
  const supportsRetry = [
    "shell",
    "ssh_command",
    "http_request",
    "ai_agent",
    "notify_channel",
    "loop",
  ].includes(node.kind);

  const insertVar = (field: string, token: string) => {
    const cur = String(cfg[field] ?? "");
    setCfg({
      [field]: cur
        ? `${cur}${cur.endsWith(" ") || cur.endsWith("\n") ? "" : " "}${token}`
        : token,
    });
  };

  const renderVarChips = (field: string) => (
    <div className="wf-var-chips">
      {["{{prev}}", "{{date}}", "{{time}}"].map((t) => (
        <button
          key={t}
          type="button"
          className="wf-var-chip"
          onClick={() => insertVar(field, t)}
        >
          +{t}
        </button>
      ))}
      {props.allNodes
        .filter((n) => n.id !== node.id)
        .slice(0, 4)
        .map((n) => (
          <button
            key={n.id}
            type="button"
            className="wf-var-chip"
            title={n.label}
            onClick={() => insertVar(field, `{{node.${n.id}}}`)}
          >
            +{n.id}
          </button>
        ))}
    </div>
  );

  return (
    <>
      <div className="wf-field">
        <label>{tr(lang, "Nome do bloco", "Block label")}</label>
        <input
          value={node.label}
          onChange={(e) => props.onChange({ ...node, label: e.target.value })}
        />
      </div>

      {node.kind === "trigger_cron" ? (
        <CronEditor
          cron={String(cfg.cron ?? "0 8 * * *")}
          tz={typeof cfg.tz_offset_minutes === "number" ? cfg.tz_offset_minutes : undefined}
          lang={lang}
          onChange={(cron, tz) => setCfg({ cron, tz_offset_minutes: tz })}
        />
      ) : null}

      {node.kind === "trigger_webhook" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Slug da rota HTTP", "HTTP Route Slug")}</label>
            <input
              value={String(cfg.slug ?? "")}
              placeholder={props.workflowId}
              onChange={(e) => setCfg({ slug: e.target.value.replace(/[^a-zA-Z0-9_-]/g, "-") })}
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Token secreto (gerado ao salvar se vazio; ?token= / Bearer)", "Secret token (generated on save if empty; ?token= / Bearer)")}</label>
            <input
              type="password"
              value={String(cfg.secret ?? "")}
              placeholder="meu-segredo-123"
              onChange={(e) => setCfg({ secret: e.target.value })}
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "URL Local do Webhook", "Local Webhook URL")}</label>
            <div className="wf-webhook-box">
              <code className="mono">{webhookUrl}</code>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void navigator.clipboard?.writeText(webhookUrl)}
              >
                <Copy size={11} />
                <span>{tr(lang, "Copiar", "Copy")}</span>
              </button>
            </div>
            <p className="pn-hint" style={{ marginTop: 4 }}>
              {`curl -X POST "${webhookUrl}" -d '{"alert":"host-down"}'`}
            </p>
          </div>
        </>
      ) : null}

      {node.kind === "delay" ? (
        <div className="wf-field">
          <label>{tr(lang, "Aguardar (segundos, 1–300)", "Delay (seconds, 1–300)")}</label>
          <input
            type="number"
            min={1}
            max={300}
            value={Number(cfg.seconds ?? 5)}
            onChange={(e) =>
              setCfg({ seconds: Math.max(1, Math.min(300, Number(e.target.value) || 5)) })
            }
          />
        </div>
      ) : null}

      {node.kind === "loop" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Ação por item", "Per-item action")}</label>
            <select
              value={String(cfg.mode ?? "template")}
              onChange={(e) => setCfg({ mode: e.target.value })}
            >
              <option value="template">{tr(lang, "Formatar template ({{item}})", "Template ({{item}})")}</option>
              <option value="shell">{tr(lang, "Executar comando Shell", "Run Shell command")}</option>
              <option value="http">{tr(lang, "Chamada HTTP", "HTTP Request")}</option>
            </select>
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Lista de itens (1/linha ou JSON; vazio = {{prev}})", "Items (lines or JSON array; empty = {{prev}})")}</label>
            <textarea
              value={String(cfg.items ?? "")}
              placeholder={"192.168.1.1\n192.168.1.2"}
              onChange={(e) => setCfg({ items: e.target.value })}
            />
          </div>
          {String(cfg.mode ?? "template") === "shell" ? (
            <div className="wf-field">
              <label>{tr(lang, "Comando Shell (use {{item}})", "Shell command (use {{item}})")}</label>
              <textarea
                value={String(cfg.command ?? "echo {{item}}")}
                onChange={(e) => setCfg({ command: e.target.value })}
              />
              {renderVarChips("command")}
            </div>
          ) : String(cfg.mode ?? "template") === "http" ? (
            <div className="wf-field">
              <label>{tr(lang, "URL HTTP (use {{item}})", "HTTP URL (use {{item}})")}</label>
              <input
                value={String(cfg.url ?? "")}
                placeholder="https://api.exemplo.com/hosts/{{item}}"
                onChange={(e) => setCfg({ url: e.target.value })}
              />
            </div>
          ) : (
            <div className="wf-field">
              <label>{tr(lang, "Template (use {{item}} e {{index}})", "Template (use {{item}} & {{index}})")}</label>
              <textarea
                value={String(cfg.template ?? "{{item}}")}
                onChange={(e) => setCfg({ template: e.target.value })}
              />
              {renderVarChips("template")}
            </div>
          )}
          <div className="wf-field">
            <label>{tr(lang, "Separador na saída", "Output separator")}</label>
            <input
              value={String(cfg.separator ?? "\n")}
              onChange={(e) => setCfg({ separator: e.target.value })}
            />
          </div>
        </>
      ) : null}

      {node.kind === "shell" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Comando Shell / Script", "Shell / Script command")}</label>
            <textarea
              value={String(cfg.command ?? "")}
              onChange={(e) => setCfg({ command: e.target.value })}
            />
            {renderVarChips("command")}
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Timeout (segundos)", "Timeout (seconds)")}</label>
            <input
              type="number"
              value={Number(cfg.timeout_secs ?? 30)}
              onChange={(e) => setCfg({ timeout_secs: Number(e.target.value) || 30 })}
            />
          </div>
        </>
      ) : null}

      {node.kind === "ssh_command" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Equipamento do inventário (cliente/nome)", "Inventory device (client/name)")}</label>
            <input
              value={String(cfg.device ?? "")}
              placeholder={tr(lang, "opcional: preenche host, porta, usuário e senha", "optional: fills host, port, user and password")}
              onChange={(e) => setCfg({ device: e.target.value })}
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Segredo com a senha (sem chave)", "Password secret (no key)")}</label>
            <input
              value={String(cfg.password_secret ?? "")}
              placeholder="ex. dude-senha"
              onChange={(e) => setCfg({ password_secret: e.target.value })}
            />
          </div>
          <label className="pn-inline">
            <input
              type="checkbox"
              checked={Boolean(cfg.legacy_ssh)}
              onChange={(e) => setCfg({ legacy_ssh: e.target.checked })}
            />
            <span>{tr(lang, "Algoritmos antigos (ssh-rsa, DH group14-sha1)", "Legacy algorithms (ssh-rsa, DH group14-sha1)")}</span>
          </label>
          <div className="wf-field">
            <label>Host / IP</label>
            <input
              value={String(cfg.host ?? "")}
              placeholder="192.168.1.1"
              onChange={(e) => setCfg({ host: e.target.value })}
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Usuário & Porta", "User & Port")}</label>
            <div className="pn-inline">
              <input
                value={String(cfg.user ?? "root")}
                onChange={(e) => setCfg({ user: e.target.value })}
              />
              <input
                type="number"
                style={{ width: 82 }}
                value={Number(cfg.port ?? 22)}
                onChange={(e) => setCfg({ port: Number(e.target.value) || 22 })}
              />
            </div>
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Comando remoto", "Remote command")}</label>
            <textarea
              value={String(cfg.command ?? "")}
              onChange={(e) => setCfg({ command: e.target.value })}
            />
            {renderVarChips("command")}
          </div>
        </>
      ) : null}

      {node.kind === "http_request" ? (
        <>
          <div className="wf-field">
            <label>Method & URL</label>
            <div className="pn-inline">
              <select
                style={{ width: 92 }}
                value={String(cfg.method ?? "GET")}
                onChange={(e) => setCfg({ method: e.target.value })}
              >
                {["GET", "POST", "PUT", "PATCH", "DELETE"].map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </select>
              <input
                value={String(cfg.url ?? "")}
                onChange={(e) => setCfg({ url: e.target.value })}
              />
            </div>
          </div>
          <div className="wf-field">
            <label>Body (JSON / text)</label>
            <textarea
              value={String(cfg.body ?? "")}
              onChange={(e) => setCfg({ body: e.target.value })}
            />
            {renderVarChips("body")}
          </div>
        </>
      ) : null}

      {node.kind === "transform" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Modo", "Mode")}</label>
            <select
              value={String(cfg.mode ?? "template")}
              onChange={(e) => setCfg({ mode: e.target.value })}
            >
              <option value="template">{tr(lang, "Formatar template", "Template")}</option>
              <option value="json_path">{tr(lang, "Extrair JSON (a.b.0)", "JSON path (a.b.0)")}</option>
              <option value="filter_lines">{tr(lang, "Filtrar linhas", "Filter lines")}</option>
              <option value="dedup_lines">{tr(lang, "Filtrar apenas inéditos (Anti-repetição)", "Only unseen lines (Deduplicate)")}</option>
            </select>
          </div>
          {String(cfg.mode ?? "template") === "template" ? (
            <div className="wf-field">
              <label>Template</label>
              <textarea
                value={String(cfg.template ?? "{{prev}}")}
                onChange={(e) => setCfg({ template: e.target.value })}
              />
              {renderVarChips("template")}
            </div>
          ) : (
            <div className="wf-field">
              <label>
                {String(cfg.mode ?? "") === "dedup_lines"
                  ? tr(lang, "Chave de memória (opcional, ex: videos-yt)", "Seen key (optional, e.g. videos-yt)")
                  : tr(lang, "Caminho / Padrão", "Path / Pattern")}
              </label>
              <input
                value={String(cfg.pattern ?? "")}
                placeholder={String(cfg.mode ?? "") === "dedup_lines" ? node.id : ""}
                onChange={(e) => setCfg({ pattern: e.target.value })}
              />
            </div>
          )}
        </>
      ) : null}

      {node.kind === "condition" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Regra", "Rule")}</label>
            <select
              value={String(cfg.mode ?? "not_empty")}
              onChange={(e) => setCfg({ mode: e.target.value })}
            >
              <option value="not_empty">{tr(lang, "Não está vazio", "Is not empty")}</option>
              <option value="contains">{tr(lang, "Contém texto", "Contains text")}</option>
              <option value="not_contains">{tr(lang, "Não contém texto", "Does not contain")}</option>
              <option value="equals">{tr(lang, "Igual a", "Equals")}</option>
            </select>
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Texto / Padrão", "Pattern")}</label>
            <input
              value={String(cfg.pattern ?? "")}
              onChange={(e) => setCfg({ pattern: e.target.value })}
            />
          </div>
        </>
      ) : null}

      {node.kind === "ai_agent" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Bot especialista (opcional)", "Specialist Bot (optional)")}</label>
            <select
              value={String(cfg.bot_name ?? "")}
              onChange={(e) => setCfg({ bot_name: e.target.value })}
            >
              <option value="">{tr(lang, "Nenhum (usar prompt abaixo)", "None (use prompt below)")}</option>
              {props.agents.map((a) => (
                <option key={a.name} value={a.name}>
                  @{a.name}
                </option>
              ))}
            </select>
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Modelo", "Model")}</label>
            <select
              value={String(cfg.model ?? "")}
              onChange={(e) => setCfg({ model: e.target.value })}
            >
              <option value="">{tr(lang, "Padrão configurado no Zerith", "Zerith default model")}</option>
              {props.models.map((m) => (
                <option key={m.value} value={m.value}>
                  {m.label}
                </option>
              ))}
            </select>
          </div>
          <div className="wf-field">
            <label>System Prompt</label>
            <textarea
              value={String(cfg.system ?? "")}
              onChange={(e) => setCfg({ system: e.target.value })}
            />
          </div>
          <div className="wf-field">
            <label>Prompt</label>
            <textarea
              value={String(cfg.prompt ?? "{{prev}}")}
              onChange={(e) => setCfg({ prompt: e.target.value })}
            />
            {renderVarChips("prompt")}
          </div>
          <div className="wf-pin-box" style={{ borderStyle: "solid" }}>
            <label className="pn-inline">
              <input
                type="checkbox"
                checked={Boolean(cfg.use_tools)}
                onChange={(e) => setCfg({ use_tools: e.target.checked })}
              />
              <span>
                {tr(
                  lang,
                  "Usar Skills, Plugins, MCP & Memória em Loop",
                  "Enable Skills, Plugins, MCP & Memory Agent Loop",
                )}
              </span>
            </label>
            {cfg.use_tools ? (
              <div className="wf-retry-row">
                <div className="wf-field">
                  <label>{tr(lang, "Máx. voltas (1–20)", "Max loop steps (1–20)")}</label>
                  <input
                    type="number"
                    min={1}
                    max={20}
                    value={Number(cfg.max_turns ?? 6)}
                    onChange={(e) =>
                      setCfg({ max_turns: Math.max(1, Math.min(20, Number(e.target.value) || 6)) })
                    }
                  />
                </div>
                <div className="wf-field">
                  <label>{tr(lang, "Modo de ferramentas", "Tool access mode")}</label>
                  <select
                    value={String(cfg.tool_mode ?? "safe")}
                    onChange={(e) => setCfg({ tool_mode: e.target.value })}
                  >
                    <option value="safe">{tr(lang, "Seguro (Leitura + Web/MCP)", "Safe (Read + Web/MCP)")}</option>
                    <option value="bypass">{tr(lang, "Autônomo total (Bypass)", "Autonomous (Bypass)")}</option>
                  </select>
                </div>
              </div>
            ) : null}
          </div>
        </>
      ) : null}

      {node.kind === "notify_channel" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Canal global", "Global channel")}</label>
            <div className="pn-inline">
              <select
                value={String(cfg.channel_id ?? "")}
                onChange={(e) => setCfg({ channel_id: e.target.value })}
              >
                <option value="">{tr(lang, "-- Escolha um canal --", "-- Select channel --")}</option>
                {props.channels.map((ch) => (
                  <option key={ch.id} value={ch.id}>
                    {ch.name} ({ch.kind})
                  </option>
                ))}
              </select>
              <button type="button" className="btn btn-sm" onClick={props.onOpenChannels}>
                +
              </button>
            </div>
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Destino opcional (número / chat_id)", "Optional target override")}</label>
            <input
              value={String(cfg.target ?? "")}
              placeholder="5511999999999"
              onChange={(e) => setCfg({ target: e.target.value })}
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Mensagem", "Message")}</label>
            <textarea
              value={String(cfg.message ?? "{{prev}}")}
              onChange={(e) => setCfg({ message: e.target.value })}
            />
            {renderVarChips("message")}
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Anexar arquivo (opcional)", "Attach file (optional)")}</label>
            <select
              value={String(cfg.attach_node ?? "")}
              onChange={(e) => setCfg({ attach_node: e.target.value || undefined })}
            >
              <option value="">{tr(lang, "-- Nenhum bloco Salvar arquivo --", "-- No Save file block --")}</option>
              {props.allNodes
                .filter((n) => n.kind === "save_file")
                .map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.label}
                  </option>
                ))}
            </select>
            {!cfg.attach_node ? (
              <input
                value={String(cfg.attachment ?? "")}
                placeholder={tr(lang, "ou caminho / URL: reports/rel-{{date}}.pdf", "or path / URL: reports/rel-{{date}}.pdf")}
                onChange={(e) => setCfg({ attachment: e.target.value })}
              />
            ) : null}
            <p className="pn-hint">
              {tr(
                lang,
                "Vai como arquivo na Evolution, WAHA e Telegram; nos outros canais só o texto.",
                "Sent as a file on Evolution, WAHA and Telegram; other channels get the text only.",
              )}
            </p>
          </div>
        </>
      ) : null}

      {node.kind === "save_file" ? (
        <>
          <div className="wf-field">
            <label>{tr(lang, "Caminho do arquivo", "File path")}</label>
            <input
              value={String(cfg.path ?? "reports/out-{{date}}.md")}
              onChange={(e) => setCfg({ path: e.target.value })}
            />
          </div>
          <label className="pn-inline">
            <input
              type="checkbox"
              checked={Boolean(cfg.append)}
              onChange={(e) => setCfg({ append: e.target.checked })}
            />
            <span>{tr(lang, "Acrescentar ao final (append)", "Append to file")}</span>
          </label>
        </>
      ) : null}

      {props.runOn && (secretRefs(cfg).length || node.kind === "ssh_command" || node.kind === "notify_channel") ? (
        <ServerSecrets serverId={props.runOn} refs={secretRefs(cfg)} />
      ) : null}

      {supportsRetry ? (
        <div className="wf-retry-row">
          <div className="wf-field">
            <label>{tr(lang, "Tentativas (0–5)", "Retries (0–5)")}</label>
            <input
              type="number"
              min={0}
              max={5}
              value={Number(cfg.retry_count ?? 0)}
              onChange={(e) =>
                setCfg({ retry_count: Math.max(0, Math.min(5, Number(e.target.value) || 0)) })
              }
            />
          </div>
          <div className="wf-field">
            <label>{tr(lang, "Espera (s)", "Delay (s)")}</label>
            <input
              type="number"
              min={1}
              max={60}
              value={Number(cfg.retry_delay_secs ?? 2)}
              onChange={(e) =>
                setCfg({ retry_delay_secs: Math.max(1, Math.min(60, Number(e.target.value) || 2)) })
              }
            />
          </div>
        </div>
      ) : null}

      <div className="wf-pin-box">
        <label className="pn-inline">
          <input
            type="checkbox"
            checked={Boolean(cfg.pin_enabled)}
            onChange={(e) => setCfg({ pin_enabled: e.target.checked })}
          />
          <span>{tr(lang, "Fixar saída mock (pular execução real)", "Pin mock output (skip real run)")}</span>
        </label>
        {Boolean(cfg.pin_enabled) || String(cfg.pinned_output ?? "") ? (
          <textarea
            rows={2}
            value={String(cfg.pinned_output ?? "")}
            placeholder={tr(lang, "Saída simulada para testar os próximos blocos...", "Mocked output for downstream testing...")}
            onChange={(e) => setCfg({ pinned_output: e.target.value })}
          />
        ) : null}
        {props.testOutput != null ? (
          <button
            type="button"
            className="wf-var-chip"
            onClick={() => setCfg({ pin_enabled: true, pinned_output: props.testOutput })}
          >
            {tr(lang, "Fixar último resultado de teste", "Pin latest test output")}
          </button>
        ) : null}
      </div>

      <div style={{ borderTop: "1px solid var(--line-faint)", paddingTop: 10, display: "grid", gap: 8 }}>
        <div className="wf-field">
          <label>{tr(lang, "Testar este bloco (entrada opcional)", "Test this block (optional input)")}</label>
          <input
            value={props.testInput}
            placeholder="{{prev}} sample..."
            onChange={(e) => props.onTestInput(e.target.value)}
          />
        </div>
        <div className="pn-inline">
          <button
            type="button"
            className="btn btn-sm"
            onClick={props.onTest}
            disabled={props.testBusy}
          >
            <Play size={12} />
            <span>{props.testBusy ? "..." : tr(lang, "Testar bloco", "Test block")}</span>
          </button>
          <span className="pn-spacer" />
          <button type="button" className="btn btn-sm" onClick={props.onDelete}>
            <Trash2 size={12} />
            <span>{tr(lang, "Remover bloco", "Delete block")}</span>
          </button>
        </div>
        {props.testOutput != null ? <div className="wf-run-log">{props.testOutput}</div> : null}
      </div>
    </>
  );
}

function CronEditor(props: {
  cron: string;
  tz: number | undefined;
  lang: Lang;
  onChange: (cron: string, tz: number | undefined) => void;
}) {
  const { lang } = props;
  const applyPreset = (expr: string) => props.onChange(expr, props.tz);

  return (
    <>
      <div className="wf-field">
        <label>{tr(lang, "Atalhos de agendamento", "Quick schedule presets")}</label>
        <div className="wf-presets">
          <button type="button" className="wf-var-chip" onClick={() => applyPreset("0 8 * * *")}>
            {tr(lang, "Todo dia 08:00", "Daily 08:00")}
          </button>
          <button type="button" className="wf-var-chip" onClick={() => applyPreset("*/15 * * * *")}>
            {tr(lang, "A cada 15 min", "Every 15m")}
          </button>
          <button type="button" className="wf-var-chip" onClick={() => applyPreset("0 * * * *")}>
            {tr(lang, "Toda hora", "Hourly")}
          </button>
          <button type="button" className="wf-var-chip" onClick={() => applyPreset("30 8 * * 1-5")}>
            {tr(lang, "Seg–Sex 08:30", "Weekdays 08:30")}
          </button>
        </div>
      </div>
      <div className="wf-field">
        <label>Cron (min hour dom month dow)</label>
        <input
          className="mono"
          value={props.cron}
          onChange={(e) => props.onChange(e.target.value, props.tz)}
        />
      </div>
      <div className="wf-field">
        <label>{tr(lang, "Fuso UTC (minutos)", "UTC Offset (minutes)")}</label>
        <input
          type="number"
          value={props.tz ?? ""}
          placeholder={tr(lang, "vazio = fuso do sistema", "empty = system timezone")}
          onChange={(e) =>
            props.onChange(props.cron, e.target.value === "" ? undefined : Number(e.target.value))
          }
        />
      </div>
    </>
  );
}

/** Aviso de falha das execuções agendadas: canais, destino por canal e N. */
function AlertSettings(props: {
  workflow: Workflow;
  channels: ChannelConfig[];
  lang: Lang;
  onChange: (patch: Partial<Workflow>) => void;
}) {
  const { workflow: wf, lang } = props;
  const chosen = wf.alert_channels ?? [];
  const targets = wf.alert_target ?? {};
  const toggle = (id: string, on: boolean) => {
    const next = on ? [...chosen, id] : chosen.filter((c) => c !== id);
    const nextTargets = { ...targets };
    if (!on) delete nextTargets[id];
    props.onChange({ alert_channels: next, alert_target: nextTargets });
  };
  return (
    <div className="wf-field">
      <label>{tr(lang, "Alerta de falha", "Failure alert")}</label>
      <p className="pn-hint">
        {tr(
          lang,
          "Avisa uma vez quando falha e outra quando volta a funcionar. Sem canal marcado usa o canal padrão do servidor.",
          "Alerts once on failure and once on recovery. With no channel checked, the server's default channel is used.",
        )}
      </p>
      {props.channels.length ? null : (
        <p className="pn-hint">{tr(lang, "Nenhum canal cadastrado.", "No channels yet.")}</p>
      )}
      {props.channels.map((ch) => {
        const on = chosen.includes(ch.id);
        return (
          <div key={ch.id} style={{ display: "grid", gap: 4 }}>
            <label className="pn-inline" style={{ fontWeight: 400 }}>
              <input type="checkbox" checked={on} onChange={(e) => toggle(ch.id, e.target.checked)} />
              <span>
                {ch.name} ({ch.kind})
              </span>
            </label>
            {on ? (
              <input
                aria-label={tr(lang, `Destino em ${ch.name}`, `Target on ${ch.name}`)}
                value={targets[ch.id] ?? ""}
                placeholder={String(ch.config.default_target ?? "") || tr(lang, "destino padrão do canal", "channel default target")}
                onChange={(e) => {
                  const next = { ...targets };
                  if (e.target.value.trim()) next[ch.id] = e.target.value;
                  else delete next[ch.id];
                  props.onChange({ alert_target: next });
                }}
              />
            ) : null}
          </div>
        );
      })}
      <label className="pn-inline" style={{ fontWeight: 400 }}>
        <span>{tr(lang, "Avisar após", "Alert after")}</span>
        <input
          type="number"
          min={1}
          style={{ width: 64 }}
          value={wf.alert_after_failures ?? 1}
          onChange={(e) => {
            const n = Math.max(1, Math.floor(Number(e.target.value) || 1));
            props.onChange({ alert_after_failures: n === 1 ? null : n });
          }}
        />
        <span>{tr(lang, "falha(s) seguida(s)", "failure(s) in a row")}</span>
      </label>
    </div>
  );
}

function ChannelsModal(props: {
  channels: ChannelConfig[];
  lang: Lang;
  onClose: () => void;
  onUpdated: () => void;
}) {
  const { lang } = props;
  const [selectedId, setSelectedId] = useState<string | null>(props.channels[0]?.id ?? null);
  const [draft, setDraft] = useState<ChannelConfig>(() =>
    props.channels[0]
      ? structuredClone(props.channels[0])
      : newBlankChannel(),
  );
  const [msg, setMsg] = useState<{ text: string; tone: "ok" | "err" | "" } | null>(null);
  const [busy, setBusy] = useState(false);
  const [groups, setGroups] = useState<ChannelGroup[] | null>(null);

  const selectChannel = (ch: ChannelConfig) => {
    setSelectedId(ch.id);
    setDraft(structuredClone(ch));
    setMsg(null);
    setGroups(null);
  };

  const createNew = () => {
    const fresh = newBlankChannel();
    setSelectedId(fresh.id);
    setDraft(fresh);
    setMsg(null);
  };

  const setCfg = (patch: Record<string, unknown>) =>
    setDraft((prev) => ({ ...prev, config: { ...prev.config, ...patch } }));

  const save = async () => {
    setBusy(true);
    setMsg(null);
    try {
      const saved = await channelsApi.save({ ...draft, updated_at_ms: Date.now() });
      setDraft(structuredClone(saved));
      setSelectedId(saved.id);
      props.onUpdated();
      setMsg({ text: tr(lang, "Canal salvo com sucesso!", "Channel saved!"), tone: "ok" });
    } catch (e) {
      setMsg({ text: String(e), tone: "err" });
    } finally {
      setBusy(false);
    }
  };

  /** Salva o rascunho (o núcleo lê o canal do banco) e roda `action`. */
  const withSaved = async (action: () => Promise<string | void>) => {
    setBusy(true);
    setMsg(null);
    try {
      await channelsApi.save({ ...draft, updated_at_ms: Date.now() });
      const res = await action();
      props.onUpdated();
      if (res) setMsg({ text: res, tone: "ok" });
    } catch (e) {
      setMsg({ text: String(e), tone: "err" });
    } finally {
      setBusy(false);
    }
  };
  const test = () => withSaved(() => channelsApi.test(draft.id));
  const checkConnection = () => withSaved(() => channelsApi.check(draft.id));
  const pickGroup = () =>
    withSaved(async () => {
      const list = await channelsApi.groups(draft.id);
      setGroups(list);
      if (!list.length) return tr(lang, "Nenhum grupo nesta instância.", "No groups in this instance.");
    });

  const cfg = draft.config ?? {};

  return (
    <div className="wf-modal-backdrop" onMouseDown={props.onClose}>
      <div className="wf-modal" onMouseDown={(e) => e.stopPropagation()}>
        <aside className="wf-rail">
          <div className="wf-rail-head">
            <span className="wf-rail-title">{tr(lang, "Canais Globais", "Global Channels")}</span>
            <button type="button" className="btn btn-sm" onClick={createNew}>
              <Plus size={14} />
            </button>
          </div>
          <div className="wf-rail-list">
            {props.channels.map((c) => (
              <button
                key={c.id}
                type="button"
                className={"wf-roster-card" + (selectedId === c.id ? " on" : "")}
                onClick={() => selectChannel(c)}
              >
                <BotEmblem icon="bell" size={30} />
                <div className="wf-roster-meta">
                  <span className="wf-roster-name">{c.name}</span>
                  <span className="wf-roster-sub">{c.kind}</span>
                </div>
              </button>
            ))}
          </div>
        </aside>

        <div style={{ display: "flex", flexDirection: "column", minHeight: 0 }}>
          <div className="wf-topbar">
            <strong style={{ flex: 1 }}>
              {tr(lang, "Configurar Canal de Entrega", "Configure Delivery Channel")}
            </strong>
            <button type="button" className="icon-btn sm" onClick={props.onClose}>
              <X size={16} />
            </button>
          </div>

          <div className="wf-insp-body" style={{ flex: 1 }}>
            <div className="wf-field">
              <label>{tr(lang, "Nome do canal", "Channel name")}</label>
              <input
                value={draft.name}
                onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
              />
            </div>

            <div className="wf-field">
              <label>{tr(lang, "Provedor", "Provider")}</label>
              <select
                value={draft.kind}
                onChange={(e) => setDraft((d) => ({ ...d, kind: e.target.value as ChannelKind }))}
              >
                {CHANNEL_KINDS.map((k) => (
                  <option key={k.kind} value={k.kind}>
                    {k.label}
                  </option>
                ))}
              </select>
            </div>

            {draft.kind === "evolution_api" || draft.kind === "waha" ? (
              <>
                <div className="wf-field">
                  <label>Base URL</label>
                  <input
                    value={String(cfg.base_url ?? "")}
                    placeholder="https://api.seuservidor.com"
                    onChange={(e) => setCfg({ base_url: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>{draft.kind === "waha" ? "Session" : "Instance"}</label>
                  <input
                    value={String(draft.kind === "waha" ? cfg.session ?? "default" : cfg.instance ?? "")}
                    onChange={(e) =>
                      setCfg(draft.kind === "waha" ? { session: e.target.value } : { instance: e.target.value })
                    }
                  />
                </div>
                <div className="wf-field">
                  <label>API Key</label>
                  <input
                    type="password"
                    value={String(cfg.api_key ?? "")}
                    onChange={(e) => setCfg({ api_key: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>{tr(lang, "Número ou grupo padrão", "Default phone or group")}</label>
                  <div className="pn-inline">
                    <input
                      value={String(cfg.default_target ?? "")}
                      placeholder="11999999999 / 120363...@g.us"
                      onChange={(e) => setCfg({ default_target: e.target.value })}
                    />
                    {draft.kind === "evolution_api" ? (
                      <button type="button" className="btn btn-sm" disabled={busy} onClick={() => void pickGroup()}>
                        {tr(lang, "Escolher grupo", "Pick group")}
                      </button>
                    ) : null}
                  </div>
                  <p className="pn-hint">
                    {tr(
                      lang,
                      "Número com DDD (o 55 entra sozinho) ou o JID do grupo, terminado em @g.us.",
                      "Phone with area code (Brazil's 55 is added) or the group JID ending in @g.us.",
                    )}
                  </p>
                  {groups && groups.length ? (
                    <select
                      aria-label={tr(lang, "Grupos da instância", "Instance groups")}
                      value=""
                      onChange={(e) => {
                        if (e.target.value) setCfg({ default_target: e.target.value });
                        setGroups(null);
                      }}
                    >
                      <option value="">{tr(lang, "-- Escolha o grupo --", "-- Select group --")}</option>
                      {groups.map((g) => (
                        <option key={g.id} value={g.id}>
                          {g.name}
                          {g.size ? ` (${g.size})` : ""}
                        </option>
                      ))}
                    </select>
                  ) : null}
                </div>
                <p className="pn-hint">
                  {tr(
                    lang,
                    "Protocolo não oficial do WhatsApp: use um número dedicado, que pode ser banido. Guia em docs/whatsapp.md.",
                    "Unofficial WhatsApp protocol: use a dedicated number, it may be banned. Guide in docs/whatsapp.md.",
                  )}
                </p>
              </>
            ) : null}

            {draft.kind === "whatsapp_cloud" ? (
              <>
                <div className="wf-field">
                  <label>Phone Number ID</label>
                  <input
                    value={String(cfg.phone_number_id ?? "")}
                    onChange={(e) => setCfg({ phone_number_id: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>Access Token</label>
                  <input
                    type="password"
                    value={String(cfg.access_token ?? "")}
                    onChange={(e) => setCfg({ access_token: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>{tr(lang, "Número destino padrão", "Default recipient number")}</label>
                  <input
                    value={String(cfg.default_target ?? "")}
                    placeholder="5511999999999"
                    onChange={(e) => setCfg({ default_target: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>{tr(lang, "Versão do Graph API", "Graph API version")}</label>
                  <input
                    value={String(cfg.graph_version ?? "")}
                    placeholder="v26.0"
                    onChange={(e) => setCfg({ graph_version: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>{tr(lang, "Template aprovado (opcional)", "Approved template (optional)")}</label>
                  <div className="pn-inline">
                    <input
                      value={String(cfg.template_name ?? "")}
                      placeholder="alerta_zerith"
                      onChange={(e) => setCfg({ template_name: e.target.value })}
                    />
                    <input
                      style={{ maxWidth: 90 }}
                      aria-label={tr(lang, "Idioma do template", "Template language")}
                      value={String(cfg.template_language ?? "")}
                      placeholder="pt_BR"
                      onChange={(e) => setCfg({ template_language: e.target.value })}
                    />
                  </div>
                  <p className="pn-hint">
                    {tr(
                      lang,
                      "Fora da janela de 24 h a Meta só aceita template; o aviso vai na variável {{1}} do corpo. A Cloud API não envia para grupos.",
                      "Outside the 24 h window Meta only accepts templates; the alert fills body variable {{1}}. The Cloud API cannot send to groups.",
                    )}
                  </p>
                </div>
              </>
            ) : null}

            {draft.kind === "telegram" ? (
              <>
                <div className="wf-field">
                  <label>Bot Token</label>
                  <input
                    type="password"
                    value={String(cfg.bot_token ?? "")}
                    onChange={(e) => setCfg({ bot_token: e.target.value })}
                  />
                </div>
                <div className="wf-field">
                  <label>Default Chat ID</label>
                  <input
                    value={String(cfg.default_target ?? "")}
                    onChange={(e) => setCfg({ default_target: e.target.value })}
                  />
                </div>
              </>
            ) : null}

            {draft.kind === "discord_webhook" ||
            draft.kind === "slack_webhook" ||
            draft.kind === "generic_webhook" ? (
              <div className="wf-field">
                <label>Webhook URL</label>
                <input
                  value={String(cfg.webhook_url ?? "")}
                  placeholder="https://..."
                  onChange={(e) => setCfg({ webhook_url: e.target.value })}
                />
              </div>
            ) : null}

            {draft.kind === "generic_webhook" ? (
              <div className="wf-field">
                <label>Auth Header (optional, e.g. Bearer xxx)</label>
                <input
                  type="password"
                  value={String(cfg.auth_header ?? "")}
                  onChange={(e) => setCfg({ auth_header: e.target.value })}
                />
              </div>
            ) : null}

            <div className="pn-inline" style={{ marginTop: 6 }}>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => void save()}
                disabled={busy}
              >
                <Save size={13} />
                <span>{tr(lang, "Salvar", "Save")}</span>
              </button>
              {["evolution_api", "waha", "whatsapp_cloud", "telegram"].includes(draft.kind) ? (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => void checkConnection()}
                  disabled={busy}
                >
                  <RefreshCw size={13} />
                  <span>{tr(lang, "Testar conexão", "Test connection")}</span>
                </button>
              ) : null}
              <button type="button" className="btn btn-sm" onClick={() => void test()} disabled={busy}>
                <Send size={13} />
                <span>{tr(lang, "Enviar Teste", "Send Test")}</span>
              </button>
              {props.channels.some((c) => c.id === draft.id) ? (
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={busy}
                  onClick={async () => {
                    setBusy(true);
                    setMsg(null);
                    try {
                      await channelsApi.delete(draft.id);
                      props.onUpdated();
                      createNew();
                      setMsg({ text: tr(lang, "Canal removido!", "Channel deleted!"), tone: "ok" });
                    } catch (e) {
                      setMsg({ text: String(e), tone: "err" });
                    } finally {
                      setBusy(false);
                    }
                  }}
                >
                  <Trash2 size={13} />
                </button>
              ) : null}
            </div>
            {msg ? <div className={"wf-banner-msg " + msg.tone}>{msg.text}</div> : null}
          </div>
        </div>
      </div>
    </div>
  );
}

function newBlankChannel(): ChannelConfig {
  return {
    id: `ch-${Date.now()}`,
    name: "WhatsApp NOC",
    kind: "evolution_api",
    enabled: true,
    config: { base_url: "", instance: "", api_key: "", default_target: "" },
    updated_at_ms: Date.now(),
  };
}
