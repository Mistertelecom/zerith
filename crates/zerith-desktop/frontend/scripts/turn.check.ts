// node scripts/turn.check.ts — text after a tool call is a new paragraph.
import assert from "node:assert";
import { applyEvent, type TurnState } from "../src/lib/turn.ts";

let s = { blocks: [] } as unknown as TurnState;
for (const ev of [
  { type: "delta", text: "Vou gerar." },
  { type: "toolCall", id: "1", name: "generate_video", args: "{}" },
  { type: "delta", text: "Pronto!" },
  { type: "delta", text: " Feito." },
]) s = applyEvent(s, ev as never);
const a = (s as unknown as { blocks: { text: string }[] }).blocks[0];
assert.equal(a.text, "Vou gerar.\n\nPronto! Feito.");
console.log("turn ok");
