// What the transcript becomes while a turn runs. Pure functions over the event
// stream: the transcript is a list of blocks, and a turn changes the last one.
// Self-check: scripts/turn.check.ts.
import type { ChatMessage, LearnedItem, Question, Todo, TurnEvent } from "./api";

export type Block =
  /** `id`: the stored row (history), for "Editar". */
  | { kind: "user"; text: string; id?: number }
  | {
      kind: "assistant";
      text: string;
      reasoning: string;
      tools: ToolRow[];
      error?: string;
      streaming: boolean;
      /** Row id of the stored answer (`done.stored` / history): keys its diff. */
      stored?: number;
      /** Tool calls already behind the text, so text after a new one starts a paragraph. */
      toolsAtText?: number;
    }
  /** `learned`: what the background review saved after the turn (with undo). */
  | { kind: "note"; text: string; learned?: LearnedItem[]; sub?: string }
  /** Antigravity-style live/expandable card for a delegated subagent. */
  | {
      kind: "subagent";
      id: string;
      name: string;
      model: string;
      state: string;
      summary: string;
      steps: string[];
      startedAt: number;
      updatedAt: number;
    };

export interface ToolRow {
  id: string;
  name: string;
  args: string;
  ok?: boolean;
  preview?: string;
  progress?: string;
}

export interface TurnState {
  blocks: Block[];
  tasks: Todo[];
  approvals: { id: number; tool: string; summary: string }[];
  question: { id: number; questions: Question[] } | null;
  usage: {
    input: number;
    output: number;
    cacheRead?: number;
    cacheWrite?: number;
    ttftMs?: number;
  } | null;
  saved: number;
  /** The measured part of `saved`: bytes RTK cut, at 4 bytes per token. */
  savedRtk: number;
  route: { provider: string; model: string } | null;
  /** Children the turn delegated to, live: who runs on which model. */
  subagents: SubagentRun[];
  busy: boolean;
}

export interface SubagentRun {
  id: string;
  name: string;
  /** The model the child runs on (empty when unknown). */
  model: string;
  state: string;
  summary: string;
  /** Epoch ms of the last update. */
  at: number;
}

export const emptyTurn: TurnState = {
  blocks: [],
  tasks: [],
  approvals: [],
  question: null,
  usage: null,
  saved: 0,
  savedRtk: 0,
  route: null,
  subagents: [],
  busy: false,
};

/** The running answer, as a fresh copy in a fresh block list: the previous
 * state stays intact and every other block keeps its reference, so React
 * re-renders only the answer that changed. Created on demand so a tool event
 * before the first token still lands somewhere. */
function assistant(state: TurnState): Block & { kind: "assistant" } {
  const blocks = (state.blocks = [...state.blocks]);
  for (let i = blocks.length - 1; i >= 0; i--) {
    const b = blocks[i];
    if (b.kind === "assistant") return (blocks[i] = { ...b, tools: [...b.tools] }) as Block & { kind: "assistant" };
  }
  const created = { kind: "assistant" as const, text: "", reasoning: "", tools: [], streaming: true };
  blocks.push(created);
  return created;
}

/** Applies one event. Never mutates `prev`: returns a new state that shares
 * every block the event did not touch. */
export function applyEvent(prev: TurnState, ev: TurnEvent): TurnState {
  const state = { ...prev };
  switch (ev.type) {
    case "delta": {
      const a = assistant(state);
      // Text after a tool call starts a new paragraph, not the old sentence.
      if (a.tools.length > (a.toolsAtText ?? 0) && a.text && !/\s$/.test(a.text)) a.text += "\n\n";
      a.toolsAtText = a.tools.length;
      a.text += ev.text;
      return state;
    }
    case "reasoning":
      assistant(state).reasoning += ev.text;
      return state;
    case "toolCall": {
      assistant(state).tools.push({ id: ev.id, name: ev.name, args: ev.args });
      return state;
    }
    case "toolResult": {
      const a = assistant(state);
      let i = -1;
      for (let k = a.tools.length - 1; k >= 0; k--) {
        if (a.tools[k].id === ev.id && a.tools[k].ok === undefined) {
          i = k;
          break;
        }
      }
      if (i < 0) {
        for (let k = a.tools.length - 1; k >= 0; k--) {
          if (a.tools[k].id === ev.id) {
            i = k;
            break;
          }
        }
      }
      if (i >= 0) {
        a.tools[i] = { ...a.tools[i], ok: ev.ok, preview: ev.preview };
      } else {
        a.tools.push({ id: ev.id, name: ev.name, args: "", ok: ev.ok, preview: ev.preview });
      }
      return state;
    }
    case "toolProgress": {
      const a = assistant(state);
      let idx = -1;
      for (let i = a.tools.length - 1; i >= 0; i--) {
        if (a.tools[i].ok === undefined && a.tools[i].name === ev.tool) {
          idx = i;
          break;
        }
      }
      if (idx < 0) {
        for (let i = a.tools.length - 1; i >= 0; i--) {
          if (a.tools[i].ok === undefined) {
            idx = i;
            break;
          }
        }
      }
      if (idx >= 0) {
        const next = ((a.tools[idx].progress ?? "") + ev.chunk).slice(-4000);
        a.tools[idx] = { ...a.tools[idx], progress: next };
      }
      return state;
    }
    case "skillsLoaded": {
      const a = assistant(state);
      a.tools = [
        ...a.tools,
        ...ev.names.map((name, i) => ({
          id: `skill-preload-${a.tools.length + i}`,
          name: "skill",
          args: JSON.stringify({ name }),
          ok: true,
          preview: "Pré-carregada: o pedido combina com esta skill.",
        })),
      ];
      return state;
    }
    case "steered":
      state.blocks = [...state.blocks, { kind: "note", text: `Direcionado: ${ev.text}` }];
      return state;
    case "approval":
      if (prev.approvals.some((a) => a.id === ev.id)) return prev;
      state.approvals = [
        ...prev.approvals,
        { id: ev.id, tool: ev.tool, summary: ev.summary },
      ];
      return state;
    case "question":
      state.question = { id: ev.id, questions: ev.questions };
      return state;
    case "usage":
      state.usage = {
        input: ev.input,
        output: ev.output,
        cacheRead: ev.cacheRead ?? 0,
        cacheWrite: ev.cacheWrite ?? 0,
        ttftMs: ev.ttftMs ?? 0,
      };
      return state;
    case "tasks":
      state.tasks = ev.items;
      return state;
    case "route":
      state.route = { provider: ev.provider, model: ev.model };
      return state;
    case "saved":
      // RTK reports bytes; the runtime counts 4 bytes per token
      {
        const t = Math.floor(Math.max(0, ev.before - ev.after) / 4);
        state.saved += t;
        state.savedRtk += t;
      }
      return state;
    case "compacted":
      state.blocks = [...state.blocks, { kind: "note", text: "Contexto compactado" }];
      return state;
    case "autoCompacted": {
      const n = (x: number) => x.toLocaleString("pt-BR");
      const text = `Conversa resumida automaticamente (${n(ev.before)} → ${n(ev.after)} tokens)`;
      state.blocks = [...state.blocks, { kind: "note", text }];
      return state;
    }
    case "subagent": {
      const now = Date.now();
      state.subagents = [...(state.subagents ?? [])];
      const known = state.subagents.findIndex((s) => s.id === ev.id);
      const prevSub = known >= 0 ? state.subagents[known] : null;
      const run: SubagentRun = {
        id: ev.id,
        name: ev.name,
        model: ev.model || prevSub?.model || "",
        state: ev.state,
        summary: ev.summary,
        at: now,
      };
      if (known >= 0) state.subagents[known] = run;
      else state.subagents.push(run);
      const at = state.blocks.findIndex(
        (b) => b.kind === "subagent" && b.id === ev.id,
      );
      state.blocks = [...state.blocks];
      if (at >= 0) {
        const prevBlk = state.blocks[at] as Extract<Block, { kind: "subagent" }>;
        const summary = ev.summary?.trim() ?? "";
        const steps =
          summary && prevBlk.steps[prevBlk.steps.length - 1] !== summary
            ? [...prevBlk.steps.slice(-11), summary]
            : prevBlk.steps;
        state.blocks[at] = {
          ...prevBlk,
          name: ev.name,
          model: ev.model || prevBlk.model,
          state: ev.state,
          summary,
          steps,
          updatedAt: now,
        };
      } else {
        const summary = ev.summary?.trim() ?? "";
        state.blocks.push({
          kind: "subagent",
          id: ev.id,
          name: ev.name,
          model: ev.model ?? "",
          state: ev.state,
          summary,
          steps: summary ? [summary] : [],
          startedAt: now,
          updatedAt: now,
        });
      }
      return state;
    }
    case "done": {
      const a = assistant(state);
      a.streaming = false;
      // the turn's full saving: RTK plus the Ponytail/Caveman estimates
      state.saved = Math.max(state.saved, ev.savedTokens);
      if (!a.text && ev.text) a.text = ev.text;
      if (ev.stored != null) a.stored = ev.stored;
      return state;
    }
    case "failed": {
      const a = assistant(state);
      a.streaming = false;
      a.error = ev.message;
      return state;
    }
    default:
      return prev;
  }
}

/** `turn:done`: the turn is over, whatever it said. Nothing is left to answer,
 * and a turn that ended without its own `done`/`failed` says so instead of
 * leaving a silent or still-"typing" answer behind. */
export function finishTurn(prev: TurnState, cancelled = false): TurnState {
  const state = { ...prev, busy: false, approvals: [], question: null };
  const last = [...state.blocks].reverse().find((b) => b.kind === "assistant");
  // Stop pressed: not a failure. Close the partial answer and say so neutrally.
  if (cancelled) {
    if (last?.streaming) assistant(state).streaming = false;
    state.blocks = [...state.blocks, { kind: "note", text: "Interrompido" }];
    return state;
  }
  if (!last || (last.streaming && !last.error)) {
    const a = assistant(state);
    a.streaming = false;
    a.error = "O turno terminou sem resposta.";
  }
  return state;
}

/** The prompt just sent has no row id until the store has it ("Editar" needs
 * one): it is the newest user row stored before the answer `stored` (all rows
 * when the turn failed before storing an answer). By position, not by text: a
 * repeated prompt or a stored text that differs still finds its row.
 * ponytail: hook/tool context rows are user rows too and would win if a turn
 * adds one; send `raw` kinds in ChatMessage if that shows up. */
export function borrowIds(blocks: Block[], list: ChatMessage[], stored?: number | null): Block[] {
  let i = blocks.length - 1;
  while (i >= 0 && !(blocks[i].kind === "user" && (blocks[i] as { id?: number }).id == null)) i--;
  let row: ChatMessage | undefined;
  for (const m of list) if (m.role === "user" && m.id != null && (stored == null || m.id < stored)) row = m;
  if (i < 0 || !row || blocks.some((b) => b.kind === "user" && b.id === row!.id)) return blocks;
  const out = [...blocks];
  out[i] = { ...(blocks[i] as Block & { kind: "user" }), id: row.id };
  return out;
}

/** Stored rows as the live view draws them: everything between two prompts is
 * one answer (text of every step, its tool calls as tool lines, the final row's
 * id), so a reopened chat shows tool output as tool lines, not as answers, and
 * a step that only called tools leaves no empty bubble. */
export function toBlocks(list: ChatMessage[]): Block[] {
  const out: Block[] = [];
  let a: (Block & { kind: "assistant" }) | null = null;
  // `tool_calls`: [{id, name, arguments}] on a step, {tool_call_id} on a result
  // (old rows may lack them; the tool line then just lacks its argument).
  const calls = new Map<string, string>();
  for (const m of list) {
    if (m.role === "user") {
      out.push({ kind: "user", text: m.content, id: m.id });
      a = null;
      continue;
    }
    if (!a) out.push((a = { kind: "assistant", text: "", reasoning: "", tools: [], streaming: false }));
    const tc = m.tool_calls as { id?: string; arguments?: string; tool_call_id?: string }[] | { tool_call_id?: string } | null | undefined;
    if (m.role === "tool") {
      const id = Array.isArray(tc) ? undefined : tc?.tool_call_id;
      a.tools.push({ id: id ?? `row${m.id ?? a.tools.length}`, name: m.tool_name ?? "ferramenta", args: (id && calls.get(id)) || "", ok: !toolFailed(m.content), preview: m.content });
      continue;
    }
    if (Array.isArray(tc)) for (const c of tc) if (c.id) calls.set(c.id, c.arguments ?? "");
    if (m.content) a.text = a.text ? `${a.text}\n\n${m.content}` : m.content;
    if (m.reasoning) a.reasoning = a.reasoning ? `${a.reasoning}\n\n${m.reasoning}` : m.reasoning;
    if (m.role === "assistant") a.stored = m.id;
  }
  return out.filter((b) => b.kind !== "assistant" || b.text || b.tools.length || b.reasoning);
}

/** A stored tool output the runtime wrote for a failed call: `erro: …`
 * (the tool returned an error), `negado…` (policy, hook or the person) or
 * `[cancelado]`.
 * ponytail: the store keeps no success flag, so this reads the prefix; a
 * successful output that itself starts with `erro:` shows as failed. Store
 * `ok` next to `tool_call_id` in the runtime if that ever matters. */
export function toolFailed(output: string): boolean {
  return /^(erro:|negado|\[cancelado\])/.test(output);
}

/** Token count as the meter shows it. */
export function tokenTotal(usage: TurnState["usage"]): number {
  return usage ? usage.input + usage.output : 0;
}
