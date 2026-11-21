import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Bell,
  Bot,
  Clock,
  Copy,
  FileText,
  GitBranch,
  Globe,
  LayoutGrid,
  Maximize2,
  PanelRight,
  Play,
  Plus,
  RefreshCw,
  Save,
  Send,
  Server,
  Sliders,
  Sparkles,
  Terminal,
  Trash2,
  Wand2,
  Workflow as WorkflowIcon,
  X,
  ZoomIn,
  ZoomOut,
  type LucideIcon,
} from "lucide-react";
import {
  channelsApi,
  onWorkflowDone,
  onWorkflowEvent,
  onWorkflowsChanged,
  workflowsApi,
  type ChannelConfig,
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
    defaultCfg: () => ({ cron: "0 8 * * *", tz_offset_minutes: -new Date().getTimezoneOffset() }),
  },
  trigger_manual: {
    pt: "Manual",
    en: "Manual Trigger",
    cat: "trigger",
    color: "#fbbf24",
    icon: Play,
    defaultCfg: () => ({}),
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
      model: "",
      system: "Summarize concisely and highlight actionable issues.",
      prompt: "{{prev}}",
    }),
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
  { cat: "trigger", pt: "Gatilhos", en: "Triggers", kinds: ["trigger_cron", "trigger_manual"] },
  { cat: "collect", pt: "Coleta & Ação", en: "Collect & Act", kinds: ["shell", "ssh_command", "http_request"] },
  { cat: "logic", pt: "IA & Lógica", en: "AI & Logic", kinds: ["ai_agent", "condition", "transform"] },
  { cat: "deliver", pt: "Entrega", en: "Delivery", kinds: ["notify_channel", "save_file"] },
];

const NODE_KINDS: NodeKind[] = [
  "trigger_cron",
  "trigger_manual",
  "shell",
  "ssh_command",
  "http_request",
  "transform",
  "condition",
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
    case "ai_agent":
      return String(c.model || "default model");
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
}) {
  const lang = useLang();
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [channels, setChannels] = useState<ChannelConfig[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<Workflow | null>(null);
  const [dirty, setDirty] = useState(false);
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
  const [nodeTestInput, setNodeTestInput] = useState("");
  const [nodeTestOutput, setNodeTestOutput] = useState<string | null>(null);
  const [nodeTestBusy, setNodeTestBusy] = useState(false);

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
      const [wfs, chs] = await Promise.all([workflowsApi.list(), channelsApi.list()]);
      setWorkflows(wfs);
      setChannels(chs);
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
  }, [loadAll]);

  useEffect(() => {
    if (!selectedId) {
      setDraft(null);
      setRuns([]);
      return;
    }
    const found = workflows.find((w) => w.id === selectedId);
    if (found && !dirty) {
      const cloned = structuredClone(found);
      setDraft(cloned);
      setSelectedNodeId((prev) =>
        prev && found.nodes.some((n) => n.id === prev) ? prev : found.nodes[0]?.id ?? null,
      );
      requestAnimationFrame(() => fitToView(cloned));
    }
    void workflowsApi.runs(selectedId, 25).then(setRuns).catch(() => undefined);
  }, [selectedId, workflows, dirty, fitToView]);

  useEffect(() => {
    let offEv = () => {};
    let offDone = () => {};
    let offChanged = () => {};
    void onWorkflowEvent((ev) => {
      setLiveNodes((prev) => ({ ...prev, [ev.node_id]: ev }));
    }).then((fn) => {
      offEv = fn;
    });
    void onWorkflowDone((run) => {
      setRunningId((cur) => (cur === run.workflow_id ? null : cur));
      void loadAll(true);
      if (run.workflow_id === selectedId) {
        void workflowsApi.runs(run.workflow_id, 25).then(setRuns).catch(() => undefined);
      }
      setBanner({
        text: run.status === "ok" ? tr(lang, "Execução concluída", "Run completed") : run.summary,
        tone: run.status === "ok" ? "ok" : "err",
      });
    }).then((fn) => {
      offDone = fn;
    });
    void onWorkflowsChanged(() => {
      setDirty(false);
      void loadAll(true);
    }).then((fn) => {
      offChanged = fn;
    });
    return () => {
      offEv();
      offDone();
      offChanged();
    };
  }, [loadAll, selectedId, lang]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (connectingFrom) setConnectingFrom(null);
        else if (addMenuOpen) setAddMenuOpen(false);
        else if (emblemMenuOpen) setEmblemMenuOpen(false);
        else if (!props.inline && !channelsOpen && !templatesOpen) props.onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props, channelsOpen, templatesOpen, connectingFrom, addMenuOpen, emblemMenuOpen]);

  const selectedNode = useMemo(
    () => draft?.nodes.find((n) => n.id === selectedNodeId) ?? null,
    [draft, selectedNodeId],
  );

  const patchDraft = (fn: (w: Workflow) => Workflow) => {
    setDraft((prev) => (prev ? fn(prev) : prev));
    setDirty(true);
  };

  const handleSave = async (): Promise<Workflow | null> => {
    if (!draft) return null;
    try {
      const saved = await workflowsApi.save(draft);
      setDirty(false);
      setDraft(structuredClone(saved));
      await loadAll(true);
      setBanner({ text: tr(lang, "Workflow salvo", "Workflow saved"), tone: "ok" });
      return saved;
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
      return null;
    }
  };

  const handleRunNow = async () => {
    if (!draft) return;
    const saved = dirty ? await handleSave() : draft;
    if (!saved) return;
    setLiveNodes({});
    setRunningId(saved.id);
    setBanner({ text: tr(lang, "Executando fluxo...", "Running workflow..."), tone: "" });
    try {
      await workflowsApi.runNow(saved.id);
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
    const saved = await workflowsApi.save(fresh);
    setDirty(false);
    await loadAll(false);
    setSelectedId(saved.id);
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
    try {
      const t = await workflowsApi.templates();
      setTemplates(t);
      setTemplatesOpen(true);
    } catch (e) {
      setBanner({ text: String(e), tone: "err" });
    }
  };

  const instantiateTemplate = async (tpl: Workflow) => {
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
    const saved = await workflowsApi.save(copy);
    setTemplatesOpen(false);
    setDirty(false);
    await loadAll(false);
    setSelectedId(saved.id);
  };

  const runSingleNodeTest = async (targetNode?: WorkflowNode) => {
    const nodeToTest = targetNode ?? selectedNode;
    if (!nodeToTest || nodeTestBusy) return;
    setSelectedNodeId(nodeToTest.id);
    setInspOpen(true);
    setInspTab("insp");
    setNodeTestBusy(true);
    setNodeTestOutput(null);
    try {
      const out = await workflowsApi.testNode(nodeToTest, nodeTestInput);
      setNodeTestOutput(out || "(sem saída)");
    } catch (e) {
      setNodeTestOutput(`Erro: ${String(e)}`);
    } finally {
      setNodeTestBusy(false);
    }
  };

  const handleWheelZoom = (e: React.WheelEvent<HTMLDivElement>) => {
    e.preventDefault();
    const vp = viewportRef.current;
    if (!vp) return;
    const rect = vp.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const factor = e.deltaY < 0 ? 1.1 : 0.9;
    const nextZoom = Math.max(0.35, Math.min(1.8, +(zoom * factor).toFixed(3)));
    const scaleRatio = nextZoom / zoom;
    setPan({
      x: mx - (mx - pan.x) * scaleRatio,
      y: my - (my - pan.y) * scaleRatio,
    });
    setZoom(nextZoom);
  };

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
          {workflows.map((w) => {
            const isRunning = runningId === w.id || w.last_status === "running";
            const status = isRunning
              ? "running"
              : w.last_status === "error"
                ? "error"
                : w.enabled
                  ? "on"
                  : "idle";
            return (
              <div
                key={w.id}
                className={"wf-roster-card" + (selectedId === w.id ? " on" : "")}
                onClick={() => {
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
                }}
              >
                <BotEmblem icon={w.icon} size={34} status={status} />
                <div className="wf-roster-meta">
                  <span className="wf-roster-name">{w.name}</span>
                  <span className="wf-roster-sub">
                    <span>{workflowScheduleLabel(w, lang)}</span>
                    <span>· {w.nodes.length} nós</span>
                  </span>
                </div>
                <input
                  type="checkbox"
                  className="wf-switch"
                  checked={w.enabled}
                  aria-label={tr(lang, "Ativar workflow", "Enable workflow")}
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => {
                    const next = e.target.checked;
                    void workflowsApi
                      .toggle(w.id, next)
                      .then(() => loadAll(true))
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
                onChange={(e) => patchDraft((w) => ({ ...w, name: e.target.value }))}
                aria-label={tr(lang, "Nome do workflow", "Workflow name")}
              />
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
                title={tr(lang, "Excluir workflow", "Delete workflow")}
                onClick={() => {
                  if (
                    !window.confirm(
                      tr(lang, `Excluir "${draft.name}"?`, `Delete "${draft.name}"?`),
                    )
                  ) {
                    return;
                  }
                  void workflowsApi.delete(draft.id).then(() => {
                    setDirty(false);
                    void loadAll(false);
                  });
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
          onWheel={handleWheelZoom}
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
            const nx = Math.round(dn.origX + (e.clientX - dn.startX) / zoom);
            const ny = Math.round(dn.origY + (e.clientY - dn.startY) / zoom);
            patchDraft((w) => ({
              ...w,
              nodes: w.nodes.map((n) => (n.id === dn.id ? { ...n, x: nx, y: ny } : n)),
            }));
          }}
          onPointerUp={() => {
            dragNodeRef.current = null;
            dragPanRef.current = null;
            setIsPanning(false);
          }}
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
                      dragNodeRef.current = {
                        id: n.id,
                        startX: e.clientX,
                        startY: e.clientY,
                        origX: n.x,
                        origY: n.y,
                      };
                    }}
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
                  setDraft(arranged);
                  setDirty(true);
                  requestAnimationFrame(() => fitToView(arranged));
                }}
                title={tr(lang, "Auto-organizar blocos", "Auto-arrange blocks")}
              >
                <Wand2 size={12} />
                <span>{tr(lang, "Organizar", "Arrange")}</span>
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
                  allNodes={draft.nodes}
                  channels={channels}
                  models={props.models}
                  lang={lang}
                  onOpenChannels={() => setChannelsOpen(true)}
                  onChange={(next) =>
                    patchDraft((w) => ({
                      ...w,
                      nodes: w.nodes.map((x) => (x.id === next.id ? next : x)),
                    }))
                  }
                  onDelete={() => deleteNode(selectedNode.id)}
                  testInput={nodeTestInput}
                  onTestInput={setNodeTestInput}
                  testOutput={nodeTestOutput}
                  testBusy={nodeTestBusy}
                  onTest={() => void runSingleNodeTest()}
                />
              ) : (
                <p className="pn-hint">
                  {tr(
                    lang,
                    "Clique em qualquer bloco no canvas para configurá-lo.",
                    "Click any block on the canvas to edit its settings.",
                  )}
                </p>
              )}
            </div>
          ) : (
            <div className="wf-insp-body">
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => selectedId && void workflowsApi.runs(selectedId, 25).then(setRuns)}
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
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}

function NodeInspector(props: {
  node: WorkflowNode;
  allNodes: WorkflowNode[];
  channels: ChannelConfig[];
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
}) {
  const { node, lang } = props;
  const cfg = node.config ?? {};
  const setCfg = (patch: Record<string, unknown>) =>
    props.onChange({ ...node, config: { ...cfg, ...patch } });

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
          tz={Number(cfg.tz_offset_minutes ?? -new Date().getTimezoneOffset())}
          lang={lang}
          onChange={(cron, tz) => setCfg({ cron, tz_offset_minutes: tz })}
        />
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
              <label>{tr(lang, "Caminho / Padrão", "Path / Pattern")}</label>
              <input
                value={String(cfg.pattern ?? "")}
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
  tz: number;
  lang: Lang;
  onChange: (cron: string, tz: number) => void;
}) {
  const { lang } = props;
  const applyPreset = (expr: string) =>
    props.onChange(expr, -new Date().getTimezoneOffset());

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
          value={props.tz}
          onChange={(e) => props.onChange(props.cron, Number(e.target.value) || 0)}
        />
      </div>
    </>
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

  const selectChannel = (ch: ChannelConfig) => {
    setSelectedId(ch.id);
    setDraft(structuredClone(ch));
    setMsg(null);
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

  const test = async () => {
    setBusy(true);
    setMsg(null);
    try {
      await channelsApi.save({ ...draft, updated_at_ms: Date.now() });
      const res = await channelsApi.test(draft.id);
      props.onUpdated();
      setMsg({ text: res, tone: "ok" });
    } catch (e) {
      setMsg({ text: String(e), tone: "err" });
    } finally {
      setBusy(false);
    }
  };

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
                  <label>{tr(lang, "Número / Grupo padrão", "Default target phone / group")}</label>
                  <input
                    value={String(cfg.default_target ?? "")}
                    placeholder="5511999999999"
                    onChange={(e) => setCfg({ default_target: e.target.value })}
                  />
                </div>
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
                    onChange={(e) => setCfg({ default_target: e.target.value })}
                  />
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
              <button type="button" className="btn btn-sm" onClick={() => void test()} disabled={busy}>
                <Send size={13} />
                <span>{tr(lang, "Enviar Teste", "Send Test")}</span>
              </button>
              {props.channels.some((c) => c.id === draft.id) ? (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() =>
                    void channelsApi.delete(draft.id).then(() => {
                      props.onUpdated();
                      createNew();
                    })
                  }
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
