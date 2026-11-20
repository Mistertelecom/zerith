// node scripts/turn.check.ts — the turn reducer, the stored-chat rebuild and
// the prompt id borrow.
import assert from "node:assert";
import { applyEvent, borrowIds, emptyTurn, finishTurn, toBlocks, toolFailed, type Block, type TurnState } from "../src/lib/turn.ts";
import type { ChatMessage } from "../src/lib/api.ts";

const run = (s: TurnState, evs: unknown[]) => evs.reduce<TurnState>((acc, ev) => applyEvent(acc, ev as never), s);
const asst = (b: Block) => b as Block & { kind: "assistant" };

// text after a tool call is a new paragraph
const s = run(emptyTurn, [
  { type: "delta", text: "Vou gerar." },
  { type: "toolCall", id: "1", name: "generate_video", args: "{}" },
  { type: "delta", text: "Pronto!" },
  { type: "delta", text: " Feito." },
]);
assert.equal(asst(s.blocks[0]).text, "Vou gerar.\n\nPronto! Feito.");
assert.equal(emptyTurn.blocks.length, 0, "emptyTurn is never written into");

// copy on write: the previous state and untouched blocks keep their references
const note: Block = { kind: "note", text: "antes" };
const before: TurnState = { ...emptyTurn, blocks: [note, { kind: "assistant", text: "a", reasoning: "", tools: [{ id: "t", name: "x", args: "" }], streaming: true }] };
const frozen = JSON.stringify(before);
const after = run(before, [{ type: "delta", text: "b" }, { type: "toolResult", id: "t", name: "x", ok: true, preview: "p" }]);
assert.equal(JSON.stringify(before), frozen, "prev never mutated");
assert.equal(after.blocks[0], note, "untouched block keeps its reference");
assert.notEqual(after.blocks[1], before.blocks[1]);
assert.equal(asst(after.blocks[1]).text, "a\n\nb");
assert.equal(asst(after.blocks[1]).tools[0].preview, "p");
assert.equal(applyEvent(after, { type: "step", n: 1 }), after, "ignored event = same state");
assert.equal(run(emptyTurn, [{ type: "autoCompacted", before: 1000, after: 10 }]).blocks[0].kind, "note");

// turn:done clears the cards and never leaves a silent, still-typing answer
const waiting = run({ ...emptyTurn, busy: true }, [{ type: "approval", id: 7, tool: "web_fetch", summary: "x" }]);
assert.equal(waiting.approval?.id, 7);
const ended = finishTurn(waiting);
assert.equal(ended.busy, false);
assert.equal(ended.approval, null);
assert.match(asst(ended.blocks[0]).error ?? "", /sem resposta/);
const answered = finishTurn(run({ ...emptyTurn, busy: true }, [{ type: "delta", text: "oi" }, { type: "done", stored: 9, text: "oi", promptTokens: 0, completionTokens: 0, savedTokens: 0, modelUsed: null }]));
assert.equal(asst(answered.blocks[0]).error, undefined);
assert.equal(asst(answered.blocks[0]).stored, 9);

// a stored agentic chat: tool rows become tool lines of one answer, no empty bubbles
const rows = [
  { id: 1, role: "user", content: "Liste", reasoning: null, tool_name: null },
  { id: 2, role: "assistant", content: "", reasoning: null, tool_name: null, tool_calls: [{ id: "c1", name: "list_dir", arguments: '{"path":"."}' }] },
  { id: 3, role: "tool", content: "AGENTS.md", reasoning: null, tool_name: "list_dir", tool_calls: { tool_call_id: "c1" } },
  { id: 4, role: "assistant", content: "Total: 1.", reasoning: null, tool_name: null },
  { id: 5, role: "user", content: "Liste", reasoning: null, tool_name: null },
  { id: 6, role: "assistant", content: "", reasoning: null, tool_name: null },
  { id: 7, role: "tool", content: "saída", reasoning: null, tool_name: "shell" },
  { id: 8, role: "assistant", content: "Feito.", reasoning: null, tool_name: null },
] as ChatMessage[];
const blocks = toBlocks(rows);
assert.deepEqual(blocks.map((b) => b.kind), ["user", "assistant", "user", "assistant"]);
const a1 = asst(blocks[1]);
assert.equal(a1.text, "Total: 1.");
assert.equal(a1.stored, 4);
assert.deepEqual(a1.tools, [{ id: "c1", name: "list_dir", args: '{"path":"."}', ok: true, preview: "AGENTS.md" }]);
assert.equal(asst(blocks[3]).tools[0].name, "shell", "works without tool_calls too");
assert.equal(asst(blocks[3]).stored, 8);
assert.equal(toBlocks([{ id: 1, role: "assistant", content: "", reasoning: null, tool_name: null }] as ChatMessage[]).length, 0);

// reopened chat: real arguments per call id, and failed calls stay failed
const failedRows = [
  { id: 1, role: "user", content: "Rode", reasoning: null, tool_name: null },
  { id: 2, role: "assistant", content: "", reasoning: null, tool_name: null, tool_calls: [{ id: "a", name: "shell", arguments: '{"command":"cargo test"}' }, { id: "b", name: "write_file", arguments: '{"path":"x"}' }] },
  { id: 3, role: "tool", content: "erro: exit 101\n--- stderr\nfalhou", reasoning: null, tool_name: "shell", tool_calls: { tool_call_id: "a" } },
  { id: 4, role: "tool", content: "negado pelo usuário", reasoning: null, tool_name: "write_file", tool_calls: { tool_call_id: "b" } },
  { id: 5, role: "assistant", content: "Os testes falham.", reasoning: null, tool_name: null },
] as ChatMessage[];
const ft = asst(toBlocks(failedRows)[1]).tools;
assert.deepEqual(ft.map((t) => [t.id, t.args, t.ok]), [["a", '{"command":"cargo test"}', false], ["b", '{"path":"x"}', false]]);
assert.ok(toolFailed("[cancelado]") && toolFailed("negado: fora do workspace"));
assert.ok(!toolFailed("exit 0\n--- stdout\nok") && !toolFailed("AGENTS.md"));

// the sent prompt borrows its row id by position, even when the text repeats
const live: Block[] = [...blocks, { kind: "user", text: "Liste" }];
const stored = [...rows, { id: 9, role: "user", content: "Liste", reasoning: null, tool_name: null }, { id: 10, role: "assistant", content: "ok", reasoning: null, tool_name: null }] as ChatMessage[];
const lent = borrowIds(live, stored, 10);
assert.equal((lent[4] as { id?: number }).id, 9);
assert.equal((lent[0] as { id?: number }).id, 1, "older prompts untouched");
assert.equal(borrowIds(lent, stored, 10), lent, "nothing to borrow = same list");
assert.equal((borrowIds(live, stored, null)[4] as { id?: number }).id, 9, "failed turn: newest user row");
console.log("turn ok");
