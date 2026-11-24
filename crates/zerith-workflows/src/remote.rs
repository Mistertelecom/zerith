//! Lado cliente do `zerith server` (API HTTP de workflows num servidor
//! cadastrado): código de pareamento, tipos das rotas e cliente `ureq`.
//! Fica aqui, e não no `zerith-cli` (binário), para o desktop reaproveitar.
//! Veja `docs/servidor.md`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const PAIR_PREFIX: &str = "zerith-pair:";

/// Conteúdo do código de pareamento. Carrega o token: trate como senha.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    pub v: u32,
    pub name: String,
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for PairingCode {
    // Nunca imprime o token, nem em `{:?}` de log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("v", &self.v)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("token", &"***")
            .finish()
    }
}

impl PairingCode {
    /// `zerith-pair:` + base64url (sem `=`) do JSON `{v:1,name,url,token}`.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!("{PAIR_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Aceita espaços em volta (colado de terminal). Só versão 1 e URL http(s).
    pub fn decode(code: &str) -> Result<Self, String> {
        let b64 = code
            .trim()
            .strip_prefix(PAIR_PREFIX)
            .ok_or("código de pareamento deve começar com 'zerith-pair:'")?;
        let bytes = URL_SAFE_NO_PAD
            .decode(b64.trim_end_matches('='))
            .map_err(|_| "código de pareamento corrompido (base64)")?;
        let pc: Self =
            serde_json::from_slice(&bytes).map_err(|_| "código de pareamento corrompido (json)")?;
        if pc.v != 1 {
            return Err(format!("versão {} do código não suportada", pc.v));
        }
        if !(pc.url.starts_with("http://") || pc.url.starts_with("https://")) {
            return Err("URL do código deve ser http:// ou https://".into());
        }
        if pc.token.is_empty() {
            return Err("código sem token".into());
        }
        Ok(pc)
    }
}

// ------------------------------------------------------------ tipos das rotas

/// `GET /v1/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub name: String,
    pub version: String,
    pub hostname: String,
    pub uptime_secs: u64,
    /// Linha da tabela `daemon_status` (se o daemon gravar uma), como JSON.
    #[serde(default)]
    pub daemon: Option<serde_json::Value>,
    pub workflows_total: usize,
    pub workflows_enabled: usize,
    /// Workflows cuja última execução terminou em erro.
    pub failing_workflows: usize,
    /// Execuções disparadas por esta API ainda rodando.
    pub runs_active: usize,
}

/// Corpo de `PUT /v1/workflows/:id`. `base_updated_at_ms` é o
/// `updated_at_ms` que o cliente viu por último; `None` = só criar. Se o
/// servidor tiver versão diferente, responde 409 com a atual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutWorkflow {
    pub workflow: crate::Workflow,
    #[serde(default)]
    pub base_updated_at_ms: Option<i64>,
}

/// Resposta de `POST /v1/workflows/:id/run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStarted {
    pub run_id: String,
}

// ------------------------------------------------------------ cliente

#[derive(Debug)]
pub enum RemoteError {
    /// 409 no PUT: o servidor tem outra versão (vem junto, para mesclar).
    Conflict(Box<crate::Workflow>),
    /// Resposta HTTP de erro (mensagem do campo `error`, nunca o token).
    Status(u16, String),
    /// Rede, DNS, TLS do proxy, timeout ou JSON inesperado.
    Transport(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(_) => write!(f, "o servidor tem uma versão mais nova do workflow"),
            Self::Status(401, _) => {
                write!(f, "token recusado (revogado ou errado); pareie de novo")
            }
            Self::Status(c, m) => write!(f, "servidor respondeu {c}: {m}"),
            Self::Transport(m) => write!(f, "sem conexão com o servidor: {m}"),
        }
    }
}

impl std::error::Error for RemoteError {}

pub type RemoteResult<T> = std::result::Result<T, RemoteError>;

/// Cliente síncrono da API (`ureq`). Rode fora da thread de UI.
pub struct RemoteClient {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl RemoteClient {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            base: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(30))
                .redirects(0)
                .build(),
        }
    }

    pub fn from_pairing(pc: &PairingCode) -> Self {
        Self::new(&pc.url, &pc.token)
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}/v1{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
    }

    fn send<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> RemoteResult<T> {
        let r = self.req(method, path);
        let res = match body {
            Some(b) => r.send_json(b),
            None => r.call(),
        };
        match res {
            Ok(resp) => resp
                .into_json()
                .map_err(|e| RemoteError::Transport(e.to_string())),
            Err(ureq::Error::Status(code, resp)) => {
                let v: serde_json::Value = resp.into_json().unwrap_or_default();
                if code == 409 {
                    if let Ok(cur) = serde_json::from_value(v["current"].clone()) {
                        return Err(RemoteError::Conflict(Box::new(cur)));
                    }
                }
                let msg = v["error"].as_str().unwrap_or("").to_string();
                Err(RemoteError::Status(code, msg))
            }
            Err(e) => Err(RemoteError::Transport(e.to_string())),
        }
    }

    /// Sem token: confirma que é um Zerith e devolve a versão.
    pub fn ping(&self) -> RemoteResult<String> {
        let v: serde_json::Value = self
            .agent
            .get(&format!("{}/v1/ping", self.base))
            .call()
            .map_err(|e| RemoteError::Transport(e.to_string()))?
            .into_json()
            .map_err(|e| RemoteError::Transport(e.to_string()))?;
        if v["name"] != "zerith" {
            return Err(RemoteError::Transport(
                "o endereço não é um servidor Zerith".into(),
            ));
        }
        Ok(v["version"].as_str().unwrap_or("").to_string())
    }

    pub fn health(&self) -> RemoteResult<Health> {
        self.send("GET", "/health", None)
    }

    pub fn list(&self) -> RemoteResult<Vec<crate::Workflow>> {
        self.send("GET", "/workflows", None)
    }

    pub fn get(&self, id: &str) -> RemoteResult<crate::Workflow> {
        self.send("GET", &format!("/workflows/{id}"), None)
    }

    /// `base_updated_at_ms`: o `updated_at_ms` visto por último (`None` = criar).
    pub fn put(
        &self,
        wf: &crate::Workflow,
        base_updated_at_ms: Option<i64>,
    ) -> RemoteResult<crate::Workflow> {
        let body = PutWorkflow {
            workflow: wf.clone(),
            base_updated_at_ms,
        };
        let body = serde_json::to_value(body).map_err(|e| RemoteError::Transport(e.to_string()))?;
        self.send("PUT", &format!("/workflows/{}", wf.id), Some(body))
    }

    pub fn delete(&self, id: &str) -> RemoteResult<()> {
        self.send::<serde_json::Value>("DELETE", &format!("/workflows/{id}"), None)
            .map(|_| ())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> RemoteResult<crate::Workflow> {
        self.send(
            "POST",
            &format!("/workflows/{id}/enabled"),
            Some(serde_json::json!({ "enabled": enabled })),
        )
    }

    pub fn run(&self, id: &str, input: Option<&str>) -> RemoteResult<String> {
        let r: RunStarted = self.send(
            "POST",
            &format!("/workflows/{id}/run"),
            Some(serde_json::json!({ "input": input })),
        )?;
        Ok(r.run_id)
    }

    pub fn runs(&self, id: &str, limit: usize) -> RemoteResult<Vec<crate::WorkflowRun>> {
        self.send("GET", &format!("/workflows/{id}/runs?limit={limit}"), None)
    }

    pub fn run_get(&self, run_id: &str) -> RemoteResult<crate::WorkflowRun> {
        self.send("GET", &format!("/runs/{run_id}"), None)
    }

    pub fn secret_names(&self) -> RemoteResult<Vec<String>> {
        self.send("GET", "/secrets", None)
    }

    /// Só escrita: o servidor nunca devolve o valor.
    pub fn secret_set(&self, name: &str, value: &str) -> RemoteResult<()> {
        self.send::<serde_json::Value>(
            "PUT",
            &format!("/secrets/{name}"),
            Some(serde_json::json!({ "value": value })),
        )
        .map(|_| ())
    }

    pub fn secret_delete(&self, name: &str) -> RemoteResult<()> {
        self.send::<serde_json::Value>("DELETE", &format!("/secrets/{name}"), None)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codigo_de_pareamento_ida_e_volta() {
        let pc = PairingCode {
            v: 1,
            name: "servidor-casa".into(),
            url: "http://100.64.1.2:7878".into(),
            token: "zt_abc".into(),
        };
        let code = pc.encode();
        assert!(code.starts_with("zerith-pair:") && !code.contains('='));
        assert_eq!(PairingCode::decode(&format!("  {code}\n")).unwrap(), pc);
        assert!(!format!("{pc:?}").contains("zt_abc"));
        assert!(PairingCode::decode("zerith-pair:!!").is_err());
        assert!(PairingCode::decode(&code[1..]).is_err());
        let bad = PairingCode {
            url: "file:///etc".into(),
            ..pc
        };
        assert!(PairingCode::decode(&bad.encode()).is_err());
    }
}
