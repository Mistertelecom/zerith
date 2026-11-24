//! Parser HTTP/1.1 mínimo e fechado: uma requisição por conexão, cabeçalhos
//! até 16 KB, corpo até 1 MB com `Content-Length` (sem chunked), timeout de
//! leitura. Mesma ideia do servidor de webhook do desktop
//! (`zerith-desktop/src/workflows.rs`, `process_webhook_conn`), só que síncrono.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Tempo total para chegar a requisição inteira (cabeçalho + corpo).
pub const READ_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Nomes em minúsculas.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Token do `Authorization: Bearer <token>` (esquema sem diferenciar caixa).
    pub fn bearer(&self) -> Option<&str> {
        let v = self.header("authorization")?;
        let (scheme, tok) = v.split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| tok.trim())
            .filter(|t| !t.is_empty())
    }

    /// Valor de `chave` na query string (sem decodificação percentual: só
    /// usamos números e ids simples).
    pub fn query_param(&self, key: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    pub msg: &'static str,
}

fn err(status: u16, msg: &'static str) -> HttpError {
    HttpError { status, msg }
}

/// Caracteres de `token` da RFC 9110 (nomes de cabeçalho e método).
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Lê e valida uma requisição. `Err(None)` = conexão caiu/expirou sem nada
/// para responder; `Err(Some(e))` = responder `e.status`.
pub fn read_request<R: Read>(r: &mut R) -> Result<Request, Option<HttpError>> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut raw: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > MAX_HEADER_BYTES {
            return Err(Some(err(431, "cabeçalhos grandes demais")));
        }
        if Instant::now() > deadline {
            return Err(None);
        }
        match r.read(&mut chunk) {
            Ok(0) if raw.is_empty() => return Err(None),
            Ok(0) => return Err(Some(err(400, "requisição incompleta"))),
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(None),
        }
    };
    if head_end > MAX_HEADER_BYTES {
        return Err(Some(err(431, "cabeçalhos grandes demais")));
    }
    let mut body = raw.split_off(head_end + 4);
    let head = std::str::from_utf8(&raw[..head_end])
        .map_err(|_| Some(err(400, "cabeçalho não é UTF-8")))?;
    let mut lines = head.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Some(err(400, "linha de requisição malformada")));
    };
    if method.is_empty() || !method.bytes().all(is_tchar) {
        return Err(Some(err(400, "método inválido")));
    }
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(Some(err(505, "só HTTP/1.1")));
    }
    if !target.starts_with('/') || target.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(Some(err(400, "alvo inválido")));
    }

    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    for line in lines {
        // obs-fold (linha começando com espaço) e nome com espaço antes do
        // ':' são vetores clássicos de request smuggling: recusa.
        let Some((name, value)) = line.split_once(':') else {
            return Err(Some(err(400, "cabeçalho malformado")));
        };
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return Err(Some(err(400, "cabeçalho malformado")));
        }
        if value.bytes().any(|b| (b < b' ' && b != b'\t') || b == 0x7f) {
            return Err(Some(err(400, "cabeçalho malformado")));
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim_matches([' ', '\t']).to_string();
        match name.as_str() {
            "transfer-encoding" => return Err(Some(err(501, "use Content-Length, sem chunked"))),
            "content-length" => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Some(err(400, "Content-Length inválido")));
                }
                let n: usize = value
                    .parse()
                    .map_err(|_| Some(err(413, "corpo grande demais")))?;
                if content_length.is_some_and(|old| old != n) {
                    return Err(Some(err(400, "Content-Length duplicado")));
                }
                content_length = Some(n);
            }
            _ => {}
        }
        headers.push((name, value));
    }
    let len = content_length.unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(Some(err(413, "corpo grande demais (máx. 1 MB)")));
    }

    if body.len() > len {
        // Bytes além do corpo = segunda requisição (pipelining): não aceitamos.
        return Err(Some(err(400, "dados além do Content-Length")));
    }
    while body.len() < len {
        if Instant::now() > deadline {
            return Err(None);
        }
        let want = (len - body.len()).min(chunk.len());
        match r.read(&mut chunk[..want]) {
            Ok(0) => return Err(Some(err(400, "corpo incompleto"))),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(None),
        }
    }

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Request {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
        headers,
        body,
    })
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Internal Server Error",
    }
}

/// Escreve uma resposta JSON completa e fecha (uma requisição por conexão).
pub fn write_json<W: Write>(w: &mut W, status: u16, body: &str) -> std::io::Result<()> {
    let mut extra = String::new();
    if status == 401 {
        extra.push_str("WWW-Authenticate: Bearer\r\n");
    }
    if status == 429 {
        extra.push_str("Retry-After: 60\r\n");
    }
    write!(
        w,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         {extra}Connection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    )?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(raw: &[u8]) -> Result<Request, Option<HttpError>> {
        read_request(&mut Cursor::new(raw.to_vec()))
    }

    fn status(raw: &[u8]) -> u16 {
        parse(raw).unwrap_err().unwrap().status
    }

    #[test]
    fn requisicao_valida_com_corpo_query_e_bearer() {
        let r = parse(
            b"PUT /v1/workflows/a?limit=5 HTTP/1.1\r\nHost: x\r\nAuthorization: bearer  abc \r\nContent-Length: 2\r\n\r\n{}",
        )
        .unwrap();
        assert_eq!(
            (r.method.as_str(), r.path.as_str()),
            ("PUT", "/v1/workflows/a")
        );
        assert_eq!(r.query_param("limit"), Some("5"));
        assert_eq!(r.bearer(), Some("abc"));
        assert_eq!(r.body, b"{}");
    }

    #[test]
    fn limites_de_cabecalho_e_corpo() {
        let mut big = b"GET / HTTP/1.1\r\nX: ".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_HEADER_BYTES + 10));
        big.extend_from_slice(b"\r\n\r\n");
        assert_eq!(status(&big), 431);
        // Cabeçalho sem fim também para em 16 KB, sem ler para sempre.
        assert_eq!(status(&big[..MAX_HEADER_BYTES + 5]), 431);
        let over = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(status(over.as_bytes()), 413);
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n"),
            413
        );
    }

    #[test]
    fn cabecalhos_malformados_sao_recusados() {
        for raw in [
            &b"GET / HTTP/1.1\r\nsem-dois-pontos\r\n\r\n"[..],
            b"GET / HTTP/1.1\r\nX : y\r\n\r\n",
            b"GET / HTTP/1.1\r\nX: a\r\n continua\r\n\r\n",
            b"GET / HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            b"GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
            b"GET  / HTTP/1.1\r\n\r\n",
            b"GET relativo HTTP/1.1\r\n\r\n",
            b"POST / HTTP/1.1\r\nContent-Length: 1\r\n\r\nabGET / HTTP/1.1\r\n\r\n",
            b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab",
        ] {
            assert_eq!(status(raw), 400, "{}", String::from_utf8_lossy(raw));
        }
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
            501
        );
        assert_eq!(status(b"GET / HTTP/2\r\n\r\n"), 505);
        // Conexão aberta e fechada sem bytes: nada a responder.
        assert!(parse(b"").unwrap_err().is_none());
    }

    #[test]
    fn resposta_tem_tamanho_e_fecha_conexao() {
        let mut out = Vec::new();
        write_json(&mut out, 401, "{}").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert!(s.contains("Content-Length: 2\r\n") && s.contains("Connection: close"));
        assert!(s.contains("WWW-Authenticate: Bearer"));
    }
}
