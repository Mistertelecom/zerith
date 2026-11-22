// Self-check for the transcript's markdown parser. Node 22.18+/24 runs it as is:
//   node scripts/markdown.check.ts
import assert from "node:assert/strict";
import { parseBlocks, parseInline } from "../src/components/markdown.ts";

const reply = [
  "Vou ler o cliente.",
  "",
  "## O que muda",
  "",
  "1. `openai.rs` usa **http**",
  "2. Segundo",
  "   - aninhado",
  "",
  "```rust",
  "let a = 1;",
  "```",
  "",
  "> citação *leve*",
  "",
  "---",
  "Veja [docs](https://x.dev) e read_file_name.",
].join("\n");

const b = parseBlocks(reply);
assert.deepEqual(
  b.map((x) => x.t),
  ["p", "h", "list", "code", "quote", "hr", "p"],
);
assert.deepEqual(b[1], { t: "h", level: 2, c: ["O que muda"] });
const list = b[2] as Extract<(typeof b)[number], { t: "list" }>;
assert.equal(list.ordered, true);
assert.equal(list.items.length, 2);
assert.deepEqual(list.items[0][0], { t: "p", c: [{ t: "code", v: "openai.rs" }, " usa ", { t: "b", c: ["http"] }] });
assert.equal(list.items[1][1].t, "list", "indented item nests");
assert.deepEqual(b[3], { t: "code", lang: "rust", text: "let a = 1;" });

// links: safe ones become anchors, javascript: stays text; snake_case is not italic
assert.deepEqual(parseInline("[a](https://x.dev)"), [{ t: "a", href: "https://x.dev", c: ["a"] }]);
assert.deepEqual(parseInline("[a](javascript:alert(1))"), ["[a](javascript:alert(1)", ")"]);
assert.deepEqual(parseInline("read_file_name"), ["read_file_name"]);
assert.deepEqual(parseInline("*a* e _b_"), [{ t: "i", c: ["a"] }, " e ", { t: "i", c: ["b"] }]);

// a fence still streaming runs to the end instead of swallowing nothing
assert.deepEqual(parseBlocks("```ts\nconst x"), [{ t: "code", lang: "ts", text: "const x" }]);
// loose list: blank lines between items keep one list
assert.equal(parseBlocks("- a\n\n- b").length, 1);

// tables: header, alignment, ragged rows padded to the header's width
const t = parseBlocks("Antes\n| a | b |\n|:--|--:|\n| 1 | **2** |\n| 3 |\n\nDepois");
assert.deepEqual(t.map((x) => x.t), ["p", "table", "p"]);
const table = t[1] as Extract<(typeof t)[number], { t: "table" }>;
assert.deepEqual(table.align, ["left", "right"]);
assert.deepEqual(table.rows[0][1], [{ t: "b", c: ["2"] }]);
assert.deepEqual(table.rows[1][1], []);
// images: web, data and absolute local paths; anything else stays text
assert.deepEqual(parseInline("![gato](https://x.dev/a.png)"), [{ t: "img", src: "https://x.dev/a.png", alt: "gato" }]);
assert.deepEqual(parseInline("![](C:\\img\\a.png)"), [{ t: "img", src: "C:\\img\\a.png", alt: "" }]);
assert.deepEqual(parseInline("![x](<C:/Meus Prints/a b.png>)"), [{ t: "img", src: "C:/Meus Prints/a b.png", alt: "x" }]);
assert.deepEqual(parseInline("![x](javascript:alert(1))"), ["![x](javascript:alert(1)", ")"]);
assert.deepEqual(parseInline("![x](//evil.local/share/a.png)"), ["![x](//evil.local/share/a.png)"]);
assert.deepEqual(parseInline("![x](/\\evil.local/share/a.png)"), ["![x](/\\evil.local/share/a.png)"]);
assert.deepEqual(parseInline("[a](https://x.dev)")[0], { t: "a", href: "https://x.dev", c: ["a"] }, "links still work");
assert.deepEqual(parseInline("[https://example.com](https://example.com)"), [
  { t: "a", href: "https://example.com", c: ["https://example.com"] },
]);
assert.deepEqual(parseInline("`` `code` ``"), [{ t: "code", v: " `code` " }]);
assert.deepEqual(parseInline("``a `b` c``"), [{ t: "code", v: "a `b` c" }]);

console.log("markdown ok");
