import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

// Provider marks. First choice: LobeHub's monochrome SVGs (assets/brand, MIT),
// drawn in currentColor so they read on black and on white at any size.
// Fallback: 9router's colour PNGs (assets/providers, MIT) on a white tile.
import { BRAND } from "../assets/brand/map";

const MARKS = import.meta.glob("../assets/brand/*.svg", {
  eager: true,
  query: "?raw",
  import: "default",
}) as Record<string, string>;
const MARK_BY_NAME: Record<string, string> = {};
for (const [path, svg] of Object.entries(MARKS)) {
  MARK_BY_NAME[path.split("/").pop()!.replace(/\.svg$/, "")] =
    // fill the box; the source is sized 1em
    svg.replace(/\s(width|height)="1em"/g, "").replace(/\sstyle="[^"]*"/, "");
}

const LOGOS = import.meta.glob("../assets/providers/*.{png,svg}", {
  eager: true,
  import: "default",
}) as Record<string, string>;

const BY_NAME: Record<string, string> = {};
for (const [path, url] of Object.entries(LOGOS)) {
  const name = path.split("/").pop()!.replace(/\.(png|svg)$/, "");
  BY_NAME[name] = url;
}

// Names that differ between the core and the asset files.
const ALIAS: Record<string, string> = {
  "claude-code": "claude",
  google: "gemini",
  "lm-studio": "lmstudio",
  moonshot: "kimi",
  zhipu: "glm",
  grok: "xai",
  "black-forest-labs": "black-forest-labs",
  bytedance: "byteplus",
  "bytedance-seed": "byteplus",
};

/** The key a provider, account or model id resolves to ("openai/gpt-5" uses openai). */
function keyOf(id: string): string[] {
  const key = id.toLowerCase().split("/")[0].trim();
  const head = key.split(/[-_ .]/)[0];
  return [key, ALIAS[key], head, ALIAS[head]].filter((k): k is string => !!k);
}

/** Monochrome SVG markup for a provider, if LobeHub has its mark. */
export function markFor(id: string | null | undefined): string | null {
  if (!id) return null;
  for (const k of keyOf(id)) {
    const file = BRAND[k];
    if (file && MARK_BY_NAME[file]) return MARK_BY_NAME[file];
  }
  return null;
}

/** The colour PNG for a provider, account or model id. */
export function logoFor(id: string | null | undefined): string | null {
  if (!id) return null;
  for (const k of keyOf(id)) if (BY_NAME[k]) return BY_NAME[k];
  return null;
}

export function Logo({ id, size = 20 }: { id: string | null | undefined; size?: number }) {
  const box = { width: size, height: size, borderRadius: Math.round(size / 4) };
  const mark = markFor(id);
  if (mark) {
    return (
      <span
        className="logo logo-mark"
        style={{ ...box, padding: Math.round(size * 0.16) }}
        aria-hidden
        // trusted: bundled SVG files, never user or model input
        dangerouslySetInnerHTML={{ __html: mark }}
      />
    );
  }
  const src = logoFor(id);
  if (src) return <img className="logo logo-img" src={src} alt="" style={box} draggable={false} />;
  const letter = (id ?? "?").trim().charAt(0).toUpperCase() || "?";
  return (
    <span className="logo logo-letter" style={{ ...box, fontSize: Math.round(size * 0.5) }} aria-hidden>
      {letter}
    </span>
  );
}

/** The Zerith mark: a rounded tile with a Z. Monochrome from
 * tokens: a white tile on the dark theme, a black one on the light theme
 * (same drawing as icons/logo-*.svg and the app icon). */
export function ZerithMark({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden className="zerith-mark">
      <rect width="32" height="32" rx="9" fill="var(--accent)" />
      <path
        d="M10 10.5h12L10 21.5h12"
        fill="none"
        stroke="var(--on-accent)"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function Card(props: {
  title?: string;
  icon?: LucideIcon;
  description?: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  const Icon = props.icon;
  return (
    <section className="card">
      {props.title ? (
        <header className="card-top">
          {Icon ? (
            <span className="card-icon">
              <Icon size={15} strokeWidth={2} />
            </span>
          ) : null}
          <div className="card-heading">
            <h3>{props.title}</h3>
            {props.description ? <p>{props.description}</p> : null}
          </div>
          {props.actions ? <div className="card-extra">{props.actions}</div> : null}
        </header>
      ) : null}
      <div className="card-rows">{props.children}</div>
    </section>
  );
}

/** Label and explanation on the left, the control on the right. */
export function Row(props: { label: string; hint?: ReactNode; stacked?: boolean; children: ReactNode }) {
  return (
    <div className={"setting-row" + (props.stacked ? " stacked" : "")}>
      <div className="setting-text">
        <span className="setting-label">{props.label}</span>
        {props.hint ? <span className="field-hint">{props.hint}</span> : null}
      </div>
      <div className="setting-control">{props.children}</div>
    </div>
  );
}

export function Segmented(props: {
  value: string;
  options: readonly (readonly [string, string])[];
  onPick: (v: string) => void;
  label: string;
}) {
  return (
    <div className="segmented" role="group" aria-label={props.label}>
      {props.options.map(([v, text]) => (
        <button
          key={v}
          className={"seg" + (v === props.value ? " on" : "")}
          aria-pressed={v === props.value}
          onClick={() => props.onPick(v)}
        >
          {text}
        </button>
      ))}
    </div>
  );
}

/** An on/off switch; the label says what "on" means, so no state word is needed. */
export function Switch(props: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.label}
      disabled={props.disabled}
      className={"switch" + (props.checked ? " on" : "")}
      onClick={() => props.onChange(!props.checked)}
    >
      <span className="switch-knob" />
    </button>
  );
}

export function Stat(props: { icon: LucideIcon; value: string; label: string; tone?: "ok" | "accent" }) {
  const Icon = props.icon;
  return (
    <div className={"stat" + (props.tone ? " " + props.tone : "")}>
      <span className="stat-icon">
        <Icon size={16} strokeWidth={2} />
      </span>
      <span className="num">{props.value}</span>
      <span className="cap">{props.label}</span>
    </div>
  );
}

export function EmptyState(props: { icon: LucideIcon; title: string; text?: string; children?: ReactNode }) {
  const Icon = props.icon;
  return (
    <div className="empty-state">
      <span className="empty-icon">
        <Icon size={22} strokeWidth={1.75} />
      </span>
      <strong>{props.title}</strong>
      {props.text ? <p>{props.text}</p> : null}
      {props.children}
    </div>
  );
}