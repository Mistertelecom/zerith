//! A página que `web_fetch` devolve: HTML virado markdown simples (títulos,
//! listas, tabelas, links, blocos de código) sem a moldura do site (menu,
//! cabeçalho, rodapé, barra lateral, scripts), e, numa página grande com
//! `prompt`, só os trechos que casam com o pedido.
//!
//! Ideia do WebFetch do Claude Code (markdown + extrato) e do
//! `web_tools_extract.py` do Hermes (MIT). Sem parser HTML: um leitor de tags
//! que aguenta HTML real o bastante para um modelo ler, não para renderizar.

use std::collections::HashMap;
use std::path::Path;

use zerith_skills::search::tokens;

/// Conteúdo é texto cru: pulado sem interpretar (um `<nav>` dentro de um
/// script não abre nada).
const RAW_SKIP: &[&str] = &[
    "script", "style", "noscript", "textarea", "title", "template",
];
/// A moldura do site. `header`/`footer` dentro de `article`/`main` ficam:
/// ali costumam trazer o título e a data do texto.
const CHROME: &[&str] = &[
    "nav", "aside", "header", "footer", "svg", "iframe", "button", "select",
];
const VOID: &[&str] = &[
    "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "source", "track",
    "wbr", "param",
];

/// HTML → markdown. `base` é a URL da página, para os links relativos.
pub fn html_to_markdown(html: &str, base: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let b = html.as_bytes();
    let len = b.len();
    let mut md = Md {
        base: base.to_string(),
        ..Md::default()
    };
    let mut i = 0;
    while i < len {
        if b[i] != b'<' {
            let next = html[i..].find('<').map_or(len, |p| i + p);
            md.text(&html[i..next]);
            i = next;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(len, |p| i + p + 3);
            continue;
        }
        if matches!(b.get(i + 1), Some(b'!' | b'?')) {
            i = html[i..].find('>').map_or(len, |p| i + p + 1);
            continue;
        }
        let closing = b.get(i + 1) == Some(&b'/');
        let start = i + 1 + usize::from(closing);
        if !b.get(start).is_some_and(u8::is_ascii_alphabetic) {
            md.text("<");
            i += 1;
            continue;
        }
        let end = html[i..].find('>').map_or(len, |p| i + p);
        let inner = &html[start..end];
        let name_len = inner
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(inner.len());
        let name = &lower[start..start + name_len];
        let void = inner.ends_with('/') || VOID.contains(&name);
        i = (end + 1).min(len);
        if closing {
            md.close(name);
        } else if RAW_SKIP.contains(&name) && !void {
            let close = format!("</{name}");
            i = lower[i..].find(&close).map_or(len, |p| {
                let from = i + p;
                lower[from..].find('>').map_or(len, |q| from + q + 1)
            });
        } else {
            md.open(name, inner, void);
        }
    }
    md.finish()
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    cell: Option<String>,
}

#[derive(Default)]
struct Md {
    out: String,
    base: String,
    /// `None`: lista com marcador; `Some(n)`: numerada, próximo número.
    lists: Vec<Option<usize>>,
    pre: usize,
    /// href e onde o texto do link começa no buffer atual.
    links: Vec<(String, usize)>,
    table: Option<Table>,
    table_depth: usize,
    /// Dentro de `article`/`main`.
    content: usize,
    /// Pulando a moldura: o nome e quantos abertos.
    skip: Option<(String, usize)>,
}

impl Md {
    fn in_cell(&self) -> bool {
        self.table.as_ref().is_some_and(|t| t.cell.is_some())
    }

    /// Onde o texto vai: a célula aberta ou o documento.
    fn buf(&mut self) -> &mut String {
        match &mut self.table {
            Some(Table { cell: Some(c), .. }) => c,
            _ => &mut self.out,
        }
    }

    /// Garante `n` quebras de linha no fim (numa célula, um espaço).
    fn brk(&mut self, n: usize) {
        if self.in_cell() {
            let b = self.buf();
            if !b.is_empty() && !b.ends_with(char::is_whitespace) {
                b.push(' ');
            }
            return;
        }
        let trimmed = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(trimmed);
        if self.out.is_empty() {
            return;
        }
        let have = self.out.len() - self.out.trim_end_matches('\n').len();
        for _ in have..n {
            self.out.push('\n');
        }
    }

    fn text(&mut self, raw: &str) {
        if self.skip.is_some() || self.table.as_ref().is_some_and(|t| t.cell.is_none()) {
            return;
        }
        let t = decode(raw);
        if self.pre > 0 {
            self.buf().push_str(&t);
            return;
        }
        let lead = t.starts_with(char::is_whitespace);
        let trail = t.ends_with(char::is_whitespace);
        let core = t.split_whitespace().collect::<Vec<_>>().join(" ");
        let b = self.buf();
        let room = !b.is_empty() && !b.ends_with(char::is_whitespace);
        if (lead || core.is_empty() && trail) && room {
            b.push(' ');
        }
        if !core.is_empty() {
            b.push_str(&core);
            if trail {
                b.push(' ');
            }
        }
    }

    fn open(&mut self, name: &str, tag: &str, void: bool) {
        if let Some((n, depth)) = &mut self.skip {
            if n == name && !void {
                *depth += 1;
            }
            return;
        }
        let framed = matches!(name, "header" | "footer") && self.content > 0;
        if CHROME.contains(&name) && !framed {
            if !void {
                self.skip = Some((name.to_string(), 1));
            }
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.brk(2);
                if !self.in_cell() {
                    let level = usize::from(name.as_bytes()[1] - b'0');
                    self.out.push_str(&"#".repeat(level));
                    self.out.push(' ');
                }
            }
            "article" | "main" => {
                self.content += 1;
                self.brk(2);
            }
            "p" | "blockquote" | "section" | "figure" | "dl" | "details" => self.brk(2),
            "div" | "dt" | "dd" | "figcaption" | "address" | "summary" | "br" => self.brk(1),
            "hr" => {
                self.brk(2);
                if !self.in_cell() {
                    self.out.push_str("---");
                    self.brk(2);
                }
            }
            "ul" | "ol" => {
                self.brk(if self.lists.is_empty() { 2 } else { 1 });
                let start = attr(tag, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                self.lists.push((name == "ol").then_some(start));
            }
            "li" => {
                self.brk(1);
                if !self.in_cell() {
                    let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                    let marker = match self.lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            format!("{}. ", *n - 1)
                        }
                        _ => "- ".to_string(),
                    };
                    self.out.push_str(&indent);
                    self.out.push_str(&marker);
                }
            }
            "pre" => {
                self.brk(2);
                if !self.in_cell() && self.pre == 0 {
                    self.out.push_str("```\n");
                }
                self.pre += 1;
            }
            "code" if self.pre == 0 => self.buf().push('`'),
            "a" => {
                let href = attr(tag, "href").unwrap_or_default();
                let at = self.buf().len();
                self.links.push((href, at));
            }
            "table" => {
                if self.table.is_none() {
                    self.brk(2);
                    self.table = Some(Table::default());
                }
                self.table_depth += 1;
            }
            "tr" | "td" | "th" if self.table_depth > 1 => self.brk(1),
            "tr" => {
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            "td" | "th" => {
                if let Some(t) = &mut self.table {
                    if t.rows.is_empty() {
                        t.rows.push(Vec::new());
                    }
                    t.cell = Some(String::new());
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        if let Some((n, depth)) = &mut self.skip {
            if n == name {
                *depth -= 1;
                if *depth == 0 {
                    self.skip = None;
                }
            }
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "blockquote" | "section" | "figure"
            | "dl" | "details" => self.brk(2),
            "article" | "main" => {
                self.content = self.content.saturating_sub(1);
                self.brk(2);
            }
            "div" | "dt" | "dd" | "figcaption" | "address" | "summary" | "li" => self.brk(1),
            "ul" | "ol" => {
                self.lists.pop();
                self.brk(if self.lists.is_empty() { 2 } else { 1 });
            }
            "pre" if self.pre > 0 => {
                self.pre -= 1;
                if self.pre == 0 && !self.in_cell() {
                    if !self.out.ends_with('\n') {
                        self.out.push('\n');
                    }
                    self.out.push_str("```");
                    self.brk(2);
                }
            }
            "code" if self.pre == 0 => self.buf().push('`'),
            "a" => self.close_link(),
            "td" | "th" if self.table_depth == 1 => {
                if let Some(t) = &mut self.table {
                    if let Some(c) = t.cell.take() {
                        let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
                        if let Some(row) = t.rows.last_mut() {
                            row.push(c.replace('|', "\\|"));
                        }
                    }
                }
            }
            "table" if self.table_depth > 0 => {
                self.table_depth -= 1;
                if self.table_depth == 0 {
                    if let Some(t) = self.table.take() {
                        self.render(t);
                    }
                }
            }
            _ => {}
        }
    }

    fn close_link(&mut self) {
        let Some((href, at)) = self.links.pop() else {
            return;
        };
        let url = resolve(&href, &self.base);
        let b = self.buf();
        if at > b.len() || !b.is_char_boundary(at) {
            return;
        }
        let text = b[at..].split_whitespace().collect::<Vec<_>>().join(" ");
        let (Some(url), false) = (url, text.is_empty()) else {
            return;
        };
        let spaced = b[at..].starts_with(char::is_whitespace);
        b.truncate(at);
        if spaced && !b.is_empty() && !b.ends_with(char::is_whitespace) {
            b.push(' ');
        }
        b.push_str(&format!("[{text}]({url})"));
    }

    fn render(&mut self, t: Table) {
        let rows: Vec<Vec<String>> = t
            .rows
            .into_iter()
            .filter(|r| r.iter().any(|c| !c.is_empty()))
            .collect();
        let Some(cols) = rows.iter().map(Vec::len).max() else {
            return;
        };
        self.brk(2);
        for (n, row) in rows.iter().enumerate() {
            let mut cells = row.clone();
            cells.resize(cols, String::new());
            self.out.push_str(&format!("| {} |\n", cells.join(" | ")));
            if n == 0 {
                self.out.push_str(&format!("|{}\n", " --- |".repeat(cols)));
            }
        }
        self.brk(2);
    }

    /// Fim de linha limpo, no máximo uma linha em branco seguida (fora de
    /// bloco de código), sem item de lista vazio.
    fn finish(self) -> String {
        let mut lines: Vec<&str> = Vec::new();
        let mut fence = false;
        for line in self.out.lines() {
            let l = line.trim_end();
            if l.starts_with("```") {
                fence = !fence;
            }
            if !fence
                && (l.trim() == "-" || l.is_empty() && lines.last().is_none_or(|p| p.is_empty()))
            {
                continue;
            }
            lines.push(l);
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}

/// O valor de um atributo no texto da tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        if at == 0 || !lower.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let Some(rest) = tag[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode(value));
    }
    None
}

/// Link absoluto a partir da URL da página. Âncora e `javascript:` não viram
/// link. ponytail: `../` fica como está; normalizar se um modelo tropeçar.
fn resolve(href: &str, base: &str) -> Option<String> {
    let h = href.trim();
    let lower = h.to_ascii_lowercase();
    if h.is_empty() || h.starts_with('#') || lower.starts_with("javascript:") {
        return None;
    }
    if h.contains("://") || lower.starts_with("mailto:") || base.is_empty() {
        return Some(h.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    if let Some(net) = h.strip_prefix("//") {
        return Some(format!("{scheme}://{net}"));
    }
    if h.starts_with('/') {
        return Some(format!("{origin}{h}"));
    }
    let path = rest[host_end..].split(['?', '#']).next().unwrap_or("");
    let dir = &path[..path.rfind('/').map_or(0, |p| p + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    Some(format!("{origin}{dir}{h}"))
}

/// Entidades HTML: numéricas e as nomeadas comuns.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let semi = rest[1..].find(';').filter(|&q| q <= 10).map(|q| q + 1);
        let named = semi.and_then(|q| {
            let ent = &rest[1..q];
            let c = match ent {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                "copy" => '©',
                "reg" => '®',
                "mdash" => '—',
                "ndash" => '–',
                "hellip" => '…',
                "laquo" => '«',
                "raquo" => '»',
                "middot" => '·',
                "bull" => '•',
                "lsquo" | "rsquo" => '\'',
                "ldquo" | "rdquo" => '"',
                _ => {
                    let n = ent.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, q + 1))
        });
        match named {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// --- extrato ----------------------------------------------------------------

/// Quanto de trecho volta ao modelo numa página grande.
const EXCERPT_BUDGET: usize = 12_000;
/// Linhas do índice de títulos.
const INDEX_LINES: usize = 60;
const K1: f64 = 1.2;
const B: f64 = 0.75;
/// Fração da melhor pontuação que um bloco precisa para entrar.
const MIN_SHARE: f64 = 0.25;
/// Palavras que aparecem em todo trecho e só somam ruído.
const STOP: &[&str] = &[
    "de", "do", "da", "dos", "das", "em", "no", "na", "os", "as", "um", "uma", "que", "como",
    "para", "com", "por", "the", "of", "to", "and", "in", "is", "for", "on",
];

struct Chunk {
    /// Os títulos acima dele: `BGP › Comandos de BGP`.
    heading: String,
    text: String,
}

/// O índice de títulos (até h3) e os blocos da página. Tabela e bloco de
/// código ficam inteiros: um bloco termina numa linha em branco fora deles.
fn split(md: &str) -> (Vec<String>, Vec<Chunk>) {
    let mut index = Vec::new();
    let mut chunks = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut cur = String::new();
    let mut fence = false;
    let flush = |cur: &mut String, path: &[(usize, String)], chunks: &mut Vec<Chunk>| {
        if !cur.trim().is_empty() {
            let heading = path
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(" › ");
            chunks.push(Chunk {
                heading,
                text: std::mem::take(cur),
            });
        }
        cur.clear();
    };
    for line in md.lines() {
        let level = line.bytes().take_while(|&c| c == b'#').count();
        if !fence && (1..=6).contains(&level) && line[level..].starts_with(' ') {
            flush(&mut cur, &path, &mut chunks);
            let title = line[level..].trim().to_string();
            path.retain(|(l, _)| *l < level);
            if level <= 3 {
                index.push(format!("{}- {title}", "  ".repeat(level - 1)));
            }
            path.push((level, title));
            continue;
        }
        if line.starts_with("```") {
            fence = !fence;
        }
        if line.trim().is_empty() && !fence {
            flush(&mut cur, &path, &mut chunks);
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    flush(&mut cur, &path, &mut chunks);
    (index, chunks)
}

/// Termos de busca: a palavra e, se longa, o radical de 5 letras (`~prefi`),
/// para "prefixo" achar "prefixos".
fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in tokens(text) {
        if STOP.contains(&t.as_str()) {
            continue;
        }
        if t.chars().count() > 5 {
            out.push(format!("~{}", t.chars().take(5).collect::<String>()));
        }
        out.push(t);
    }
    out
}

/// BM25 de cada bloco contra o pedido; o título do bloco conta como texto.
fn scores(chunks: &[Chunk], query: &str) -> Vec<f64> {
    let docs: Vec<Vec<String>> = chunks
        .iter()
        .map(|c| terms(&format!("{} {}", c.heading, c.text)))
        .collect();
    let n = docs.len() as f64;
    let avg = docs.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let mut df: HashMap<&str, usize> = HashMap::new();
    for d in &docs {
        let mut seen: Vec<&str> = d.iter().map(String::as_str).collect();
        seen.sort_unstable();
        seen.dedup();
        for t in seen {
            *df.entry(t).or_default() += 1;
        }
    }
    let mut q = terms(query);
    q.sort();
    q.dedup();
    docs.iter()
        .map(|d| {
            let len = d.len() as f64;
            q.iter()
                .map(|t| {
                    let tf = d.iter().filter(|x| *x == t).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = df.get(t.as_str()).copied().unwrap_or(0) as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    let weight = if t.starts_with('~') { 0.5 } else { 1.0 };
                    weight * idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg.max(1.0)))
                })
                .sum()
        })
        .collect()
}

/// A página grande reduzida ao que o pedido procura: o índice de títulos, os
/// blocos que mais casam (na ordem da página) e onde está a página inteira.
pub fn extract(md: &str, prompt: &str, full: Option<&Path>) -> String {
    let (index, chunks) = split(md);
    let score = scores(&chunks, prompt);
    // Um bloco que só casa uma palavra solta ("limites") não enche o espaço
    // dos que casam o pedido.
    let best = score.iter().copied().fold(0.0, f64::max);
    let mut ranked: Vec<usize> = (0..chunks.len())
        .filter(|&i| score[i] > 0.0 && score[i] >= best * MIN_SHARE)
        .collect();
    ranked.sort_by(|&a, &b| score[b].total_cmp(&score[a]));
    let mut picked = Vec::new();
    let mut used = 0;
    for i in ranked {
        let cost = chunks[i].text.len() + chunks[i].heading.len() + 8;
        if used + cost <= EXCERPT_BUDGET {
            picked.push(i);
            used += cost;
        }
    }
    picked.sort_unstable();

    let mut out = format!(
        "[página com {} caracteres: abaixo o índice e só os trechos que casam com o pedido]\n",
        md.chars().count()
    );
    out.push_str(&match full {
        Some(p) => format!(
            "[página completa: {} (leia com read_file ou grep)]\n",
            p.display()
        ),
        None => "[página completa: não foi possível gravar]\n".into(),
    });
    out.push_str("\n## Índice da página\n");
    // Índice comprido: só os dois primeiros níveis.
    let index: Vec<&String> = if index.len() > INDEX_LINES {
        index.iter().filter(|l| !l.starts_with("    ")).collect()
    } else {
        index.iter().collect()
    };
    for line in index.iter().take(INDEX_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    if index.len() > INDEX_LINES {
        out.push_str(&format!("[+{} títulos]\n", index.len() - INDEX_LINES));
    }
    out.push_str("\n## Trechos\n");
    if picked.is_empty() {
        out.push_str("nenhum trecho casou com o pedido; procure na página completa.\n");
    }
    let mut last = None;
    for i in picked {
        let c = &chunks[i];
        if last != Some(&c.heading) && !c.heading.is_empty() {
            out.push_str(&format!("\n### {}\n", c.heading));
        }
        last = Some(&c.heading);
        out.push_str(&c.text);
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANUAL: &str = include_str!("../../tests/fixtures/manual.html");

    #[test]
    fn a_page_becomes_markdown_without_the_site_frame() {
        let md = html_to_markdown(MANUAL, "https://docs.example.com/xr9000/manual.html");
        assert!(
            md.starts_with("# Manual de referência do roteador XR-9000"),
            "{}",
            &md[..200]
        );
        assert!(md.contains("\n## BGP\n"));
        assert!(md.contains("| Comando | Função | Modo | Exemplo |\n| --- | --- | --- | --- |\n"));
        assert!(
            md.contains("| `peer <ip> route-limit <n> <pct>` | maximum-prefix: limita os prefixos")
        );
        assert!(
            md.contains("```\nsystem-view\n bgp 65000\n"),
            "bloco de código"
        );
        assert!(md.contains("- Documente cada mudança de bgp"));
        assert!(md.contains("  1. salve a configuração\n  2. anote o estado atual"));
        assert!(md
            .contains("[nota de versão](https://docs.example.com/docs/xr9000/release-notes.html)"));
        assert!(md.contains("[portal de suporte](https://example.com/suporte)"));
        assert!(md.contains("`display bgp`"));
        for frame in [
            "Portal do cliente",
            "Todos os direitos",
            "Produtos relacionados",
            "analytics",
            "font-family",
            "Manual XR-9000\n",
        ] {
            assert!(!md.contains(frame), "{frame}");
        }
        assert!(!md.contains("\n\n\n"));
    }

    #[test]
    fn small_pieces_convert_as_expected() {
        let md = html_to_markdown(
            "<p>Olá &amp; <b>mundo</b> &#233;&#x21;</p><ul><li>a<ul><li>b</li></ul></li></ul>\
             <a href=\"#topo\">topo</a> <a href='../x'>rel</a> <table><tr><td>a|b</td></tr></table>",
            "https://h.com/d/p.html?q=1",
        );
        assert_eq!(
            md,
            "Olá & mundo é!\n\n- a\n  - b\n\ntopo [rel](https://h.com/d/../x)\n\n| a\\|b |\n| --- |"
        );
        // Um header dentro do artigo é parte do texto.
        let md = html_to_markdown(
            "<header>menu</header><article><header><h1>T</h1></header><p>x</p></article>",
            "",
        );
        assert_eq!(md, "# T\n\nx");
    }

    #[test]
    fn a_big_page_with_a_prompt_returns_the_matching_excerpts_and_the_index() {
        let md = html_to_markdown(MANUAL, "https://docs.example.com/xr9000/manual.html");
        assert!(
            md.chars().count() > crate::OUTPUT_CAP_CHARS,
            "a fixture precisa ser grande"
        );
        let out = extract(
            &md,
            "limite de prefixos recebidos do vizinho BGP (maximum-prefix)",
            Some(Path::new("/tmp/pagina.md")),
        );
        assert!(out.len() < EXCERPT_BUDGET + 4_000, "{}", out.len());
        assert!(out.contains("[página completa: /tmp/pagina.md"));
        assert!(out.contains(
            "## Índice da página\n- Manual de referência do roteador XR-9000\n  - Interfaces\n"
        ));
        assert!(out.contains("  - VLAN\n"));
        assert!(
            out.contains("### Manual de referência do roteador XR-9000 › BGP › Comandos de BGP")
        );
        assert!(out.contains("peer 192.0.2.1 route-limit 1000 80"));
        // O que não tem a ver com o pedido fica de fora.
        assert!(!out.contains("traffic classifier"));
        assert!(!out.contains("ntp-service unicast-server"));

        let none = extract(&md, "zzzz", None);
        assert!(none.contains("nenhum trecho casou"));
        assert!(none.contains("não foi possível gravar"));
    }
}
