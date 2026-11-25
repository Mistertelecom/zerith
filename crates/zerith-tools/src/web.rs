//! `web_fetch` (and `web_search` when a search endpoint is configured).
//!
//! Both carry the policy action `Network(url)`, so the policy decides whether
//! to ask. `web_fetch` refuses loopback, private and link-local addresses
//! unless the call says `allow_private`: the check runs in the resolver, so it
//! covers every redirect hop and a hostname that points at an internal address
//! (DNS rebinding included), not just the URL the model wrote.
//!
//! `web_search` always exists: against `ZERITH_SEARCH_URL` (JSON) when that is
//! set when the registry is built, else against DuckDuckGo's HTML page, parsed
//! into title / url / snippet.

use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

mod page;
pub use page::html_to_markdown;

/// Acima disto, com `prompt`, a página volta como índice + trechos.
const EXTRACT_ABOVE: usize = crate::OUTPUT_CAP_CHARS;

/// Largest body read, in bytes.
pub const MAX_BODY: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: u32 = 5;

/// Env var holding the search endpoint (see [`SearchTool`]).
pub const SEARCH_ENV: &str = "ZERITH_SEARCH_URL";

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    let mut v = vec![make_with(
        "web_fetch",
        "Baixa uma página http(s) e devolve o conteúdo em markdown (títulos, listas, tabelas, links, código), sem menu, cabeçalho, rodapé e scripts. Página grande com `prompt`: devolve o índice de títulos, só os trechos que casam com o pedido e o arquivo com a página inteira. Endereços locais e privados são bloqueados, salvo allow_private. Limite de 1 MB e 20 s.",
        json!({"type": "object", "properties": {
            "url": {"type": "string"},
            "prompt": {"type": "string", "description": "o que procurar na página; numa página grande escolhe os trechos devolvidos"},
            "allow_private": {"type": "boolean", "description": "permite localhost e redes privadas (padrão false)"}
        }, "required": ["url"]}),
        "a página em markdown, ou índice + trechos + caminho da página inteira",
        Permission::Network,
        30,
        true,
        |a, _| Ok(Some(Action::Network(fetch_label(a)?))),
        fetch,
    )];
    let url = std::env::var(SEARCH_ENV).unwrap_or_default();
    v.push(Box::new(if url.trim().is_empty() {
        SearchTool::duckduckgo()
    } else {
        SearchTool::new(url)
    }));
    v
}

fn fetch_label(args: &Value) -> Result<String> {
    let url = str_arg(args, "url")?;
    Ok(if args["allow_private"].as_bool().unwrap_or(false) {
        format!("{url} (rede privada permitida)")
    } else {
        url.to_string()
    })
}

// --- address filter ---------------------------------------------------------

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                // 0.0.0.0/8
                || o[0] == 0)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique local, fe80::/10 link local
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Resolves like the system, then drops every non-public address.
struct PublicOnly;

impl ureq::Resolver for PublicOnly {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = netloc
            .to_socket_addrs()?
            .filter(|a| is_public(a.ip()))
            .collect();
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endereço local ou privado bloqueado (use allow_private)",
            ));
        }
        Ok(addrs)
    }
}

// --- fetching ---------------------------------------------------------------

/// One GET, cancellable: the request runs on a thread and the caller polls the
/// token. The thread is bounded by the request timeout, so a cancelled call
/// leaves nothing running for long.
fn get(url: &str, private: bool, exec: &Exec<'_>) -> Result<(String, Vec<u8>)> {
    request(url, None, private, exec)
}

/// GET, or a form POST when `form` is given.
fn request(
    url: &str,
    form: Option<String>,
    private: bool,
    exec: &Exec<'_>,
) -> Result<(String, Vec<u8>)> {
    let mut b = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(MAX_REDIRECTS)
        .user_agent("zerith");
    if !private {
        b = b.resolver(PublicOnly);
    }
    let agent = b.build();
    let url_owned = url.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> std::result::Result<(String, Vec<u8>), String> {
            let sent = match &form {
                // DuckDuckGo answers a bare client with a bot check (HTTP 202).
                Some(body) => agent
                    .post(&url_owned)
                    .set("User-Agent", BROWSER_UA)
                    .set("Content-Type", "application/x-www-form-urlencoded")
                    .send_string(body),
                None => agent.get(&url_owned).call(),
            };
            let resp = sent.map_err(|e| match e {
                ureq::Error::Status(code, _) => format!("HTTP {code}"),
                other => other.to_string(),
            })?;
            let ctype = resp.content_type().to_string();
            let mut body = Vec::new();
            resp.into_reader()
                .take(MAX_BODY as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|e| e.to_string())?;
            Ok((ctype, body))
        })();
        let _ = tx.send(r);
    });
    let stop = exec
        .deadline
        .min(Instant::now() + TIMEOUT + Duration::from_secs(2));
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => return r.map_err(ToolError::Failed),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if exec.cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                if Instant::now() >= stop {
                    return Err(ToolError::Timeout(TIMEOUT));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ToolError::Failed(
                    "a requisição terminou sem resposta".into(),
                ))
            }
        }
    }
}

fn check_url(url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(())
    } else {
        Err(ToolError::BadArguments(
            "só URLs http:// e https:// são aceitas".into(),
        ))
    }
}

fn fetch(args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let url = str_arg(args, "url")?;
    check_url(url)?;
    let private = args["allow_private"].as_bool().unwrap_or(false);
    let (ctype, mut body) = get(url, private, exec)?;
    let cut = body.len() > MAX_BODY;
    body.truncate(MAX_BODY);
    let raw = String::from_utf8_lossy(&body).into_owned();
    let mut text = if ctype.contains("html") || raw.trim_start().starts_with('<') {
        html_to_markdown(&raw, url)
    } else {
        raw
    };
    if cut {
        text.push_str("\n[página cortada em 1 MB]");
    }
    let Some(p) = opt_str(args, "prompt") else {
        return Ok(text);
    };
    if text.chars().count() <= EXTRACT_ABOVE {
        return Ok(format!("[pedido: {p}]\n{text}"));
    }
    let full = save_page(&text, exec);
    Ok(format!(
        "[pedido: {p}]\n{}",
        page::extract(&text, p, full.as_deref())
    ))
}

/// A página inteira ao lado dos spills da sessão (`<raiz>/spill/<sessão>/`),
/// onde ler não pede aprovação. `None`: não deu para gravar.
fn save_page(text: &str, exec: &Exec<'_>) -> Option<std::path::PathBuf> {
    // O mesmo nome seguro que `zerith_runtime::spill` usa para a pasta.
    let session: String = exec
        .host
        .session()
        .unwrap_or_else(|| "web".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let dir = zerith_core::Paths::resolve()
        .root()
        .join("spill")
        .join(session);
    std::fs::create_dir_all(&dir).ok()?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("web_fetch-{ms}.md"));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// A tag stripper, not a parser: drops `<script>`/`<style>` bodies, tags and
/// comments, decodes the common entities, collapses blank space. Good enough
/// for a model to read; not for rendering.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            if html[i..].starts_with("<!--") {
                i = html[i..].find("-->").map_or(b.len(), |p| i + p + 3);
                continue;
            }
            let skip = ["script", "style", "noscript", "svg"]
                .iter()
                .find(|t| lower[i + 1..].starts_with(*t));
            if let Some(t) = skip {
                let close = format!("</{t}");
                i = lower[i..].find(&close).map_or(b.len(), |p| {
                    let from = i + p;
                    lower[from..].find('>').map_or(b.len(), |q| from + q + 1)
                });
                continue;
            }
            let (tag_end, end) = match html[i..].find('>') {
                Some(p) => (i + p, i + p + 1),
                None => (b.len(), b.len()),
            };
            let tag = lower[i + 1..tag_end.max(i + 1)]
                .trim_start_matches('/')
                .split(|c: char| !c.is_ascii_alphanumeric())
                .next()
                .unwrap_or("");
            if matches!(
                tag,
                "p" | "div"
                    | "br"
                    | "li"
                    | "tr"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "section"
                    | "article"
                    | "header"
                    | "footer"
                    | "pre"
                    | "ul"
                    | "ol"
                    | "table"
            ) {
                out.push('\n');
            } else {
                out.push(' ');
            }
            i = end;
        } else {
            let next = html[i..].find('<').map_or(b.len(), |p| i + p);
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    decoded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// --- search -----------------------------------------------------------------

const DDG_URL: &str = "https://html.duckduckgo.com/html/";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const MAX_RESULTS: usize = 10;

/// `web_search`. With a template (`ZERITH_SEARCH_URL`): `{query}` is replaced
/// with the percent-encoded query, or `?q=` is appended when there is no
/// placeholder; the response must be JSON and is returned pretty-printed.
/// Without one: DuckDuckGo HTML, parsed by [`parse_ddg`].
pub struct SearchTool {
    spec: ToolSpec,
    /// Empty: DuckDuckGo.
    template: String,
}

impl SearchTool {
    /// The default: DuckDuckGo, no account and no configuration.
    pub fn duckduckgo() -> Self {
        let mut t = Self::new("");
        t.spec.description = "Pesquisa na web (DuckDuckGo) e devolve até 10 resultados com título, URL e trecho. Use para fatos recentes ou que você não sabe; depois leia a página com web_fetch e cite a URL.".into();
        t.spec.returns = "resultados numerados: título, URL e trecho";
        t
    }

    pub fn new(template: impl Into<String>) -> Self {
        let spec = ToolSpec {
            name: "web_search".into(),
            description: "Pesquisa na web pelo endpoint configurado (ZERITH_SEARCH_URL) e devolve o JSON da resposta.".into(),
            parameters: json!({"type": "object", "properties": {
                "query": {"type": "string"}
            }, "required": ["query"]}),
            returns: "JSON do mecanismo de busca",
            permission: Permission::Network,
            timeout: Duration::from_secs(30),
            cancellable: true,
            local: false,
            source: "built-in".into(),
        };
        Self {
            spec,
            template: template.into(),
        }
    }

    fn url(&self, query: &str) -> String {
        let q = encode(query);
        if self.template.is_empty() {
            format!("{DDG_URL}?q={q}")
        } else if self.template.contains("{query}") {
            self.template.replace("{query}", &q)
        } else if self.template.contains('?') {
            format!("{}&q={q}", self.template)
        } else {
            format!("{}?q={q}", self.template)
        }
    }
}

fn encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl Tool for SearchTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn action(&self, args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
        Ok(Some(Action::Network(self.url(str_arg(args, "query")?))))
    }

    fn run(&self, args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        let query = str_arg(args, "query")?;
        if self.template.is_empty() {
            let (_, body) = request(DDG_URL, Some(format!("q={}", encode(query))), false, exec)?;
            let hits = parse_ddg(&String::from_utf8_lossy(&body));
            if hits.is_empty() {
                return Ok(format!("nenhum resultado para: {query}"));
            }
            return Ok(hits
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet))
                .collect::<Vec<_>>()
                .join("\n"));
        }
        let url = self.url(query);
        check_url(&url)?;
        // The endpoint is the operator's own choice (often a local SearxNG),
        // so private addresses are allowed here, unlike `web_fetch`.
        let (_, body) = get(&url, true, exec)?;
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| ToolError::Failed(format!("a busca não devolveu JSON: {e}")))?;
        Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    fn concurrency_safe(&self, _: &Value) -> bool {
        true
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// The inner text of the first `<a ... class="<class>" ... href="..">..</a>`
/// at or after `from`, with its href and where it ends.
fn anchor(html: &str, from: usize, class: &str) -> Option<(String, String, usize)> {
    let at = from + html[from..].find(&format!("class=\"{class}\""))?;
    let open = html[..at].rfind("<a")?;
    let gt = at + html[at..].find('>')?;
    let href = html[open..gt]
        .split("href=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("");
    let close = gt + html[gt..].find("</a>")?;
    // `<b>` marks the query words; dropped so it adds no spaces.
    let inner = html[gt + 1..close].replace("<b>", "").replace("</b>", "");
    Some((href.replace("&amp;", "&"), html_to_text(&inner), close))
}

/// The real target of a DuckDuckGo redirect (`//duckduckgo.com/l/?uddg=...`);
/// any other link is returned as is.
fn ddg_target(href: &str) -> String {
    let Some(enc) = href.split("uddg=").nth(1) else {
        return if href.starts_with("//") {
            format!("https:{href}")
        } else {
            href.to_string()
        };
    };
    let enc = enc.split('&').next().unwrap_or("");
    let b = enc.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len())
            .then(|| {
                std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
            })
            .flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(if b[i] == b'+' { b' ' } else { b[i] });
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Results of a DuckDuckGo HTML page, ads skipped, at most [`MAX_RESULTS`].
pub fn parse_ddg(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut pos = 0;
    while hits.len() < MAX_RESULTS {
        let Some((href, title, end)) = anchor(html, pos, "result__a") else {
            break;
        };
        // The snippet belongs to this result only if it comes before the next.
        let next = html[end..]
            .find("class=\"result__a\"")
            .map_or(html.len(), |p| end + p);
        let snippet = anchor(&html[..next], end, "result__snippet")
            .map(|(_, s, _)| s)
            .unwrap_or_default();
        pos = end;
        let url = ddg_target(&href);
        if url.contains("duckduckgo.com/y.js") || !url.starts_with("http") {
            continue; // an ad, or a link to DuckDuckGo itself
        }
        hits.push(Hit {
            title,
            url,
            snippet,
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckduckgo_page_becomes_results() {
        let hits = parse_ddg(include_str!("../tests/fixtures/ddg.html"));
        assert_eq!(hits.len(), 3, "{hits:?}");
        assert_eq!(hits[0].title, "Rust Versions | Rust Changelogs");
        assert_eq!(hits[0].url, "https://releases.rs/");
        assert!(hits[0]
            .snippet
            .starts_with("Rust Versions # Stable: 1.100.0"));
        // A redirect link resolves to the page it points at; ads are dropped.
        assert_eq!(
            hits[1].url,
            "https://blog.rust-lang.org/2026/11/12/Rust-1.100.0/?a=1&b=2"
        );
        assert_eq!(hits[2].title, "Install Rust - Rust Programming Language");
        assert_eq!(hits[2].snippet, "");
    }

    #[test]
    fn html_becomes_readable_text() {
        let t = html_to_text(
            "<html><head><style>p{color:red}</style><script>var x='<b>';</script></head>\
             <body><h1>Título</h1><p>Olá &amp; <b>mundo</b></p><!-- c --><p>fim</p></body></html>",
        );
        assert_eq!(t, "Título\nOlá & mundo\nfim");
    }

    #[test]
    fn address_filter_covers_local_private_and_mapped() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(s.parse().unwrap()), "{s}");
        }
        for s in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(is_public(s.parse().unwrap()), "{s}");
        }
    }

    #[test]
    fn search_url_takes_a_placeholder_or_appends() {
        let a = SearchTool::new("http://x/s?q={query}&f=json");
        assert_eq!(a.url("a b&c"), "http://x/s?q=a%20b%26c&f=json");
        assert_eq!(
            SearchTool::new("http://x/s").url("é"),
            "http://x/s?q=%C3%A9"
        );
        assert_eq!(
            SearchTool::new("http://x/s?f=1").url("a"),
            "http://x/s?f=1&q=a"
        );
    }

    #[test]
    fn ddg_target_handles_percent_before_multibyte_utf8_without_panicking() {
        assert_eq!(ddg_target("//duckduckgo.com/l/?uddg=%aé&rut=1"), "%aé");
        assert_eq!(ddg_target("//duckduckgo.com/l/?uddg=%€"), "%€");
        assert_eq!(
            ddg_target("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F%aé%2F%€&x=1"),
            "https://example.com/%aé/%€"
        );
    }
}
