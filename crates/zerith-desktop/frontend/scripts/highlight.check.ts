// Self-check for the syntax & network config highlighter. Node 22.18+/24 runs it as is:
//   node scripts/highlight.check.ts
import assert from "node:assert/strict";
import { highlightCode, type Token } from "../src/components/highlight.ts";

function roundTrip(lang: string, code: string): Token[] {
  const tokens = highlightCode(lang, code);
  assert.equal(
    tokens.map((t) => t.text).join(""),
    code,
    `lossless round-trip failed for lang=${lang}`,
  );
  return tokens;
}

function hasToken(tokens: Token[], text: string, cls?: string): boolean {
  return tokens.some((t) => t.text === text && t.cls === cls);
}

// 1. Rust: keywords, lifetimes, types, strings, comments, numbers, functions
{
  const code = `// comentario\nfn parse<'a>(n: usize) -> Option<&'a str> { let ok = helper("val"); Some(ok) }`;
  const tok = roundTrip("rust", code);
  assert(hasToken(tok, "// comentario", "hl-cmt"));
  assert(hasToken(tok, "fn", "hl-kw"));
  assert(hasToken(tok, "'a", "hl-type"));
  assert(hasToken(tok, "usize", "hl-type"));
  assert(hasToken(tok, "Option", "hl-type"));
  assert(hasToken(tok, "helper", "hl-fn"));
  assert(hasToken(tok, `"val"`, "hl-str"));
}

// 2. TypeScript / JavaScript
{
  const code = `/* blk */\nconst count: number = 42;\nasync function run(): Promise<string> { return "ok"; }`;
  const tok = roundTrip("ts", code);
  assert(hasToken(tok, "/* blk */", "hl-cmt"));
  assert(hasToken(tok, "const", "hl-kw"));
  assert(hasToken(tok, "number", "hl-type"));
  assert(hasToken(tok, "42", "hl-num"));
  assert(hasToken(tok, "run", "hl-fn"));
  assert(hasToken(tok, "Promise", "hl-type"));
  assert(hasToken(tok, `"ok"`, "hl-str"));
}

// 3. Python: triple-quoted strings, keywords, types, comments
{
  const code = `# note\ndef check(x: int) -> bool:\n    doc = """hello"""\n    return True`;
  const tok = roundTrip("python", code);
  assert(hasToken(tok, "# note", "hl-cmt"));
  assert(hasToken(tok, "def", "hl-kw"));
  assert(hasToken(tok, "int", "hl-type"));
  assert(hasToken(tok, `"""hello"""`, "hl-str"));
  assert(hasToken(tok, "True", "hl-kw"));
}

// 4. JSON: keys, strings, numbers, booleans/null
{
  const code = `{"host": "10.0.0.1", "port": 443, "ok": true, "err": null}`;
  const tok = roundTrip("json", code);
  assert(hasToken(tok, `"host"`, "hl-key"));
  assert(hasToken(tok, `"10.0.0.1"`, "hl-str"));
  assert(hasToken(tok, "443", "hl-num"));
  assert(hasToken(tok, "true", "hl-kw"));
  assert(hasToken(tok, "null", "hl-kw"));
}

// 5. SQL: case-insensitive keywords, `--` comment, string
{
  const code = `-- busca\nSELECT id, count(*) FROM usage_records WHERE status = 'ok' LIMIT 10;`;
  const tok = roundTrip("sql", code);
  assert(hasToken(tok, "-- busca", "hl-cmt"));
  assert(hasToken(tok, "SELECT", "hl-kw"));
  assert(hasToken(tok, "FROM", "hl-kw"));
  assert(hasToken(tok, "'ok'", "hl-str"));
  assert(hasToken(tok, "10", "hl-num"));
}

// 6. Cisco IOS / Huawei VRP network configs: IPs, interfaces, status words, comments
{
  const code = [
    "! BGP core peer",
    "router bgp 65001",
    " neighbor 192.0.2.10 remote-as 65002",
    "interface GigabitEthernet0/0/1",
    " # link check",
    " description up",
    " shutdown",
    " permit ip 10.10.0.0/16",
  ].join("\n");
  const tok = roundTrip("cisco", code);
  assert(hasToken(tok, "! BGP core peer", "hl-cmt"));
  assert(hasToken(tok, "# link check", "hl-cmt"));
  assert(hasToken(tok, "router", "hl-kw"));
  assert(hasToken(tok, "bgp", "hl-kw"));
  assert(hasToken(tok, "192.0.2.10", "hl-ip"));
  assert(hasToken(tok, "GigabitEthernet0/0/1", "hl-net"));
  assert(hasToken(tok, "up", "hl-ok"));
  assert(hasToken(tok, "permit", "hl-ok"));
  assert(hasToken(tok, "shutdown", "hl-danger"));
  assert(hasToken(tok, "65001", "hl-num"));
}

// 7. Guard against huge code blocks (> 60_000 chars): returns single plain token
{
  const huge = "x".repeat(60_001);
  const tok = highlightCode("rust", huge);
  assert.deepEqual(tok, [{ text: huge }]);
}

console.log("highlight.check.ts ok");
