// What the transcript becomes while a turn runs. Pure functions over the event
// stream: the transcript is a list of blocks, and a turn mutates the last one.
import type { LearnedItem, Question, Todo, TurnEvent } from "./api";

export type Block =
  | { kind: "user"; text: string }
  | {
      kind: "assistant";
      text: string;
      reasoning: string;
      tools: ToolRow[];
      error?: string;
      streaming: boolean;
      /** Row id of the stored answer (`done.stored` / history): keys its diff. */
      stored?: number;
    }
  /** `learned`: what the background review saved after the turn (with undo). */
  | { kind: "note"; text: string; learned?: LearnedItem[]; sub?: string };

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
      // RTK reports bytes; the runtime counts 4 bytes per token
      state.saved += Math.floor(Math.max(0, ev.before - ev.after) / 4);
      return state;
    case "compacted":
      state.blocks.push({ kind: "note", text: "Contexto compactado" });
      return state;
    case "subagent": {
      // One line per subagent, updated in place: its live progress (step,
      // tool) and then how it ended, never a stream of notes.
      const word: Record<string, string> = {
        running: "rodando",
        done: "concluído",
        failed: "falhou",
        cancelled: "parado",
      };
      const text = `Subagente ${ev.name}: ${word[ev.state] ?? ev.state}${ev.summary ? " — " + ev.summary : ""}`;
      const prev = state.blocks.find(
        (b): b is Block & { kind: "note" } => b.kind === "note" && b.sub === ev.id,
      );
      if (prev) prev.text = text;
      else state.blocks.push({ kind: "note", text, sub: ev.id });
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
      return state;
  }
}

/** Token count as the meter shows it. */
export function tokenTotal(usage: TurnState["usage"]): number {
  return usage ? usage.input + usage.output : 0;
}
