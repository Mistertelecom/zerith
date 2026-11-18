// The markdown subset models actually write, parsed to plain data (never to
// HTML strings); Transcript.tsx turns it into React elements, so nothing the
// model says is ever injected as markup. Self-check: scripts/markdown.check.ts.
// ponytail: no setext headings, no backslash escapes, no `\|` inside table
// cells; add them when a real answer shows up broken without them.

export type Inline =
  | string
  | { t: "code"; v: string }
  | { t: "b" | "i" | "s"; c: Inline[] }
  | { t: "a"; href: string; c: Inline[] }
  | { t: "img"; src: string; alt: string };

export type Align = "left" | "center" | "right" | null;

export type MdBlock =
  | { t: "h"; level: number; c: Inline[] }
  | { t: "p"; c: Inline[] }
  | { t: "list"; ordered: boolean; start: number; items: MdBlock[][] }
  | { t: "code"; lang: string; text: string }
  | { t: "quote"; c: MdBlock[] }
  | { t: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: "hr" };

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)/;
const HEAD = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const RULE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}>/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const SAFE_HREF = /^(https?:|mailto:)/i;
// Web, inline data, or an absolute local path (C:\x, C:/x, /x, file:///x).
const SAFE_SRC = /^(https?:\/\/|data:image\/|file:\/\/\/|[a-z]:[\\/]|\/)/i;
const TABLE_RULE = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

const indentOf = (line: string) => line.length - line.trimStart().length;
const startsBlock = (line: string) =>
  FENCE.test(line) || HEAD.test(line) || RULE.test(line) || QUOTE.test(line) || ITEM.test(line);
/** A header row followed by a `|---|:--:|` rule starts a table. */
const startsTable = (lines: string[], i: number) =>
  lines[i].includes("|") && i + 1 < lines.length && lines[i + 1].includes("|") && TABLE_RULE.test(lines[i + 1]);
const cells = (row: string) =>
  row
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim());

export function parseBlocks(src: string): MdBlock[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: MdBlock[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence) {
      const mark = fence[1];
      const closes = (l: string) => {
        const t = l.trim();
        return t.length >= mark.length && t[0] === mark[0] && /^(`+|~+)$/.test(t);
      };
      const body: string[] = [];
      i++;
      while (i < lines.length && !closes(lines[i])) body.push(lines[i++]);
      i++; // the closing fence; an unclosed one (still streaming) runs to the end
      out.push({ t: "code", lang: fence[2], text: body.join("\n") });
      continue;
    }

    const head = HEAD.exec(line);
    if (head) {
      out.push({ t: "h", level: head[1].length, c: parseInline(head[2]) });
      i++;
      continue;
    }

    if (startsTable(lines, i)) {
      const head = cells(line);
      const align: Align[] = cells(lines[i + 1]).map((c) =>
        c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : null,
      );
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) {
        const r = cells(lines[i++]);
        // Short rows are padded and long ones cut, so every row has the header's columns.
        rows.push(head.map((_, k) => parseInline(r[k] ?? "")));
      }
      out.push({ t: "table", align: head.map((_, k) => align[k] ?? null), head: head.map(parseInline), rows });
      continue;
    }

    if (RULE.test(line)) {
      out.push({ t: "hr" });
      i++;
      continue;
    }

    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i])) body.push(lines[i++].replace(/^ {0,3}> ?/, ""));
      out.push({ t: "quote", c: parseBlocks(body.join("\n")) });
      continue;
    }

    const first = ITEM.exec(line);
    if (first) {
      const indent = first[1].length;
      const ordered = /\d/.test(first[2]);
      const sibling = (l: string | undefined) => {
        const m = l === undefined ? null : ITEM.exec(l);
        return !!m && m[1].length - indent <= 1 && m[1].length >= indent && /\d/.test(m[2]) === ordered;
      };
      const items: MdBlock[][] = [];
      while (i < lines.length && sibling(lines[i])) {
        const m = ITEM.exec(lines[i])!;
        const pad = m[1].length + m[2].length + 1;
        const body = [m[3]];
        i++;
        // Everything indented past the marker belongs to the item (nested
        // lists, code); a blank line ends it unless indented text follows.
        while (i < lines.length) {
          const next = lines[i];
          if (!next.trim()) {
            let j = i + 1;
            while (j < lines.length && !lines[j].trim()) j++;
            if (j < lines.length && indentOf(lines[j]) > indent && !sibling(lines[j])) {
              body.push("");
              i++;
              continue;
            }
            break;
          }
          if (indentOf(next) > indent && !sibling(next)) {
            body.push(next.slice(Math.min(pad, indentOf(next))));
          } else if (startsBlock(next)) {
            break;
          } else {
            body.push(next.trim()); // lazy continuation of the item's paragraph
          }
          i++;
        }
        items.push(parseBlocks(body.join("\n")));
        // Blank lines between two items of the same list do not end it.
        let j = i;
        while (j < lines.length && !lines[j].trim()) j++;
        if (j > i && sibling(lines[j])) i = j;
      }
      out.push({ t: "list", ordered, start: ordered ? parseInt(first[2], 10) : 1, items });
      continue;
    }

    const para = [line.trim()];
    i++;
    while (i < lines.length && lines[i].trim() && !startsBlock(lines[i]) && !startsTable(lines, i)) {
      para.push(lines[i++].trim());
    }
    out.push({ t: "p", c: parseInline(para.join("\n")) });
  }
  return out;
}

// One alternation, leftmost match wins: `code`, **bold**, ~~strike~~, *em*,
// _em_ (not inside snake_case), [label](url), bare http(s) URL, ![alt](src).
// The image is last in the regex but its leading `!` makes it match before
// the link alternative could start one character later.
const INLINE =
  /`([^`\n]+)`|\*\*(.+?)\*\*|~~(.+?)~~|\*(?![\s*])(.+?)(?<![\s*])\*|(?<![\w])_(?![\s_])(.+?)(?<![\s_])_(?![\w])|(?<!!)\[([^\]\n]+)\]\(([^)\s]+)(?:\s+"[^"]*")?\)|(https?:\/\/[^\s<>()[\]`]*[^\s<>()[\]`.,;:!?'"*_])|!\[([^\]\n]*)\]\(<?([^)>\n]+?)>?(?:\s+"[^"]*")?\)/g;

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  for (const m of src.matchAll(INLINE)) {
    const at = m.index ?? 0;
    if (at > last) out.push(src.slice(last, at));
    last = at + m[0].length;
    if (m[1] !== undefined) out.push({ t: "code", v: m[1] });
    else if (m[2] !== undefined) out.push({ t: "b", c: parseInline(m[2]) });
    else if (m[3] !== undefined) out.push({ t: "s", c: parseInline(m[3]) });
    else if (m[4] !== undefined || m[5] !== undefined) out.push({ t: "i", c: parseInline((m[4] ?? m[5])!) });
    else if (m[6] !== undefined) {
      // A javascript: or relative link stays text; only web and mail open.
      out.push(SAFE_HREF.test(m[7]) ? { t: "a", href: m[7], c: parseInline(m[6]) } : m[0]);
    } else if (m[8] !== undefined) out.push({ t: "a", href: m[8], c: [m[8]] });
    else out.push(SAFE_SRC.test(m[10]) ? { t: "img", src: m[10].trim(), alt: m[9] } : m[0]);
  }
  if (last < src.length) out.push(src.slice(last));
  return out;
}
