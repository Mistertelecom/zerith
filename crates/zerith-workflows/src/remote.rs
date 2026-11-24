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
