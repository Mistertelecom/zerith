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
}

export const BOT_EMBLEMS: readonly EmblemSpec[] = [
  { id: "activity", label: "NetOps / Radar", icon: Activity },
  { id: "bot", label: "Bot / Orchestrator", icon: Bot },
  { id: "trending-up", label: "Finance / Market", icon: TrendingUp },
  { id: "rocket", label: "Executor / Fast", icon: Rocket },
  { id: "globe", label: "Web / Research", icon: Globe },
  { id: "terminal", label: "Linux / DevOps", icon: Terminal },
  { id: "cpu", label: "Proxmox / Server", icon: Cpu },
  { id: "zap", label: "FTTH / Fiber", icon: Zap },
  { id: "bell", label: "NOC Alerts", icon: Bell },
  { id: "shield", label: "Security", icon: ShieldCheck },
  { id: "flame", label: "Hot / Ops", icon: Flame },
  { id: "sparkles", label: "AI / Creative", icon: Sparkles },
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
      className={`bot-emblem em-${spec.id}`}
      style={{
        width: size,
        height: size,
        borderRadius: Math.max(8, Math.round(size * 0.28)),
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
