// What the transcript becomes while a turn runs. Pure functions over the event
// stream: the transcript is a list of blocks, and a turn mutates the last one.
import type { Question, Todo, TurnEvent } from "./api";

export type Block =
  | { kind: "user"; text: string }
  | {
      kind: "assistant";
      text: string;
      reasoning: string;
      tools: ToolRow[];
      error?: string;
      streaming: boolean;
    }
  | { kind: "note"; text: string };

export interface ToolRow {
  id: string;
  name: string;
  args: string;
  ok?: boolean;
  preview?: string;
}

export interface TurnState {
  blocks: Block[];
  tasks: Todo[];
  approval: { id: number; tool: string; summary: string } | null;
  question: { id: number; questions: Question[] } | null;
  usage: { input: number; output: number } | null;
  saved: number;
  route: { provider: string; model: string } | null;
  busy: boolean;
}

export const emptyTurn: TurnState = {
  blocks: [],
  tasks: [],
  approval: null,
  question: null,
  usage: null,
  saved: 0,
  route: null,
  busy: false,
};

/** The running answer. Created on demand so a tool event before the first
 * token still lands somewhere. */
function assistant(state: TurnState): Block & { kind: "assistant" } {
  for (let i = state.blocks.length - 1; i >= 0; i--) {
    const b = state.blocks[i];
    if (b.kind === "assistant") return b;
  }
  const created = { kind: "assistant" as const, text: "", reasoning: "", tools: [], streaming: true };
  state.blocks.push(created);
  return created;
}

/** Applies one event. Mutates the draft state it is given; the caller copies. */
export function applyEvent(state: TurnState, ev: TurnEvent): TurnState {
  switch (ev.type) {
    case "delta": {
      const a = assistant(state);
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
      const row = a.tools.find((t) => t.id === ev.id);
      if (row) {
        row.ok = ev.ok;
        row.preview = ev.preview;
      } else {
        a.tools.push({ id: ev.id, name: ev.name, args: "", ok: ev.ok, preview: ev.preview });
      }
      return state;
    }
    case "approval":
      state.approval = { id: ev.id, tool: ev.tool, summary: ev.summary };
      return state;
    case "question":
      state.question = { id: ev.id, questions: ev.questions };
      return state;
    case "usage":
      state.usage = { input: ev.input, output: ev.output };
      return state;
    case "tasks":
      state.tasks = ev.items;
      return state;
    case "route":
      state.route = { provider: ev.provider, model: ev.model };
      return state;
    case "saved":
      state.saved += Math.max(0, ev.before - ev.after);
      return state;
    case "compacted":
      state.blocks.push({ kind: "note", text: "Contexto compactado" });
      return state;
    case "subagent":
      state.blocks.push({
        kind: "note",
        text: `Subagente ${ev.name}: ${ev.state}${ev.summary ? " — " + ev.summary : ""}`,
      });
      return state;
    case "done": {
      const a = assistant(state);
      a.streaming = false;
      if (!a.text && ev.text) a.text = ev.text;
      return state;
    }
    case "failed": {
      const a = assistant(state);
      a.streaming = false;
      a.error = ev.message;
      return state;
    }
    default:
      return state;
  }
}

/** Token count as the meter shows it. */
export function tokenTotal(usage: TurnState["usage"]): number {
  return usage ? usage.input + usage.output : 0;
}
