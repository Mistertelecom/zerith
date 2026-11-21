import {
  Activity,
  Bell,
  Bot,
  Cpu,
  Flame,
  Globe,
  Rocket,
  ShieldCheck,
  Sparkles,
  Terminal,
  TrendingUp,
  Zap,
  type LucideIcon,
} from "lucide-react";

export interface EmblemSpec {
  id: string;
  label: string;
  icon: LucideIcon;
  bg: string;
  fg: string;
  border: string;
}

export const BOT_EMBLEMS: readonly EmblemSpec[] = [
  {
    id: "activity",
    label: "NetOps / Radar",
    icon: Activity,
    bg: "linear-gradient(135deg, rgba(16, 185, 129, 0.28), rgba(5, 150, 105, 0.12))",
    fg: "#34d399",
    border: "rgba(52, 211, 153, 0.4)",
  },
  {
    id: "bot",
    label: "Bot / Orchestrator",
    icon: Bot,
    bg: "linear-gradient(135deg, rgba(139, 92, 246, 0.30), rgba(109, 40, 217, 0.14))",
    fg: "#c4b5fd",
    border: "rgba(196, 181, 253, 0.4)",
  },
  {
    id: "trending-up",
    label: "Finance / Market",
    icon: TrendingUp,
    bg: "linear-gradient(135deg, rgba(245, 158, 11, 0.28), rgba(217, 119, 6, 0.12))",
    fg: "#fbbf24",
    border: "rgba(251, 191, 36, 0.4)",
  },
  {
    id: "rocket",
    label: "Executor / Fast",
    icon: Rocket,
    bg: "linear-gradient(135deg, rgba(6, 182, 212, 0.28), rgba(8, 145, 178, 0.12))",
    fg: "#22d3ee",
    border: "rgba(34, 211, 238, 0.4)",
  },
  {
    id: "globe",
    label: "Web / Research",
    icon: Globe,
    bg: "linear-gradient(135deg, rgba(56, 189, 248, 0.28), rgba(2, 132, 199, 0.12))",
    fg: "#7dd3fc",
    border: "rgba(125, 211, 252, 0.4)",
  },
  {
    id: "terminal",
    label: "Linux / DevOps",
    icon: Terminal,
    bg: "linear-gradient(135deg, rgba(34, 197, 94, 0.26), rgba(21, 128, 61, 0.12))",
    fg: "#4ade80",
    border: "rgba(74, 222, 128, 0.4)",
  },
  {
    id: "cpu",
    label: "Proxmox / Server",
    icon: Cpu,
    bg: "linear-gradient(135deg, rgba(20, 184, 166, 0.28), rgba(15, 118, 110, 0.12))",
    fg: "#2dd4bf",
    border: "rgba(45, 212, 191, 0.4)",
  },
  {
    id: "zap",
    label: "FTTH / Fiber",
    icon: Zap,
    bg: "linear-gradient(135deg, rgba(234, 179, 8, 0.28), rgba(161, 98, 7, 0.12))",
    fg: "#fde047",
    border: "rgba(253, 224, 71, 0.4)",
  },
  {
    id: "bell",
    label: "NOC Alerts",
    icon: Bell,
    bg: "linear-gradient(135deg, rgba(244, 63, 94, 0.28), rgba(190, 18, 60, 0.12))",
    fg: "#fda4af",
    border: "rgba(253, 164, 175, 0.4)",
  },
  {
    id: "shield",
    label: "Security",
    icon: ShieldCheck,
    bg: "linear-gradient(135deg, rgba(239, 68, 68, 0.26), rgba(185, 28, 28, 0.12))",
    fg: "#fca5a5",
    border: "rgba(252, 165, 165, 0.4)",
  },
  {
    id: "flame",
    label: "Hot / Ops",
    icon: Flame,
    bg: "linear-gradient(135deg, rgba(249, 115, 22, 0.28), rgba(194, 65, 12, 0.12))",
    fg: "#fdba74",
    border: "rgba(253, 186, 116, 0.4)",
  },
  {
    id: "sparkles",
    label: "AI / Creative",
    icon: Sparkles,
    bg: "linear-gradient(135deg, rgba(99, 102, 241, 0.28), rgba(67, 56, 202, 0.12))",
    fg: "#a5b4fc",
    border: "rgba(165, 180, 252, 0.4)",
  },
] as const;

const BY_ID = new Map<string, EmblemSpec>(BOT_EMBLEMS.map((e) => [e.id, e]));

export function emblemSpec(id?: string | null): EmblemSpec {
  if (!id) return BOT_EMBLEMS[1];
  if (id === "workflow") return BOT_EMBLEMS[1];
  if (id === "file-text") return BOT_EMBLEMS[11];
  return BY_ID.get(id) ?? BOT_EMBLEMS[1];
}

export function emblemForAgent(name: string): string {
  const n = name.toLowerCase();
  if (n.includes("finance") || n.includes("stock") || n.includes("market")) return "trending-up";
  if (n.includes("netops") || n.includes("bgp") || n.includes("router")) return "activity";
  if (n.includes("proxmox") || n.includes("cluster") || n.includes("infra")) return "cpu";
  if (n.includes("linux") || n.includes("devops") || n.includes("shell")) return "terminal";
  if (n.includes("ftth") || n.includes("olt") || n.includes("gpon")) return "zap";
  if (n.includes("noc") || n.includes("zabbix") || n.includes("alert")) return "bell";
  if (n.includes("workflow")) return "bot";
  if (n.includes("executor")) return "rocket";
  if (n.includes("sec") || n.includes("guard") || n.includes("hunt")) return "shield";
  if (n.includes("research") || n.includes("web") || n.includes("search")) return "globe";
  return BOT_EMBLEMS[Math.abs(hashStr(n)) % BOT_EMBLEMS.length].id;
}

function hashStr(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = (h * 31 + s.charCodeAt(i)) | 0;
  }
  return h;
}

export function BotEmblem(props: {
  icon?: string | null;
  size?: number;
  status?: "on" | "running" | "error" | "idle" | null;
}) {
  const spec = emblemSpec(props.icon);
  const Icon = spec.icon;
  const size = props.size ?? 32;
  const iconSize = Math.round(size * 0.5);
  return (
    <span
      className="bot-emblem"
      style={{
        width: size,
        height: size,
        borderRadius: Math.max(8, Math.round(size * 0.28)),
        background: spec.bg,
        color: spec.fg,
        border: `1px solid ${spec.border}`,
      }}
      aria-hidden
    >
      <Icon size={iconSize} strokeWidth={2.1} />
      {props.status && props.status !== "idle" ? (
        <span className={`bot-emblem-dot ${props.status}`} />
      ) : null}
    </span>
  );
}
