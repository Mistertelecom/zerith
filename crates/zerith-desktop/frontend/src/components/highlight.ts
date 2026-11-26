export interface Token {
  cls?: string;
  text: string;
}

const MAX_HIGHLIGHT_CHARS = 60_000;

type Family = "rust" | "js" | "py" | "sh" | "json" | "toml" | "yaml" | "sql" | "net" | "plain";

const LANG_MAP: Record<string, Family> = {
  rust: "rust",
  rs: "rust",
  ts: "js",
  tsx: "js",
  typescript: "js",
  js: "js",
  jsx: "js",
  javascript: "js",
  mjs: "js",
  cjs: "js",
  py: "py",
  python: "py",
  py3: "py",
  bash: "sh",
  sh: "sh",
  shell: "sh",
  zsh: "sh",
  fish: "sh",
  powershell: "sh",
  ps1: "sh",
  pwsh: "sh",
  cmd: "sh",
  bat: "sh",
  json: "json",
  jsonc: "json",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  ini: "toml",
  sql: "sql",
  sqlite: "sql",
  postgres: "sql",
  psql: "sql",
  cisco: "net",
  ios: "net",
  iosxe: "net",
  nxos: "net",
  huawei: "net",
  vrp: "net",
  juniper: "net",
  junos: "net",
  mikrotik: "net",
  routeros: "net",
  rsc: "net",
  fortinet: "net",
  fortios: "net",
  net: "net",
  network: "net",
  bgp: "net",
  ospf: "net",
  frr: "net",
  vyos: "net",
  arista: "net",
  eos: "net",
};

const RUST_KW = new Set([
  "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
  "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
  "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
  "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
]);

const RUST_TYPES = new Set([
  "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize",
  "str", "u8", "u16", "u32", "u64", "u128", "usize", "String", "Vec", "Option",
  "Result", "Box", "Arc", "Mutex", "HashMap", "BTreeMap", "Path", "PathBuf",
]);

const JS_KW = new Set([
  "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
  "debugger", "default", "delete", "do", "else", "enum", "export", "extends",
  "false", "finally", "for", "from", "function", "if", "implements", "import",
  "in", "instanceof", "interface", "let", "new", "null", "of", "package",
  "private", "protected", "public", "readonly", "return", "static", "super",
  "switch", "this", "throw", "true", "try", "type", "typeof", "undefined",
  "var", "void", "while", "with", "yield", "satisfies",
]);

const JS_TYPES = new Set([
  "any", "boolean", "bigint", "never", "number", "object", "string", "symbol",
  "unknown", "Array", "Promise", "Record", "Map", "Set", "Partial", "Required",
]);

const PY_KW = new Set([
  "False", "None", "True", "and", "as", "assert", "async", "await", "break",
  "class", "continue", "def", "del", "elif", "else", "except", "finally", "for",
  "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not",
  "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case",
]);

const PY_TYPES = new Set([
  "bool", "bytes", "dict", "float", "int", "list", "object", "set", "str", "tuple",
]);

const SH_KW = new Set([
  "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done",
  "case", "esac", "in", "function", "return", "local", "export", "readonly",
  "unset", "shift", "exit", "break", "continue", "true", "false", "sudo",
  "echo", "printf", "cd", "grep", "rg", "awk", "sed", "curl", "ssh", "docker", "git",
]);

const SQL_KW = new Set([
  "select", "from", "where", "join", "inner", "left", "right", "outer", "on",
  "as", "group", "by", "order", "having", "limit", "offset", "insert", "into",
  "values", "update", "set", "delete", "create", "table", "index", "alter",
  "drop", "if", "not", "exists", "primary", "key", "foreign", "references",
  "null", "and", "or", "in", "is", "like", "match", "distinct", "union", "all",
  "begin", "commit", "rollback", "pragma", "text", "integer", "real", "blob",
]);

const NET_KW = new Set([
  "router", "bgp", "ospf", "isis", "eigrp", "rip", "neighbor", "network",
  "interface", "ip", "ipv6", "route", "static", "acl", "access-list",
  "prefix-list", "route-map", "route-policy", "policy", "firewall", "nat",
  "bridge", "trunk", "access", "switchport", "encapsulation", "dot1q",
  "mtu", "bfd", "mpls", "ldp", "l2vpn", "evpn", "set", "delete", "edit",
  "commit", "save", "quit", "exit", "show", "display", "print", "export",
  "ping", "traceroute", "tracert", "system-view", "configure", "terminal",
  "remote-as", "address-family", "redistribute", "no", "undo", "default",
  "description", "name", "peer", "as-number", "import-route", "export-route",
  "vlan", "vrf", "ip-forwarding", "gateway", "add", "comment", "filter",
]);

const NET_OK = new Set([
  "permit", "established", "up", "active", "allow", "accept", "forward",
  "pass", "enabled", "reachable", "full",
]);

const NET_DANGER = new Set([
  "deny", "shutdown", "down", "drop", "reject", "err-disabled", "idle",
  "disabled", "blocked", "unreachable", "fail", "failed", "shut",
]);

function isIdentStart(ch: number): boolean {
  return (
    (ch >= 65 && ch <= 90) ||
    (ch >= 97 && ch <= 122) ||
    ch === 95
  );
}

function isIdentPart(ch: number): boolean {
  return isIdentStart(ch) || (ch >= 48 && ch <= 57);
}

function isNetWordPart(ch: number): boolean {
  return isIdentPart(ch) || ch === 45 || ch === 47 || ch === 58 || ch === 46;
}

function isDigit(ch: number): boolean {
  return ch >= 48 && ch <= 57;
}

function isHexDigit(ch: number): boolean {
  return isDigit(ch) || (ch >= 65 && ch <= 70) || (ch >= 97 && ch <= 102);
}

function isWhitespace(ch: number): boolean {
  return ch === 32 || ch === 9 || ch === 10 || ch === 13;
}

function pushToken(out: Token[], cls: string | undefined, text: string): void {
  if (!text) return;
  const last = out[out.length - 1];
  if (last && last.cls === cls) {
    last.text += text;
  } else {
    out.push(cls ? { cls, text } : { text });
  }
}

function scanQuoted(code: string, start: number, quote: string): number {
  let i = start + quote.length;
  const n = code.length;
  if (quote.length === 3) {
    while (i < n) {
      if (code[i] === "\\" && i + 1 < n) {
        i += 2;
        continue;
      }
      if (code.startsWith(quote, i)) {
        return i + 3;
      }
      i += 1;
    }
    return n;
  }
  const q = quote.charCodeAt(0);
  while (i < n) {
    const ch = code.charCodeAt(i);
    if (ch === 92 && i + 1 < n) {
      i += 2;
      continue;
    }
    if (ch === q || (ch === 10 && q !== 96)) {
      return ch === q ? i + 1 : i;
    }
    i += 1;
  }
  return n;
}

function scanBlockComment(code: string, start: number): number {
  const end = code.indexOf("*/", start + 2);
  return end === -1 ? code.length : end + 2;
}

function scanLineEnd(code: string, start: number): number {
  const nl = code.indexOf("\n", start);
  return nl === -1 ? code.length : nl;
}

function scanNumber(code: string, start: number): number {
  let i = start;
  const n = code.length;
  if (code[i] === "0" && i + 1 < n && (code[i + 1] === "x" || code[i + 1] === "X" || code[i + 1] === "b" || code[i + 1] === "o")) {
    i += 2;
    while (i < n && (isHexDigit(code.charCodeAt(i)) || code.charCodeAt(i) === 95)) {
      i += 1;
    }
    return i;
  }
  while (i < n && (isDigit(code.charCodeAt(i)) || code.charCodeAt(i) === 95)) {
    i += 1;
  }
  if (i + 1 < n && code[i] === "." && isDigit(code.charCodeAt(i + 1))) {
    i += 1;
    while (i < n && (isDigit(code.charCodeAt(i)) || code.charCodeAt(i) === 95)) {
      i += 1;
    }
  }
  return i;
}

function nextNonSpace(code: string, from: number): string {
  let i = from;
  while (i < code.length && (code[i] === " " || code[i] === "\t")) i += 1;
  return code[i] ?? "";
}

const IPV4_RE = /^(?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}(?:\/\d{1,2})?$/;
const IPV6_RE = /^(?:[0-9a-fA-F]{0,4}:){2,}[0-9a-fA-F]{0,4}(?:\/\d{1,3})?$/;
const IFACE_RE = /^(?:GigabitEthernet|TenGigabitEthernet|TenGigE|FastEthernet|Loopback|Eth-Trunk|Port-channel|Vlanif|Vlan|Tunnel|Null|Management|Mgmt|Bridge|ge-|xe-|et-|ae\d|sfp-sfpplus|ether\d|wlan\d|br\d|eth\d|ens\d|enp\d|vlan\d|AS\d+)/i;

function highlightCodeFamily(family: Exclude<Family, "net" | "plain">, code: string): Token[] {
  const out: Token[] = [];
  const n = code.length;
  let i = 0;

  while (i < n) {
    const ch = code.charCodeAt(i);
    const prev = i > 0 ? code[i - 1] : "\n";

    // Line comments
    if (
      (family === "rust" || family === "js" || family === "json") &&
      code[i] === "/" &&
      code[i + 1] === "/"
    ) {
      const end = scanLineEnd(code, i);
      pushToken(out, "hl-cmt", code.slice(i, end));
      i = end;
      continue;
    }
    if ((family === "py" || family === "sh" || family === "toml" || family === "yaml") && ch === 35) {
      const end = scanLineEnd(code, i);
      pushToken(out, "hl-cmt", code.slice(i, end));
      i = end;
      continue;
    }
    if (family === "sql" && code[i] === "-" && code[i + 1] === "-") {
      const end = scanLineEnd(code, i);
      pushToken(out, "hl-cmt", code.slice(i, end));
      i = end;
      continue;
    }
    // Block comments
    if (
      (family === "rust" || family === "js" || family === "json" || family === "sql") &&
      code[i] === "/" &&
      code[i + 1] === "*"
    ) {
      const end = scanBlockComment(code, i);
      pushToken(out, "hl-cmt", code.slice(i, end));
      i = end;
      continue;
    }

    // Python triple quotes
    if (family === "py" && (code.startsWith('"""', i) || code.startsWith("'''", i))) {
      const q = code.slice(i, i + 3);
      const end = scanQuoted(code, i, q);
      pushToken(out, "hl-str", code.slice(i, end));
      i = end;
      continue;
    }

    // Quoted strings (skip Rust lifetime `'a` where closing `'` doesn't precede)
    if (ch === 34 || (ch === 39 && family !== "rust") || (ch === 96 && (family === "js" || family === "sh"))) {
      const end = scanQuoted(code, i, code[i]);
      const text = code.slice(i, end);
      const isKey = (family === "json" || family === "yaml") && nextNonSpace(code, end) === ":";
      pushToken(out, isKey ? "hl-key" : "hl-str", text);
      i = end;
      continue;
    }
    if (ch === 39 && family === "rust") {
      // Distinguish `'a` lifetime from `'c'` char literal
      const look = code[i + 1] === "\\" ? i + 3 : i + 2;
      if (look < n && code[look] === "'") {
        pushToken(out, "hl-str", code.slice(i, look + 1));
        i = look + 1;
        continue;
      }
      if (i + 1 < n && isIdentStart(code.charCodeAt(i + 1))) {
        let j = i + 2;
        while (j < n && isIdentPart(code.charCodeAt(j))) j += 1;
        pushToken(out, "hl-type", code.slice(i, j));
        i = j;
        continue;
      }
    }

    // Rust attributes / TOML section headers
    if (family === "toml" && prev === "\n" && ch === 91) {
      const close = code.indexOf("]", i + 1);
      if (close !== -1 && close < scanLineEnd(code, i)) {
        const end = code[close + 1] === "]" ? close + 2 : close + 1;
        pushToken(out, "hl-kw", code.slice(i, end));
        i = end;
        continue;
      }
    }

    // Numbers
    if (isDigit(ch) && !isIdentPart(prev.charCodeAt(0))) {
      const end = scanNumber(code, i);
      pushToken(out, "hl-num", code.slice(i, end));
      i = end;
      continue;
    }

    // Identifiers & keywords
    if (isIdentStart(ch)) {
      let end = i + 1;
      while (end < n && (isIdentPart(code.charCodeAt(end)) || (family === "sh" && code[end] === "-"))) {
        end += 1;
      }
      const word = code.slice(i, end);
      const next = code[end] ?? "";
      if ((family === "toml" || family === "yaml") && next === ":" && (family === "yaml")) {
        pushToken(out, "hl-key", word);
      } else if (family === "toml" && nextNonSpace(code, end) === "=") {
        pushToken(out, "hl-key", word);
      } else if (family === "rust") {
        if (RUST_KW.has(word)) pushToken(out, "hl-kw", word);
        else if (RUST_TYPES.has(word) || (word[0] >= "A" && word[0] <= "Z")) pushToken(out, "hl-type", word);
        else if (next === "(" || (next === "!" && code[end + 1] === "(")) pushToken(out, "hl-fn", word);
        else pushToken(out, undefined, word);
      } else if (family === "js") {
        if (JS_KW.has(word)) pushToken(out, "hl-kw", word);
        else if (JS_TYPES.has(word) || (word[0] >= "A" && word[0] <= "Z")) pushToken(out, "hl-type", word);
        else if (next === "(") pushToken(out, "hl-fn", word);
        else pushToken(out, undefined, word);
      } else if (family === "py") {
        if (PY_KW.has(word)) pushToken(out, "hl-kw", word);
        else if (PY_TYPES.has(word) || (word[0] >= "A" && word[0] <= "Z")) pushToken(out, "hl-type", word);
        else if (next === "(") pushToken(out, "hl-fn", word);
        else pushToken(out, undefined, word);
      } else if (family === "sh") {
        if (SH_KW.has(word)) pushToken(out, "hl-kw", word);
        else if (prev === "$") pushToken(out, "hl-type", word);
        else pushToken(out, undefined, word);
      } else if (family === "sql") {
        if (SQL_KW.has(word.toLowerCase())) pushToken(out, "hl-kw", word);
        else if (next === "(") pushToken(out, "hl-fn", word);
        else pushToken(out, undefined, word);
      } else if (family === "json" || family === "yaml" || family === "toml") {
        if (word === "true" || word === "false" || word === "null" || word === "None") {
          pushToken(out, "hl-kw", word);
        } else {
          pushToken(out, undefined, word);
        }
      } else {
        pushToken(out, undefined, word);
      }
      i = end;
      continue;
    }

    pushToken(out, undefined, code[i]);
    i += 1;
  }

  return out;
}

function highlightNet(code: string): Token[] {
  const out: Token[] = [];
  const n = code.length;
  let i = 0;

  while (i < n) {
    const ch = code.charCodeAt(i);
    if (isWhitespace(ch)) {
      let end = i + 1;
      while (end < n && isWhitespace(code.charCodeAt(end))) end += 1;
      pushToken(out, undefined, code.slice(i, end));
      i = end;
      continue;
    }

    // `!` at line start (ignoring spaces) or `#` comments in Cisco/Huawei/MikroTik/Juniper,
    // while leaving Markdown headings (`# ...`..`###### ...` at col 0) readable.
    if (ch === 35) {
      const atLineStart = i === 0 || code[i - 1] === "\n";
      if (atLineStart) {
        let hashes = 1;
        while (i + hashes < n && code[i + hashes] === "#" && hashes <= 6) hashes += 1;
        if (hashes <= 6 && (code[i + hashes] === " " || code[i + hashes] === "\t")) {
          pushToken(out, undefined, code.slice(i, i + hashes));
          i += hashes;
          continue;
        }
      }
      const end = scanLineEnd(code, i);
      pushToken(out, "hl-cmt", code.slice(i, end));
      i = end;
      continue;
    }
    if (ch === 33) {
      let ls = i;
      while (ls > 0 && code[ls - 1] !== "\n") ls -= 1;
      if (code.slice(ls, i).trim().length === 0) {
        const end = scanLineEnd(code, i);
        pushToken(out, "hl-cmt", code.slice(i, end));
        i = end;
        continue;
      }
    }

    if (ch === 34 || ch === 39) {
      const end = scanQuoted(code, i, code[i]);
      pushToken(out, "hl-str", code.slice(i, end));
      i = end;
      continue;
    }

    if (isNetWordPart(ch)) {
      let end = i + 1;
      while (end < n && isNetWordPart(code.charCodeAt(end))) end += 1;
      const tok = code.slice(i, end);
      const low = tok.toLowerCase();

      if (IPV4_RE.test(tok) || IPV6_RE.test(tok)) {
        pushToken(out, "hl-ip", tok);
      } else if (IFACE_RE.test(tok)) {
        pushToken(out, "hl-net", tok);
      } else if (NET_OK.has(low)) {
        pushToken(out, "hl-ok", tok);
      } else if (NET_DANGER.has(low)) {
        pushToken(out, "hl-danger", tok);
      } else if (NET_KW.has(low)) {
        pushToken(out, "hl-kw", tok);
      } else if (/^\d+(?:\.\d+)?$/.test(tok)) {
        pushToken(out, "hl-num", tok);
      } else {
        pushToken(out, undefined, tok);
      }
      i = end;
      continue;
    }

    pushToken(out, undefined, code[i]);
    i += 1;
  }

  return out;
}

export function highlightCode(lang: string, code: string): Token[] {
  if (!code || code.length > MAX_HIGHLIGHT_CHARS) {
    return [{ text: code }];
  }
  const normalized = lang.trim().toLowerCase();
  const family: Family = LANG_MAP[normalized] ?? "plain";
  if (family === "plain") {
    return [{ text: code }];
  }
  if (family === "net") {
    return highlightNet(code);
  }
  return highlightCodeFamily(family, code);
}
