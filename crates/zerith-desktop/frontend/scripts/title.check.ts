// Self-check for conversation titles: node scripts/title.check.ts
import assert from "node:assert/strict";
import { titleOf } from "../src/lib/title.ts";

assert.equal(titleOf("Qual é a capital do Peru?"), "Qual é a capital do Peru?");
const t = titleOf(
  "Use a ferramenta shell (cmd.exe do Windows) para executar exatamente python3.99 --version",
);
assert.ok(t.endsWith("…") && t.length <= 61, t);
assert.equal(t, "Use a ferramenta shell (cmd.exe do Windows) para executar…"); // whole words
assert.equal(titleOf("![x](<C:/a.png>)"), "Imagem");
assert.equal(titleOf("linha um\nlinha dois"), "linha um linha dois");
assert.equal(titleOf("a".repeat(80)).length, 61); // no space: hard cut + …
console.log("title ok:", t);
