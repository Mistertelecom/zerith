//! Nó `zabbix`: API JSON-RPC do Zabbix (alvo 7.0 LTS).
//!
//! - `POST <url>/api_jsonrpc.php`, `Content-Type: application/json-rpc`.
//! - Token de API (5.4+). Padrão `auth_mode: "header"`
//!   (`Authorization: Bearer <token>`, recomendado desde 6.4; no 7.0 o campo
//!   `auth` no corpo está obsoleto). `auth_mode: "auth_field"` manda `auth` no
//!   corpo, para versões antigas.
//! - O token vem de `token_secret` (nome do segredo) ou de `token` com
//!   `{{secret:nome}}`; texto puro é recusado. Nunca aparece em erro/saída.
//!
//! Operações (`operation`):
//! - `problems`: `problem.get` (`recent: false`, `suppressed: false`,
//!   `severities` opcional) → `trigger.get` (`selectHosts`) → `host.get`
//!   (`selectInterfaces`) para o IP. No 7.0 `problem.get` não aceita
//!   `selectHosts`, por isso os três passos.
//! - `hosts_unavailable`: problemas cujo nome casa com `patterns` (padrão
//!   `Unavailable by ICMP ping|is not available`) + interfaces com
//!   `available = 2` (agente/SNMP/IPMI/JMX), um item por host. Não dá para
//!   usar só `available`: host monitorado só por ICMP fica com 0
//!   (desconhecido) mesmo fora do ar.
//! - `raw`: `method` e `params` (JSON) livres; devolve o `result`.
//!
//! Saída de `problems`/`hosts_unavailable`: JSON
//! `[{host, ip, problema, severidade, desde}]` (`desde` em segundos UNIX), que
//! o Transformar `offline_report` vira mensagem.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::runner::secret_value;
use crate::{Result, WorkflowError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthMode {
    Header,
    Body,
}

pub(crate) struct Client {
    endpoint: String,
    token: String,
    auth: AuthMode,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Offline {
    pub host: String,
    pub ip: String,
    pub problema: String,
    pub severidade: String,
    pub desde: i64,
}

fn cfg_str<'a>(cfg: &'a Value, key: &str) -> Option<&'a str> {
    cfg.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Texto de um campo que o Zabbix devolve como string ("4") ou número.
fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn severity_name(code: &str) -> String {
    match code {
        "0" => "Não classificada",
        "1" => "Informação",
        "2" => "Atenção",
        "3" => "Média",
        "4" => "Alta",
        "5" => "Desastre",
        other => other,
    }
    .to_string()
}

fn iface_type(code: &str) -> &'static str {
    match code {
        "1" => "agente Zabbix",
        "2" => "SNMP",
        "3" => "IPMI",
        "4" => "JMX",
        _ => "interface",
    }
}

fn clip(t: &str, max: usize) -> String {
    let mut out: String = t.chars().take(max).collect();
    if t.chars().count() > max {
        out.push('…');
    }
    out
}

/// Aceita qualquer certificado (`insecure_tls`: Zabbix com certificado
/// próprio na rede interna). As assinaturas do handshake continuam checadas.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn insecure_tls_config() -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| WorkflowError::Exec(format!("TLS: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

impl Client {
    pub(crate) fn new(
        url: &str,
        token: String,
        auth: AuthMode,
        timeout: Duration,
        insecure_tls: bool,
    ) -> Result<Self> {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(WorkflowError::Exec(
                "URL do Zabbix deve começar com http:// ou https:// (ex.: https://zabbix.exemplo/zabbix)"
                    .into(),
            ));
        }
        let endpoint = if url.ends_with(".php") {
            url.to_string()
        } else {
            format!("{}/api_jsonrpc.php", url.trim_end_matches('/'))
        };
        let mut b = ureq::AgentBuilder::new().timeout(timeout).redirects(0);
        if insecure_tls {
            b = b.tls_config(insecure_tls_config()?);
        }
        Ok(Self {
            endpoint,
            token,
            auth,
            agent: b.build(),
        })
    }

    fn err(&self, msg: String) -> WorkflowError {
        let msg = if self.token.is_empty() {
            msg
        } else {
            msg.replace(&self.token, "***")
        };
        WorkflowError::Exec(msg)
    }

    /// Uma chamada JSON-RPC; `error` vira falha com `message` e `data`.
    pub(crate) fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut body = json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1});
        let mut req = self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json-rpc");
        match self.auth {
            AuthMode::Header => {
                req = req.set("Authorization", &format!("Bearer {}", self.token));
            }
            AuthMode::Body => body["auth"] = json!(self.token),
        }
        let text = match req.send_string(&body.to_string()) {
            Ok(r) => r
                .into_string()
                .map_err(|e| self.err(format!("Zabbix {method}: resposta ilegível ({e})")))?,
            Err(ureq::Error::Status(code, r)) => {
                let t = r.into_string().unwrap_or_default();
                return Err(self.err(format!(
                    "Zabbix {method}: HTTP {code} {}",
                    clip(t.trim(), 300)
                )));
            }
            Err(e) => {
                return Err(self.err(format!(
                    "Zabbix {method}: falha ao chamar {} ({e})",
                    self.endpoint
                )))
            }
        };
        let v: Value = serde_json::from_str(&text).map_err(|_| {
            self.err(format!(
                "Zabbix {method}: resposta não é JSON-RPC: {}",
                clip(text.trim(), 300)
            ))
        })?;
        if let Some(e) = v.get("error") {
            let msg = s(e, "message");
            let data = s(e, "data");
            let hint = if data.contains("Not authori") || msg.contains("Not authori") {
                " (token inválido/expirado ou sem permissão; confira o segredo e o auth_mode)"
            } else {
                ""
            };
            return Err(self.err(format!(
                "Zabbix {method}: {msg}{}{hint}",
                if data.is_empty() {
                    String::new()
                } else {
                    format!(" {data}")
                }
            )));
        }
        v.get("result")
            .cloned()
            .ok_or_else(|| self.err(format!("Zabbix {method}: resposta sem `result`")))
    }

    fn call_list(&self, method: &str, params: Value) -> Result<Vec<Value>> {
        match self.call(method, params)? {
            Value::Array(a) => Ok(a),
            other => Err(self.err(format!(
                "Zabbix {method}: esperava lista, veio {}",
                clip(&other.to_string(), 120)
            ))),
        }
    }

    /// Ids dos grupos de hosts pelo nome exato.
    fn group_ids(&self, names: &[String]) -> Result<Option<Vec<String>>> {
        if names.is_empty() {
            return Ok(None);
        }
        let found = self.call_list(
            "hostgroup.get",
            json!({"output": ["groupid", "name"], "filter": {"name": names}}),
        )?;
        if found.is_empty() {
            return Err(self.err(format!(
                "grupo(s) de hosts não encontrado(s) no Zabbix: {}",
                names.join(", ")
            )));
        }
        Ok(Some(found.iter().map(|g| s(g, "groupid")).collect()))
    }

    /// IP principal de cada host (interface `main=1`, agente antes de SNMP).
    fn host_ips(&self, hostids: &[String]) -> Result<HashMap<String, String>> {
        if hostids.is_empty() {
            return Ok(HashMap::new());
        }
        let hosts = self.call_list(
            "host.get",
            json!({"hostids": hostids, "output": ["hostid"],
                   "selectInterfaces": ["ip", "type", "main", "available"]}),
        )?;
        Ok(hosts.iter().map(|h| (s(h, "hostid"), main_ip(h))).collect())
    }

    /// Problemas ativos com host e IP.
    pub(crate) fn problems(
        &self,
        groupids: Option<&[String]>,
        severities: Option<&[u8]>,
    ) -> Result<Vec<Offline>> {
        let mut params = json!({
            "output": ["eventid", "objectid", "name", "severity", "clock"],
            "recent": false,
            "suppressed": false,
            "sortfield": ["eventid"],
            "sortorder": "DESC"
        });
        if let Some(g) = groupids {
            params["groupids"] = json!(g);
        }
        if let Some(sv) = severities {
            params["severities"] = json!(sv);
        }
        let probs = self.call_list("problem.get", params)?;
        if probs.is_empty() {
            return Ok(Vec::new());
        }
        let trig_ids: BTreeSet<String> = probs.iter().map(|p| s(p, "objectid")).collect();
        let trigs = self.call_list(
            "trigger.get",
            json!({"triggerids": trig_ids, "output": ["triggerid", "description"],
                   "selectHosts": ["hostid", "host"]}),
        )?;
        let host_of: HashMap<String, (String, String)> = trigs
            .iter()
            .filter_map(|t| {
                let h = t.get("hosts")?.as_array()?.first()?;
                Some((s(t, "triggerid"), (s(h, "hostid"), s(h, "host"))))
            })
            .collect();
        let hostids: Vec<String> = host_of
            .values()
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let ips = self.host_ips(&hostids)?;
        Ok(probs
            .iter()
            .map(|p| {
                let (hid, host) = host_of.get(&s(p, "objectid")).cloned().unwrap_or_default();
                Offline {
                    host: if host.is_empty() { "?".into() } else { host },
                    ip: ips.get(&hid).cloned().unwrap_or_default(),
                    problema: s(p, "name"),
                    severidade: severity_name(&s(p, "severity")),
                    desde: s(p, "clock").parse().unwrap_or(0),
                }
            })
            .collect())
    }

    /// Hosts fora do ar: problemas que casam com `patterns` (ICMP, agente) e
    /// interfaces `available = 2`; um item por host.
    pub(crate) fn hosts_unavailable(
        &self,
        groupids: Option<&[String]>,
        patterns: &[String],
    ) -> Result<Vec<Offline>> {
        let mut out: Vec<Offline> = Vec::new();
        for p in self.problems(groupids, None)? {
            let name = p.problema.to_lowercase();
            if patterns.iter().any(|pt| name.contains(pt.as_str()))
                && !out.iter().any(|o| o.host == p.host)
            {
                out.push(p);
            }
        }
        let mut params = json!({
            "output": ["hostid", "host"],
            "monitored_hosts": true,
            "selectInterfaces": ["ip", "type", "main", "available", "error", "errors_from"]
        });
        if let Some(g) = groupids {
            params["groupids"] = json!(g);
        }
        for h in self.call_list("host.get", params)? {
            let host = s(&h, "host");
            if out.iter().any(|o| o.host == host) {
                continue;
            }
            let ifaces = h.get("interfaces").and_then(Value::as_array);
            if let Some(i) = ifaces
                .into_iter()
                .flatten()
                .find(|i| s(i, "available") == "2")
            {
                let err = s(i, "error");
                out.push(Offline {
                    host,
                    ip: s(i, "ip"),
                    problema: format!(
                        "{} indisponível{}",
                        iface_type(&s(i, "type")),
                        if err.is_empty() {
                            String::new()
                        } else {
                            format!(": {err}")
                        }
                    ),
                    severidade: "Indisponível".into(),
                    desde: s(i, "errors_from").parse().unwrap_or(0),
                });
            }
        }
        Ok(out)
    }
}

fn main_ip(host: &Value) -> String {
    let Some(ifaces) = host.get("interfaces").and_then(Value::as_array) else {
        return String::new();
    };
    let rank = |i: &Value| (s(i, "main") != "1", s(i, "type"));
    ifaces
        .iter()
        .filter(|i| !s(i, "ip").is_empty())
        .min_by_key(|i| rank(i))
        .map(|i| s(i, "ip"))
        .unwrap_or_default()
}

/// Token do nó: `token_secret` (nome) ou `token` com `{{secret:nome}}`.
fn token(cfg: &Value, interp: &dyn Fn(&str) -> String) -> Result<String> {
    if let Some(name) = cfg_str(cfg, "token_secret") {
        return Ok(secret_value(name)?.trim().to_string());
    }
    match cfg_str(cfg, "token") {
        Some(raw) if raw.contains("{{secret:") => Ok(interp(raw).trim().to_string()),
        Some(_) => Err(WorkflowError::Exec(
            "token do Zabbix em texto puro não é aceito: use token_secret (nome do segredo) \
             ou {{secret:nome}}"
                .into(),
        )),
        None => Err(WorkflowError::Exec(
            "token do Zabbix: informe token_secret (nome do segredo no cofre)".into(),
        )),
    }
}

fn severities(cfg: &Value) -> Option<Vec<u8>> {
    if let Some(a) = cfg.get("severities").and_then(Value::as_array) {
        let v: Vec<u8> = a
            .iter()
            .filter_map(|x| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()))
            .filter(|n| *n <= 5)
            .map(|n| n as u8)
            .collect();
        return (!v.is_empty()).then_some(v);
    }
    cfg.get("min_severity")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|n| *n > 0 && *n <= 5)
        .map(|n| (n as u8..=5).collect())
}

/// Executa o nó. `interp`: interpolação do runner (`{{prev}}`, `{{secret:x}}`).
pub(crate) fn run(cfg: &Value, interp: &dyn Fn(&str) -> String) -> Result<String> {
    let url = cfg_str(cfg, "url")
        .map(interp)
        .ok_or_else(|| WorkflowError::Exec("URL do Zabbix obrigatória".into()))?;
    let auth = match cfg_str(cfg, "auth_mode") {
        Some("auth_field" | "body" | "auth") => AuthMode::Body,
        _ => AuthMode::Header,
    };
    let timeout = cfg
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 120);
    let insecure = cfg
        .get("insecure_tls")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let c = Client::new(
        &url,
        token(cfg, interp)?,
        auth,
        Duration::from_secs(timeout),
        insecure,
    )?;
    let list = |key: &str, sep: char| -> Vec<String> {
        cfg_str(cfg, key)
            .map(interp)
            .unwrap_or_default()
            .split(sep)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    match cfg_str(cfg, "operation").unwrap_or("problems") {
        "problems" => {
            let g = c.group_ids(&list("groups", ','))?;
            pretty(&c.problems(g.as_deref(), severities(cfg).as_deref())?)
        }
        "hosts_unavailable" => {
            let g = c.group_ids(&list("groups", ','))?;
            let mut pats: Vec<String> = list("patterns", '|')
                .into_iter()
                .map(|p| p.to_lowercase())
                .collect();
            if pats.is_empty() {
                pats = vec!["unavailable by icmp ping".into(), "is not available".into()];
            }
            pretty(&c.hosts_unavailable(g.as_deref(), &pats)?)
        }
        "raw" => {
            let method = cfg_str(cfg, "method").ok_or_else(|| {
                WorkflowError::Exec("operação raw: informe o método (ex.: host.get)".into())
            })?;
            let params = match cfg_str(cfg, "params") {
                Some(p) => serde_json::from_str(&interp(p)).map_err(|e| {
                    WorkflowError::Exec(format!("params do Zabbix não é JSON válido: {e}"))
                })?,
                None => json!({}),
            };
            pretty(&c.call(method, params)?)
        }
        other => Err(WorkflowError::Exec(format!(
            "operação do Zabbix desconhecida: '{other}' (problems, hosts_unavailable, raw)"
        ))),
    }
}

fn pretty<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// Requisição recebida pelo Zabbix falso: (cabeçalhos em minúsculas, corpo).
    pub(crate) type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// Servidor JSON-RPC falso em porta efêmera. `answer(method, body)` dá o
    /// JSON da resposta. Respostas no formato do Zabbix 7.0.
    pub(crate) fn fake_zabbix(
        answer: impl Fn(&str, &Value) -> Value + Send + 'static,
    ) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/zabbix", l.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut rd = BufReader::new(stream.try_clone().unwrap());
                let (mut head, mut len) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&low);
                }
                let mut body = vec![0; len];
                rd.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = body["method"].as_str().unwrap_or("").to_string();
                let resp = answer(&method, &body).to_string();
                log.lock().unwrap().push((head, body));
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            }
        });
        (url, seen)
    }

    /// Respostas reais do 7.0 (campos e tipos), hosts/IPs de exemplo.
    pub(crate) fn zabbix7(method: &str, body: &Value) -> Value {
        let result = match method {
            "hostgroup.get" => {
                if body["params"]["filter"]["name"][0] == "Rede" {
                    json!([{"groupid": "22", "name": "Rede"}])
                } else {
                    json!([])
                }
            }
            "problem.get" => json!([
                {"eventid": "6", "objectid": "25227", "name": "ICMP Ping: Unavailable by ICMP ping",
                 "severity": "4", "clock": "1795567712"},
                {"eventid": "24", "objectid": "22391",
                 "name": "Linux: Zabbix agent is not available (for 3m)",
                 "severity": "3", "clock": "1795567946"}
            ]),
            "trigger.get" => json!([
                {"triggerid": "22391", "description": "Linux: Zabbix agent is not available",
                 "hosts": [{"hostid": "10084", "host": "Zabbix server"}]},
                {"triggerid": "25227", "description": "ICMP Ping: Unavailable by ICMP ping",
                 "hosts": [{"hostid": "10600", "host": "olt-exemplo"}]}
            ]),
            "host.get" => json!([
                {"hostid": "10084", "host": "Zabbix server", "interfaces": [
                    {"ip": "127.0.0.1", "type": "1", "main": "1", "available": "2",
                     "error": "Get value from agent failed", "errors_from": "1795567900"}]},
                {"hostid": "10600", "host": "olt-exemplo", "interfaces": [
                    {"ip": "192.0.2.10", "type": "1", "main": "1", "available": "0",
                     "error": "", "errors_from": "0"}]},
                {"hostid": "10700", "host": "switch-snmp", "interfaces": [
                    {"ip": "198.18.7.2", "type": "2", "main": "1", "available": "2",
                     "error": "Timeout while connecting", "errors_from": "1795570000"}]}
            ]),
            _ => json!(null),
        };
        json!({"jsonrpc": "2.0", "result": result, "id": 1})
    }

    fn client(url: &str, auth: AuthMode) -> Client {
        Client::new(
            url,
            "tok-SEGREDO-123".into(),
            auth,
            Duration::from_secs(5),
            false,
        )
        .unwrap()
    }

    #[test]
    fn problems_em_tres_passos_com_host_ip_e_bearer() {
        let (url, seen) = fake_zabbix(zabbix7);
        let c = client(&url, AuthMode::Header);
        let g = c.group_ids(&["Rede".into()]).unwrap();
        let p = c.problems(g.as_deref(), Some(&[3, 4, 5])).unwrap();
        assert_eq!(
            p,
            vec![
                Offline {
                    host: "olt-exemplo".into(),
                    ip: "192.0.2.10".into(),
                    problema: "ICMP Ping: Unavailable by ICMP ping".into(),
                    severidade: "Alta".into(),
                    desde: 1_795_567_712,
                },
                Offline {
                    host: "Zabbix server".into(),
                    ip: "127.0.0.1".into(),
                    problema: "Linux: Zabbix agent is not available (for 3m)".into(),
                    severidade: "Média".into(),
                    desde: 1_795_567_946,
                },
            ]
        );
        let seen = seen.lock().unwrap();
        let methods: Vec<&str> = seen
            .iter()
            .map(|(_, b)| b["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            ["hostgroup.get", "problem.get", "trigger.get", "host.get"]
        );
        let (head, body) = &seen[1];
        assert!(head.contains("authorization: bearer tok-segredo-123"));
        assert!(head.contains("content-type: application/json-rpc"));
        assert!(body.get("auth").is_none(), "Bearer não manda auth no corpo");
        let params = &body["params"];
        assert_eq!(params["recent"], false);
        assert_eq!(params["suppressed"], false);
        assert_eq!(params["groupids"], json!(["22"]));
        assert_eq!(params["severities"], json!([3, 4, 5]));
        assert!(
            params.get("selectHosts").is_none(),
            "7.0 recusa selectHosts no problem.get"
        );
        assert_eq!(seen[2].1["params"]["triggerids"], json!(["22391", "25227"]));
    }

    #[test]
    fn hosts_indisponiveis_por_icmp_agente_e_interface() {
        let (url, _) = fake_zabbix(zabbix7);
        let c = client(&url, AuthMode::Header);
        let pats = vec!["unavailable by icmp ping".into(), "is not available".into()];
        let h = c.hosts_unavailable(None, &pats).unwrap();
        let hosts: Vec<&str> = h.iter().map(|o| o.host.as_str()).collect();
        assert_eq!(hosts, ["olt-exemplo", "Zabbix server", "switch-snmp"]);
        assert_eq!(h[2].ip, "198.18.7.2");
        assert_eq!(h[2].problema, "SNMP indisponível: Timeout while connecting");
        assert_eq!(h[2].desde, 1_795_570_000);
    }

    #[test]
    fn auth_no_corpo_para_versoes_antigas() {
        let (url, seen) = fake_zabbix(zabbix7);
        let c = client(&format!("{url}/api_jsonrpc.php"), AuthMode::Body);
        c.call("host.get", json!({})).unwrap();
        let (head, body) = &seen.lock().unwrap()[0];
        assert!(!head.contains("authorization:"));
        assert_eq!(body["auth"], "tok-SEGREDO-123");
        assert!(head.starts_with("post /zabbix/api_jsonrpc.php "), "{head}");
    }

    #[test]
    fn erro_json_rpc_vira_falha_clara_sem_token() {
        let (url, _) = fake_zabbix(|m, _| {
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32602, "message": "Invalid params.",
                   "data": format!("Invalid parameter \"/\": unexpected parameter \"selectHosts\" ({m}, tok-SEGREDO-123).")}})
        });
        let e = client(&url, AuthMode::Header)
            .problems(None, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("Zabbix problem.get: Invalid params."), "{e}");
        assert!(e.contains("unexpected parameter"), "{e}");
        assert!(!e.contains("tok-SEGREDO"), "token vazou: {e}");
        assert!(e.contains("***"));

        assert!(Client::new(
            "zabbix.local",
            String::new(),
            AuthMode::Header,
            Duration::from_secs(1),
            false
        )
        .is_err());
        assert!(insecure_tls_config().is_ok());
    }

    #[test]
    fn no_completo_com_token_do_cofre_de_teste() {
        crate::runner::TEST_VAULT.with(|v| {
            v.borrow_mut()
                .insert("zbx".into(), "tok-SEGREDO-123".into())
        });
        let (url, seen) = fake_zabbix(zabbix7);
        let id = |t: &str| t.to_string();
        let cfg = json!({"url": url, "token_secret": "zbx", "operation": "hosts_unavailable"});
        let out: Vec<Value> = serde_json::from_str(&run(&cfg, &id).unwrap()).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["host"], "olt-exemplo");
        assert_eq!(out[0]["desde"], 1_795_567_712);
        let cfg = json!({"url": url, "token_secret": "zbx", "auth_mode": "auth_field",
                         "operation": "raw", "method": "host.get", "params": "{\"output\":[\"host\"]}"});
        assert!(run(&cfg, &id).unwrap().contains("switch-snmp"));
        let seen = seen.lock().unwrap();
        let (head, body) = seen.last().unwrap();
        assert_eq!(body["auth"], "tok-SEGREDO-123");
        assert!(!head.contains("authorization:"));
        assert_eq!(body["params"], json!({"output": ["host"]}));
    }

    #[test]
    fn token_so_de_segredo() {
        let id = |t: &str| t.to_string();
        assert!(
            token(&json!({"token": "abc123"}), &id).is_err(),
            "texto puro recusado"
        );
        assert!(token(&json!({}), &id).is_err());
        assert_eq!(severities(&json!({"min_severity": 4})), Some(vec![4, 5]));
        assert_eq!(
            severities(&json!({"severities": ["3", 5]})),
            Some(vec![3, 5])
        );
        assert_eq!(severities(&json!({})), None);
    }
}
