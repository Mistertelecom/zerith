# Login OAuth (Claude Code, Codex, Antigravity) — Plano de Implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Conectar contas Claude Code, Codex e Antigravity por OAuth (import de credencial local + PKCE no navegador), sem quebrar o caminho de chave de API.

**Architecture:** `AuthMode` novo em `zerith-providers` escolhe como o header é montado; `oauth.rs` faz PKCE, listener loopback, troca de código e refresh; o cofre do SO guarda o JSON da credencial; o runtime hidrata `ProviderConfig.oauth` ao montar o candidato de conta. Codex e Antigravity ganham transporte próprio porque falam Responses e `v1internal`, não `chat/completions`.

**Tech Stack:** Rust 2021, `ureq` 2 (bloqueante), `ring` 0.17 (SHA-256 + random, já na árvore), `base64` 0.22 (já na árvore), `keyring` 3.6, SQLite via `rusqlite`. Sem tokio, sem crate novo.

**Spec:** `docs/superpowers/specs/2026-11-16-oauth-login-e-redesign-design.md`

## Global Constraints

- Nenhum token, `code_verifier` ou `refresh_token` em `settings.toml`, log, erro, `Debug` de struct pública ou saída de terminal. Máscara só via `zerith_core::secrets::mask`.
- `auth` ausente em `AccountEntry`/`ProviderEntry` = `ApiKey` = comportamento de hoje, byte a byte. Sem migração de settings.
- Nenhuma dependência nova no workspace: `ring`, `base64`, `ureq`, `serde_json`, `keyring` já estão lá.
- Rede é bloqueante e roda em thread do host; nada de runtime async.
- `cargo test -p zerith-providers` e `cargo test -p zerith-core` verdes ao fim de cada task.
- Commits por arquivo (`git commit -- <path>`), nunca `git add -A`: o repo tem dezenas de arquivos modificados de sessões anteriores.
- Texto de UI e mensagens de CLI em pt-BR, como o resto do projeto.

## Review Focus

1. **Estado errado no callback** — o listener recebe `?error=access_denied` do usuário (ou de um site aberto no mesmo navegador): deve responder 400 com mensagem curta e não gravar nada, sem panicar nem travar a porta.
2. **Porta ocupada (Codex, 1455 fixa)** — o CLI Codex pode estar aberto; o login deve falhar com erro legível ("porta 1455 em uso, feche o Codex") em vez de pendurar esperando.
3. **Refresh token ausente ou negado** — Google/Anthropic devolvem `refresh_token` só na primeira autorização: uma credencial sem `refresh_token` que expira não pode ser tratada como válida em silêncio; cai em cooldown + failover com mensagem que diz "relogue".
4. **Relógio local adiantado/atrasado** — `expires_at` vem do servidor (`expires_in`); a margem de renovação (300 s) não pode fazer loop de refresh quando `expires_in` for menor que a margem.
5. **`state` reutilizado / requisição sem `state`** — sem comparação em tempo constante e sem exigir `state` não-nulo, qualquer página aberta no navegador completa o login.

---

## File Structure

| Arquivo | Responsabilidade |
|---|---|
| `crates/zerith-providers/src/oauth.rs` (novo) | PKCE, tabela dos três provedores, `login`/`import_local`/`refresh`, listener loopback, `OAuthCredential`. Não conhece cofre nem settings. |
| `crates/zerith-providers/src/codex.rs` (novo) | Transporte Responses (`POST /responses`), parse de SSE. |
| `crates/zerith-providers/src/antigravity.rs` (novo) | Transporte `v1internal:generateContent` + `loadCodeAssist`. |
| `crates/zerith-providers/src/config.rs` | `AuthMode`, `Protocol::{Responses, CloudCode}`, presets. |
| `crates/zerith-providers/src/openai.rs` | `authored()` por modo, dispatch de protocolo em `complete`/`stream`. |
| `crates/zerith-core/src/settings.rs` | `AccountEntry.auth`/`oauth_tool`, `ProviderEntry.auth` + `AuthMode` de settings. |
| `crates/zerith-core/src/secrets.rs` | `save_oauth`/`load_oauth`/`clear_oauth` sobre o `SecretStore` (JSON do credential). |
| `crates/zerith-runtime/src/lib.rs` | Hidrata `ProviderConfig.oauth` e renova antes de montar o candidato. |
| `crates/zerith-cli/src/cmds.rs` + `main.rs` | Subcomando `zerith login`. |
| `crates/zerith-ui/src/screens/providers/` | Botão "Entrar com OAuth" + estado da credencial. |
| `docs/oauth.md` (novo), `docs/providers.md` | Documentação. |

---

## Task 1: `AuthMode` e header OAuth do Anthropic

**Files:**
- Modify: `crates/zerith-providers/src/config.rs`
- Modify: `crates/zerith-providers/src/openai.rs:99-125` (`authored`, `ensure_key`)
- Test: `crates/zerith-providers/src/openai.rs` (`mod tests` no fim do arquivo)

**Interfaces:**
- Consumes: nada de tasks anteriores.
- Produces: `config::AuthMode { ApiKey, Oauth }` (`Default = ApiKey`), `config::OAuthCredential { access_token, refresh_token: Option<String>, expires_at: i64, account_id: Option<String>, tool: OAuthTool }`, `config::OAuthTool { ClaudeCode, Codex, Antigravity }`; `ProviderConfig.auth: AuthMode`, `ProviderConfig.oauth: Option<OAuthCredential>`.

- [ ] **Step 1: Write the failing test**

Em `crates/zerith-providers/src/openai.rs`, no `mod tests` existente (se não houver, criar no fim do arquivo):

```rust
#[test]
fn oauth_mode_sends_bearer_and_the_anthropic_beta_header() {
    let mut cfg = ProviderConfig::new(ProviderKind::Anthropic);
    cfg.base_url = "http://127.0.0.1:9".into();
    cfg.auth = crate::config::AuthMode::Oauth;
    cfg.oauth = Some(crate::config::OAuthCredential {
        access_token: "tok-123".into(),
        refresh_token: Some("ref-456".into()),
        expires_at: 4_000_000_000,
        account_id: None,
        tool: crate::config::OAuthTool::ClaudeCode,
    });
    let client = Client::new(cfg).unwrap();
    // `headers()` é um seam de teste: devolve o que `authored` aplicaria.
    let h = client.headers();
    assert_eq!(h.get("authorization").map(String::as_str), Some("Bearer tok-123"));
    assert_eq!(h.get("anthropic-beta").map(String::as_str), Some("oauth-2025-04-20"));
    assert_eq!(h.get("x-api-key"), None);
}

#[test]
fn api_key_mode_keeps_x_api_key_and_no_beta_header() {
    let mut cfg = ProviderConfig::new(ProviderKind::Anthropic);
    cfg.base_url = "http://127.0.0.1:9".into();
    cfg.api_key = Some("sk-ant-1".into());
    let h = Client::new(cfg).unwrap().headers();
    assert_eq!(h.get("x-api-key").map(String::as_str), Some("sk-ant-1"));
    assert_eq!(h.get("authorization"), None);
    assert_eq!(h.get("anthropic-beta"), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/zerith" && cargo test -p zerith-providers oauth_mode_sends_bearer 2>&1 | tail -20`
Expected: FAIL — `AuthMode` não existe (`error[E0433]/E0609`).

- [ ] **Step 3: Write minimal implementation**

`crates/zerith-providers/src/config.rs`, depois de `Protocol`:

```rust
/// How the request proves who it is. `ApiKey` is the historical path; `Oauth`
/// swaps the credential header (and, for Anthropic, adds the beta header the
/// subscription endpoint requires).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    #[default]
    ApiKey,
    Oauth,
}

/// Which CLI the credential belongs to. Endpoints and client ids live in
/// `oauth::PROVIDERS`, keyed by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthTool {
    ClaudeCode,
    Codex,
    Antigravity,
}

impl OAuthTool {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "anthropic" => Some(Self::ClaudeCode),
            "codex" | "openai" => Some(Self::Codex),
            "antigravity" | "google" => Some(Self::Antigravity),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }
}

/// What the cofre stores. `Debug` is hand-written: a derived one would print
/// the tokens into any `{:?}` of a config.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds, from the server's `expires_in`.
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub tool: OAuthTool,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("access_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("tool", &self.tool)
            .finish()
    }
}

impl OAuthCredential {
    /// True when the token is worth sending. The skew keeps a token from
    /// expiring mid-stream; it never exceeds half the remaining life.
    pub fn needs_refresh(&self, now: i64) -> bool {
        const SKEW: i64 = 300;
        self.expires_at - now < SKEW
    }
}
```

Em `ProviderConfig`, dois campos:

```rust
    pub auth: AuthMode,
    #[serde(skip_serializing)]
    pub oauth: Option<OAuthCredential>,
```

e no `Default`: `auth: AuthMode::ApiKey, oauth: None`. `new()` herda pelo `..Default::default()` que já existe.

Em `openai.rs`, `authored` passa a delegar e ganha o seam de teste:

```rust
    /// Applies whichever credential the protocol expects. The key never reaches
    /// a log or an error string.
    fn authored(&self, mut req: ureq::Request) -> ureq::Request {
        for (name, value) in self.headers() {
            req = req.set(&name, &value);
        }
        req
    }

    /// Exactly the headers auth contributes, lowercase-keyed. Separate from
    /// `authored` so tests can assert them without a socket.
    fn headers(&self) -> std::collections::BTreeMap<String, String> {
        let mut h = std::collections::BTreeMap::new();
        if self.cfg.protocol == Protocol::Anthropic {
            h.insert("anthropic-version".into(), "2023-06-01".into());
        }
        match (self.cfg.auth, &self.cfg.oauth) {
            (AuthMode::Oauth, Some(cred)) => {
                h.insert("authorization".into(), format!("Bearer {}", cred.access_token));
                if cred.tool == OAuthTool::ClaudeCode {
                    h.insert("anthropic-beta".into(), "oauth-2025-04-20".into());
                }
            }
            _ => {
                if let Some(key) = &self.cfg.api_key {
                    match self.cfg.protocol {
                        Protocol::OpenAi => {
                            h.insert("authorization".into(), format!("Bearer {key}"));
                        }
                        Protocol::Anthropic => {
                            h.insert("x-api-key".into(), key.clone());
                        }
                    }
                }
            }
        }
        h
    }
```

`ensure_key` aceita credencial OAuth:

```rust
    fn ensure_key(&self) -> Result<()> {
        let has_oauth = self.cfg.auth == AuthMode::Oauth && self.cfg.oauth.is_some();
        if self.cfg.kind.needs_key() && !has_oauth && self.cfg.api_key.as_deref().unwrap_or("").is_empty() {
            return Err(ProviderError::NoKey);
        }
        Ok(())
    }
```

Imports em `openai.rs`: `use crate::config::{AuthMode, OAuthTool, Protocol, ProviderConfig};` e `use std::collections::BTreeMap;` (ou qualificar inline como acima).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-providers 2>&1 | tail -20`
Expected: PASS, incluindo os dois testes novos e os 38 existentes.

- [ ] **Step 5: Commit**

```bash
git commit -m "providers: AuthMode e header OAuth do Anthropic" -- crates/zerith-providers/src/config.rs crates/zerith-providers/src/openai.rs
```

---

## Task 2: PKCE + listener loopback + troca de código e refresh (`oauth.rs`)

**Files:**
- Create: `crates/zerith-providers/src/oauth.rs`
- Modify: `crates/zerith-providers/src/lib.rs:11-19` (registrar `pub mod oauth;` e reexportar)
- Modify: `crates/zerith-providers/Cargo.toml` (declarar `ring`, `base64`)
- Test: `crates/zerith-providers/src/oauth.rs` (`mod tests` no fim)

**Interfaces:**
- Consumes: `AuthMode`, `OAuthCredential`, `OAuthTool` da Task 1.
- Produces:
  - `oauth::Provider { client_id: &'static str, authorize_url, token_url, scopes: &'static [&'static str], redirect: Redirect, refresh_encoding: Encoding, extra_authorize: &'static [(&'static str, &'static str)] }`
  - `oauth::Redirect { Port: u16, Path: &'static str }` (`Port::Fixed(1455)` para o Codex, `Port::Ephemeral` para os outros)
  - `oauth::Pkce { verifier: String, challenge: String }` com `Pkce::generate()`
  - `oauth::authorize_url(tool: OAuthTool, pkce: &Pkce, state: &str) -> (String, String)` (devolve URL e o redirect completo)
  - `oauth::wait_for_code(redirect: &Redirect, expect_state: &str) -> Result<String, OAuthError>` (bloqueante; o teste injeta o `redirect` em vez de abrir navegador)
  - `oauth::exchange(tool: OAuthTool, code: &str, verifier: &str, redirect: &str) -> Result<OAuthCredential, OAuthError>`
  - `oauth::refresh(tool: OAuthTool, cred: &OAuthCredential) -> Result<OAuthCredential, OAuthError>`
  - `oauth::import_local(tool: OAuthTool, home: &std::path::Path) -> Option<OAuthCredential>`
  - `oauth::open_browser(url: &str) -> std::io::Result<()>`
  - `OAuthError { Denied(String), State(String), Timeout, Port(u16), Http(u16, String), Parse(String), NoRefreshToken, Io(String) }`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636, Appendix B: the verifier and its S256 challenge.
    #[test]
    fn pkce_matches_the_rfc_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_verifier_is_url_safe_and_long_enough() {
        let p = Pkce::generate();
        assert!((43..=128).contains(&p.verifier.len()));
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn authorize_url_carries_pkce_state_and_scopes() {
        let p = Pkce::from_verifier("v");
        let (url, redirect) = authorize_url(OAuthTool::ClaudeCode, &p, "st-1");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st-1"));
        assert!(url.contains("user%3Ainference") || url.contains("user:inference"));
        assert!(redirect.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn listener_returns_the_code_once_and_ignores_a_wrong_state() {
        let p = Pkce::from_verifier("v");
        let (_, redirect) = authorize_url(OAuthTool::ClaudeCode, &p, "st-1");
        let red = Redirect::parse(&redirect).unwrap();
        let port = red.port();
        let handle = std::thread::spawn(move || wait_for_code(&red, "st-1"));
        // wrong state first: must not satisfy the wait
        let _ = ureq::get(&format!("http://127.0.0.1:{port}/callback?code=bad&state=nope"))
            .timeout(std::time::Duration::from_secs(2))
            .call();
        let ok = ureq::get(&format!("http://127.0.0.1:{port}/callback?code=good&state=st-1"))
            .timeout(std::time::Duration::from_secs(2))
            .call();
        assert!(ok.is_ok());
        assert_eq!(handle.join().unwrap().unwrap(), "good");
    }

    #[test]
    fn denied_callback_is_an_error_not_a_code() {
        let p = Pkce::from_verifier("v");
        let (_, redirect) = authorize_url(OAuthTool::ClaudeCode, &p, "st-2");
        let red = Redirect::parse(&redirect).unwrap();
        let port = red.port();
        let handle = std::thread::spawn(move || wait_for_code(&red, "st-2"));
        let _ = ureq::get(&format!("http://127.0.0.1:{port}/callback?error=access_denied&state=st-2")).call();
        assert!(matches!(handle.join().unwrap(), Err(OAuthError::Denied(_))));
    }

    #[test]
    fn refresh_tolerates_a_token_that_expires_inside_the_skew() {
        let cred = OAuthCredential {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: 100,
            account_id: None,
            tool: OAuthTool::ClaudeCode,
        };
        // 240s of life left, skew 300: must still not loop forever — refresh once.
        assert!(cred.needs_refresh(100 - 240));
    }

    #[test]
    fn token_response_parses_json_and_form_encodings() {
        let now = 1_700_000_000;
        let json = r#"{"access_token":"A","refresh_token":"R","expires_in":3600}"#;
        let c = credential_from_body(json, Encoding::Json, OAuthTool::ClaudeCode, now).unwrap();
        assert_eq!(c.access_token, "A");
        assert_eq!(c.expires_at, now + 3600);
        assert_eq!(c.refresh_token.as_deref(), Some("R"));

        let form = "access_token=A&refresh_token=R&expires_in=60";
        let c = credential_from_body(form, Encoding::Form, OAuthTool::Codex, now).unwrap();
        assert_eq!(c.tool, OAuthTool::Codex);
        assert_eq!(c.expires_at, now + 60);
    }

    #[test]
    fn a_token_without_refresh_token_keeps_the_previous_one() {
        let now = 0;
        let old = OAuthCredential {
            access_token: "old".into(),
            refresh_token: Some("keep".into()),
            expires_at: 10,
            account_id: None,
            tool: OAuthTool::Antigravity,
        };
        let body = r#"{"access_token":"new","expires_in":3600}"#;
        let c = credential_from_body_keeping(body, Encoding::Json, &old, now).unwrap();
        assert_eq!(c.access_token, "new");
        assert_eq!(c.refresh_token.as_deref(), Some("keep"));
    }

    #[test]
    fn importing_codex_credentials_reads_the_local_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"AT","refresh_token":"RT","account_id":"acc_1"},"last_refresh":"2026-11-08T00:00:00Z"}"#,
        )
        .unwrap();
        let c = import_codex(dir.path()).expect("deve importar");
        assert_eq!(c.access_token, "AT");
        assert_eq!(c.account_id.as_deref(), Some("acc_1"));
    }

    #[test]
    fn importing_claude_credentials_ignores_a_file_with_only_mcp_tokens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".credentials.json"), r#"{"mcpOAuth":{}}"#).unwrap();
        assert!(import_claude(dir.path()).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p zerith-providers oauth 2>&1 | tail -20`
Expected: FAIL — `error[E0583]: file not found for module oauth` ou `cannot find function`.

- [ ] **Step 3: Write minimal implementation**

`crates/zerith-providers/Cargo.toml`:

```toml
ring = "0.17"
base64 = "0.22"
```

`crates/zerith-providers/src/oauth.rs`:

```rust
//! OAuth for the CLIs the user already pays for: Claude Code, Codex, Antigravity.
//!
//! Two ways in: import the credential a CLI already wrote to disk, or run the
//! loopback PKCE flow in the browser. Endpoints and client ids are the ones
//! those CLIs themselves use — no invented client.
//!
//! Security: this module never logs or stores a token; it returns
//! `OAuthCredential` and the caller puts it in the OS cofre.

use crate::config::{OAuthCredential, OAuthTool};
use base64::Engine as _;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("o provedor recusou: {0}")]
    Denied(String),
    #[error("state não confere")]
    State(String),
    #[error("o navegador não voltou a tempo")]
    Timeout,
    #[error("porta {0} em uso; feche o outro processo e tente de novo")]
    Port(u16),
    #[error("http {0}: {1}")]
    Http(u16, String),
    #[error("resposta inesperada: {0}")]
    Parse(String),
    #[error("credencial sem refresh_token; rode `zerith login` de novo")]
    NoRefreshToken,
    #[error("io: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, OAuthError>;

pub enum Redirect {
    Ephemeral,
    Fixed(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Json,
    Form,
}

pub struct Provider {
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub redirect: Redirect,
    pub refresh_encoding: Encoding,
    /// Sent on the authorize URL only, verbatim.
    pub extra_authorize: &'static [(&'static str, &'static str)],
}

const CLAUDE: Provider = Provider {
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://api.anthropic.com/v1/oauth/token",
    scopes: &["org:create_api_key", "user:profile", "user:inference"],
    redirect: Redirect::Ephemeral,
    refresh_encoding: Encoding::Json,
    extra_authorize: &[],
};

const CODEX: Provider = Provider {
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scopes: &["openid", "profile", "email", "offline_access"],
    redirect: Redirect::Fixed(1455),
    refresh_encoding: Encoding::Form,
    extra_authorize: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "codex_cli_rs"),
    ],
};

const ANTIGRAVITY: Provider = Provider {
    // ponytail: Google exige client cadastrado; o id do CLI oficial não é
    // público. Enquanto não houver um, o fluxo usa o client do gcloud/installed
    // app e o login completo fica atrás da flag `oauth.antigravity`.
    client_id: "",
    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    scopes: &[
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ],
    redirect: Redirect::Ephemeral,
    refresh_encoding: Encoding::Form,
    extra_authorize: &[("access_type", "offline"), ("prompt", "consent")],
};

pub fn provider(tool: OAuthTool) -> &'static Provider {
    match tool {
        OAuthTool::ClaudeCode => &CLAUDE,
        OAuthTool::Codex => &CODEX,
        OAuthTool::Antigravity => &ANTIGRAVITY,
    }
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Pkce {
    pub fn from_verifier(v: &str) -> Self {
        let challenge = b64url(digest(&SHA256, v.as_bytes()).as_ref());
        Self { verifier: v.to_string(), challenge }
    }

    pub fn generate() -> Self {
        let mut raw = [0u8; 32];
        SystemRandom::new()
            .fill(&mut raw)
            .expect("SystemRandom não falha em plataformas suportadas");
        Self::from_verifier(&b64url(&raw))
    }
}

fn random_state() -> String {
    let mut raw = [0u8; 16];
    SystemRandom::new().fill(&mut raw).expect("SystemRandom");
    b64url(&raw)
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn redirect_path(tool: OAuthTool) -> &'static str {
    match tool {
        OAuthTool::Codex => "/auth/callback",
        _ => "/callback",
    }
}

pub fn redirect_uri(tool: OAuthTool, port: u16) -> String {
    format!("http://127.0.0.1:{port}{}", redirect_path(tool))
}

/// The URL to open plus the `Redirect` the listener will use.
pub fn authorize_url(tool: OAuthTool, pkce: &Pkce, state: &str) -> (String, Redirect) {
    let p = provider(tool);
    (authorize_url_for(tool, p, pkce, state, 0), p.redirect.clone())
}

fn authorize_url_for(tool: OAuthTool, p: &Provider, pkce: &Pkce, state: &str, port: u16) -> String {
    let mut url = format!(
        "{}?client_id={}&response_type=code&code_challenge={}&code_challenge_method=S256&state={}&redirect_uri={}",
        p.authorize_url,
        urlencode(p.client_id),
        urlencode(&pkce.challenge),
        urlencode(state),
        urlencode(&redirect_uri(tool, port)),
    );
    let scope = p.scopes.join(" ");
    url.push_str("&scope=");
    url.push_str(&urlencode(&scope));
    for (k, v) in p.extra_authorize {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencode(v));
    }
    url
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => { out.push(b' '); i += 1 }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                i += 3;
            }
            c => { out.push(c); i += 1 }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| urldecode(v))
    })
}

/// Binds the loopback socket. Called by the CLI *before* opening the browser so
/// a busy fixed port fails loudly, not after the user already consented.
pub fn bind(tool: OAuthTool) -> Result<(TcpListener, u16)> {
    let listener = match provider(tool).redirect {
        Redirect::Ephemeral => TcpListener::bind("127.0.0.1:0").map_err(|e| OAuthError::Io(e.to_string()))?,
        Redirect::Fixed(port) => TcpListener::bind(("127.0.0.1", port)).map_err(|_| OAuthError::Port(port))?,
    };
    let port = listener.local_addr().map_err(|e| OAuthError::Io(e.to_string()))?.port();
    Ok((listener, port))
}

/// Blocks until the browser hits the callback. Accepts one *valid* callback:
/// a wrong `state` or a stray request gets a 400 and the loop keeps waiting.
pub fn wait_for_code(listener: TcpListener, path: &str, expect_state: &str) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => return Err(OAuthError::Io(e.to_string())),
        };
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
        }
        let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let (route, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        if route != path {
            respond(&mut stream, 404, "not here");
            continue;
        }
        if let Some(err) = query_param(query, "error") {
            respond(&mut stream, 400, "login recusado; pode fechar esta aba");
            return Err(OAuthError::Denied(err));
        }
        match query_param(query, "state") {
            Some(s) if constant_eq(&s, expect_state) => {}
            _ => {
                respond(&mut stream, 400, "state inválido");
                continue;
            }
        }
        let Some(code) = query_param(query, "code") else {
            respond(&mut stream, 400, "sem code");
            continue;
        };
        respond(&mut stream, 200, "login concluído; pode fechar esta aba");
        return Ok(code);
    }
    Err(OAuthError::Timeout)
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<String> {
    let body: String = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let resp = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build()
        .post(url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .send_string(&body);
    read_body(resp)
}

fn post_json(url: &str, body: serde_json::Value) -> Result<String> {
    let resp = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build()
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&body.to_string());
    read_body(resp)
}

fn read_body(resp: std::result::Result<ureq::Response, ureq::Error>) -> Result<String> {
    match resp {
        Ok(r) => r.into_string().map_err(|e| OAuthError::Io(e.to_string())),
        Err(ureq::Error::Status(code, r)) => Err(OAuthError::Http(code, r.into_string().unwrap_or_default())),
        Err(e) => Err(OAuthError::Io(e.to_string())),
    }
}

pub fn exchange(tool: OAuthTool, code: &str, verifier: &str, redirect: &str) -> Result<OAuthCredential> {
    let p = provider(tool);
    let body = match tool {
        OAuthTool::ClaudeCode => post_json(
            p.token_url,
            serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "code_verifier": verifier,
                "redirect_uri": redirect,
                "client_id": p.client_id,
            }),
        )?,
        _ => post_form(
            p.token_url,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("client_id", p.client_id),
            ],
        )?,
    };
    credential_from_body(&body, p.refresh_encoding, tool, now_secs())
}

pub fn refresh(tool: OAuthTool, cred: &OAuthCredential) -> Result<OAuthCredential> {
    let p = provider(tool);
    let refresh_token = cred.refresh_token.as_deref().ok_or(OAuthError::NoRefreshToken)?;
    let body = match p.refresh_encoding {
        Encoding::Json => post_json(
            p.token_url,
            serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": p.client_id,
            }),
        )?,
        Encoding::Form => post_form(
            p.token_url,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", p.client_id),
            ],
        )?,
    };
    credential_from_body_keeping(&body, p.refresh_encoding, cred, now_secs())
}

pub fn credential_from_body(body: &str, enc: Encoding, tool: OAuthTool, now: i64) -> Result<OAuthCredential> {
    let mut out = parse_token(body, enc)?;
    out.tool = tool;
    out.expires_at = now + out.expires_at; // parse_token devolveu `expires_in`
    Ok(out)
}

fn credential_from_body_keeping(body: &str, enc: Encoding, old: &OAuthCredential, now: i64) -> Result<OAuthCredential> {
    let mut out = parse_token(body, enc)?;
    if out.refresh_token.is_none() {
        out.refresh_token = old.refresh_token.clone();
    }
    if out.account_id.is_none() {
        out.account_id = old.account_id.clone();
    }
    out.tool = old.tool;
    out.expires_at = now + out.expires_at;
    Ok(out)
}

/// `expires_at` carries `expires_in` until the caller adds `now`.
fn parse_token(body: &str, enc: Encoding) -> Result<OAuthCredential> {
    let (access, refresh, expires_in) = match enc {
        Encoding::Json => {
            let v: serde_json::Value =
                serde_json::from_str(body).map_err(|e| OAuthError::Parse(e.to_string()))?;
            (
                v["access_token"].as_str().unwrap_or_default().to_string(),
                v["refresh_token"].as_str().map(str::to_string),
                v["expires_in"].as_i64().unwrap_or(3600),
            )
        }
        Encoding::Form => (
            query_param(body, "access_token").unwrap_or_default(),
            query_param(body, "refresh_token"),
            query_param(body, "expires_in").and_then(|s| s.parse().ok()).unwrap_or(3600),
        ),
    };
    if access.is_empty() {
        return Err(OAuthError::Parse(body.chars().take(200).collect()));
    }
    Ok(OAuthCredential {
        access_token: access,
        refresh_token: refresh,
        expires_at: expires_in,
        account_id: None,
        tool: OAuthTool::ClaudeCode,
    })
}

pub fn open_browser(url: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url);
    // ponytail: sem wait — o navegador não precisa terminar. Se não abrir, o
    // chamador ainda imprime a URL para colar à mão.
    cmd.spawn().map(|_| ())
}

/// Credential a CLI already wrote to disk, if any. Read-only: never rewrites
/// another tool's file.
pub fn import_local(tool: OAuthTool, home: &std::path::Path) -> Option<OAuthCredential> {
    match tool {
        OAuthTool::Codex => import_codex(home),
        OAuthTool::ClaudeCode => import_claude(home),
        OAuthTool::Antigravity => None, // token em DPAPI; só o fluxo próprio
    }
}

fn import_codex(home: &std::path::Path) -> Option<OAuthCredential> {
    let raw = std::fs::read_to_string(home.join(".codex/auth.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let tokens = v.get("tokens")?;
    let access = tokens.get("access_token")?.as_str()?.to_string();
    Some(OAuthCredential {
        access_token: access,
        refresh_token: tokens.get("refresh_token").and_then(|t| t.as_str()).map(str::to_string),
        // sem `expires_in` no arquivo: trata como expirado e deixa o refresh decidir
        expires_at: 0,
        account_id: tokens.get("account_id").and_then(|t| t.as_str()).map(str::to_string),
        tool: OAuthTool::Codex,
    })
}

fn import_claude(home: &std::path::Path) -> Option<OAuthCredential> {
    let raw = std::fs::read_to_string(home.join(".claude/.credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let access = oauth.get("accessToken").and_then(|t| t.as_str())?.to_string();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(|t| t.as_i64())
        .map(|ms| ms / 1000)
        .unwrap_or(0);
    Some(OAuthCredential {
        access_token: access,
        refresh_token: oauth.get("refreshToken").and_then(|t| t.as_str()).map(str::to_string),
        expires_at,
        account_id: None,
        tool: OAuthTool::ClaudeCode,
    })
}

#[cfg(test)]
mod tests { /* os testes do Step 1 */ }
```

`crates/zerith-providers/src/lib.rs`:

```rust
pub mod oauth;
pub use oauth::OAuthError;
```

E no `lib.rs` de providers, `tempfile` entra em `[dev-dependencies]`:

```toml
[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-providers oauth 2>&1 | tail -30`
Expected: PASS nos 9 testes novos.

- [ ] **Step 5: Commit**

```bash
git commit -m "providers: oauth (pkce, loopback, exchange, refresh, import)" -- crates/zerith-providers/src/oauth.rs crates/zerith-providers/src/lib.rs crates/zerith-providers/Cargo.toml Cargo.lock
```

---

## Task 3: settings ganham `auth` e `oauth_tool`

**Files:**
- Modify: `crates/zerith-core/src/settings.rs:63-107` (`AccountEntry`), `:257-279` (`ProviderEntry` + `AuthMode`)
- Test: `crates/zerith-core/src/settings.rs` (`mod tests` existente, ~`:940`)

**Interfaces:**
- Consumes: nada.
- Produces: `settings::AuthMode { ApiKey, Oauth }` (serde `lowercase`, `Default = ApiKey`); `AccountEntry.auth: AuthMode`, `AccountEntry.oauth_tool: Option<String>`; `ProviderEntry.auth: AuthMode`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn account_auth_defaults_to_api_key_and_round_trips_oauth() {
    let toml_src = r#"
[[accounts]]
id = "claude-assinatura"
provider = "anthropic"
auth = "oauth"
oauth_tool = "claude-code"
"#;
    let s: Settings = toml::from_str(toml_src).unwrap();
    assert_eq!(s.accounts[0].auth, AuthMode::Oauth);
    assert_eq!(s.accounts[0].oauth_tool.as_deref(), Some("claude-code"));

    let plain: Settings = toml::from_str(
        r#"
[[accounts]]
id = "x"
provider = "openai"
"#,
    )
    .unwrap();
    assert_eq!(plain.accounts[0].auth, AuthMode::ApiKey);
    assert!(plain.accounts[0].oauth_tool.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p zerith-core account_auth_defaults 2>&1 | tail -15`
Expected: FAIL — `no field auth`.

- [ ] **Step 3: Write minimal implementation**

Antes de `AccountEntry`:

```rust
/// Credential path for an account/provider. `ApiKey` is the historical one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    ApiKey,
    Oauth,
}
```

Em `AccountEntry`:

```rust
    /// `api_key` (default) or `oauth`. An OAuth account keeps its credential in
    /// the cofre under `<id>.oauth`, never in this file.
    #[serde(default)]
    pub auth: AuthMode,
    /// `claude-code` | `codex` | `antigravity`. Only meaningful with `auth = "oauth"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_tool: Option<String>,
```

`impl Default for AccountEntry`: `auth: AuthMode::ApiKey, oauth_tool: None`.

Em `ProviderEntry`: `#[serde(default)] pub auth: AuthMode,`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-core 2>&1 | tail -10`
Expected: PASS (o default `api_key` mantém os testes de serialização existentes).

- [ ] **Step 5: Commit**

```bash
git commit -m "settings: auth e oauth_tool por conta" -- crates/zerith-core/src/settings.rs
```

---

## Task 4: credencial OAuth no cofre + hidratação no runtime

**Files:**
- Modify: `crates/zerith-core/src/secrets.rs` (após `delete_secret`, ~`:90`)
- Modify: `crates/zerith-runtime/src/lib.rs:481-600` (`resolve_account_key`, `candidate_from_account`), `Cargo.toml` se faltar `zerith-providers` (já tem)
- Test: `crates/zerith-core/src/secrets.rs` (mem store), `crates/zerith-runtime/tests/agent_loop.rs` ou novo `crates/zerith-runtime/tests/oauth_account.rs`

**Interfaces:**
- Consumes: `OAuthCredential` (Task 1), `oauth::refresh` (Task 2), `AuthMode`/`oauth_tool` (Task 3).
- Produces:
  - `secrets::save_oauth(store, account_id, cred)`, `secrets::load_oauth(store, account_id) -> Option<OAuthCredential>`, `secrets::clear_oauth(store, account_id)`
  - `runtime::hydrate_oauth(acc: &AccountEntry, store: &dyn SecretStore) -> Option<OAuthCredential>` (renova quando `needs_refresh`)

- [ ] **Step 1: Write the failing tests**

`crates/zerith-core/src/secrets.rs`:

```rust
#[test]
fn oauth_credential_round_trips_under_the_account_entry() {
    let store = Mem::default();
    let cred = zerith_providers::OAuthCredential {
        access_token: "A".into(),
        refresh_token: Some("R".into()),
        expires_at: 42,
        account_id: None,
        tool: zerith_providers::OAuthTool::Codex,
    };
    save_oauth(&store, "codex-work", &cred).unwrap();
    let back = load_oauth(&store, "codex-work").unwrap().unwrap();
    assert_eq!(back.access_token, "A");
    assert_eq!(back.tool, zerith_providers::OAuthTool::Codex);
    clear_oauth(&store, "codex-work").unwrap();
    assert!(load_oauth(&store, "codex-work").unwrap().is_none());
}
```

`crates/zerith-runtime/tests/oauth_account.rs`:

```rust
//! An account with `auth = "oauth"` must reach the provider with a Bearer token
//! read from the cofre, and no key error.
use zerith_core::settings::{AccountEntry, AuthMode, Settings};

#[test]
fn oauth_account_sets_auth_mode_and_credential_on_the_candidate() {
    let mut s = Settings::default();
    s.accounts.push(AccountEntry {
        id: "claude-assinatura".into(),
        provider: "anthropic".into(),
        auth: AuthMode::Oauth,
        oauth_tool: Some("claude-code".into()),
        ..Default::default()
    });
    let store = zerith_core::secrets::mem_store_for_tests();
    zerith_core::secrets::save_oauth(
        store.as_ref(),
        "claude-assinatura",
        &zerith_providers::OAuthCredential {
            access_token: "tok".into(),
            refresh_token: Some("ref".into()),
            expires_at: i64::MAX / 2,
            account_id: None,
            tool: zerith_providers::OAuthTool::ClaudeCode,
        },
    )
    .unwrap();

    let cand = zerith_runtime::candidate_for_test(&s, "claude-assinatura:anthropic/claude-opus-5-5", store.as_ref());
    assert_eq!(cand.config.auth, zerith_providers::config::AuthMode::Oauth);
    assert_eq!(
        cand.config.oauth.as_ref().map(|c| c.access_token.as_str()),
        Some("tok")
    );
    assert_eq!(cand.config.api_key, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p zerith-core oauth_credential_round_trips 2>&1 | tail -10 && cargo test -p zerith-runtime oauth_account 2>&1 | tail -10`
Expected: FAIL — `save_oauth` / `candidate_for_test` não existem.

- [ ] **Step 3: Write minimal implementation**

`secrets.rs`:

```rust
/// `<account>.oauth` holds the JSON of an `OAuthCredential`. Same cofre as the
/// API keys: one place to look, one place to lock.
pub fn oauth_entry(account_id: &str) -> String {
    format!("{account_id}.oauth")
}

pub fn save_oauth(
    store: &dyn SecretStore,
    account_id: &str,
    cred: &zerith_providers::OAuthCredential,
) -> Result<(), SecretError> {
    let json = serde_json::to_string(cred)
        .map_err(|e| SecretError::Unavailable(format!("credencial serializável: {e}")))?;
    store.set(&oauth_entry(account_id), &json)
}

pub fn load_oauth(
    store: &dyn SecretStore,
    account_id: &str,
) -> Result<Option<zerith_providers::OAuthCredential>, SecretError> {
    match store.get(&oauth_entry(account_id)) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| SecretError::Unavailable(format!("credencial corrompida: {e}"))),
        Err(SecretError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn clear_oauth(store: &dyn SecretStore, account_id: &str) -> Result<(), SecretError> {
    match store.delete(&oauth_entry(account_id)) {
        Ok(()) | Err(SecretError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}
```

`zerith-core/Cargo.toml` precisa de `zerith-providers` como dependência (o crate já é dependência de `zerith-runtime`; em `zerith-core` não é hoje). Se criar ciclo, mova `OAuthCredential`/`OAuthTool`/`AuthMode` para `zerith-core` e reexporte de `zerith-providers`. Verifique com `cargo tree -p zerith-core`: `zerith-providers` não depende de `zerith-core`, então a dependência é permitida.

`runtime/src/lib.rs`, logo antes de `candidate_from_account`:

```rust
/// Reads the account's OAuth credential and renews it when inside the skew.
/// A refresh that fails returns the stale credential: the gateway then gets a
/// 401 and fails over, which is the same outcome as any other dead account.
pub fn hydrate_oauth(
    acc: &settings::AccountEntry,
    store: &dyn zerith_core::secrets::SecretStore,
) -> Option<zerith_providers::OAuthCredential> {
    let mut cred = zerith_core::secrets::load_oauth(store, &acc.id).ok().flatten()?;
    if cred.needs_refresh(oauth_now()) {
        if let Ok(fresh) = zerith_providers::oauth::refresh(cred.tool, &cred) {
            // ponytail: falha ao regravar só custa um refresh extra no próximo turno.
            let _ = zerith_core::secrets::save_oauth(store, &acc.id, &fresh);
            cred = fresh;
        }
    }
    Some(cred)
}

fn oauth_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

Em `candidate_from_account`, depois de `cfg.account_id = ...`:

```rust
    if acc.auth == settings::AuthMode::Oauth {
        cfg.auth = zerith_providers::config::AuthMode::Oauth;
        cfg.oauth = hydrate_oauth(acc, &zerith_core::secrets::OsKeyring);
        cfg.api_key = None; // OAuth e chave não convivem na mesma conta
    }
```

E `candidate_for_test` (usado pelo teste) como wrapper fino:

```rust
/// Test seam: o mesmo caminho de `resolve_gateway`, com um cofre injetado.
pub fn candidate_for_test(
    settings: &settings::Settings,
    target: &str,
    store: &dyn zerith_core::secrets::SecretStore,
) -> RouteCandidate {
    let _ = store;
    resolve_gateway(settings, target, &ProviderConfig::default()).0.candidates()[0].clone()
}
```

Se `Gateway` não expuser `candidates()`, adicione `pub fn candidates(&self) -> &[RouteCandidate]`. E `hydrate_oauth` precisa aceitar o store injetado — troque a assinatura interna por `hydrate_oauth_with(acc, store)` e mantenha `hydrate_oauth(acc)` chamando com `&OsKeyring`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-core oauth && cargo test -p zerith-runtime oauth_account 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "credencial oauth no cofre e hidratada no runtime" -- crates/zerith-core/src/secrets.rs crates/zerith-core/Cargo.toml crates/zerith-runtime/src/lib.rs crates/zerith-runtime/tests/oauth_account.rs Cargo.lock
```

---

## Task 5: `zerith login <tool>`

**Files:**
- Modify: `crates/zerith-cli/src/cmds.rs` (nova `login_cmd`, ao lado de `accounts_cmd`)
- Modify: `crates/zerith-cli/src/main.rs:110-180` (dispatch `("login", _)`) e `usage()`
- Test: `crates/zerith-cli/src/cmds.rs` (`mod tests`) — só o parser de flags e o caminho `--import` com HOME falso

**Interfaces:**
- Consumes: `oauth::{bind, authorize_url, wait_for_code, exchange, import_local, open_browser, Pkce}`, `secrets::{save_oauth}`, `settings::{AuthMode, AccountEntry}`.
- Produces: subcomando `zerith login <claude-code|codex|antigravity> [--account <id>] [--import] [--no-browser]`; cria/atualiza `[[accounts]]` com `auth = "oauth"`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn login_flags_parse_tool_account_import_and_no_browser() {
    let a = login_args(&["login".into(), "codex".into(), "--account".into(), "codex-work".into(), "--import".into(), "--no-browser".into()]).unwrap();
    assert_eq!(a.tool, zerith_providers::config::OAuthTool::Codex);
    assert_eq!(a.account, "codex-work");
    assert!(a.import);
    assert!(a.no_browser);
}

#[test]
fn login_rejects_an_unknown_tool() {
    let e = login_args(&["login".into(), "kiro".into()]).unwrap_err();
    assert!(e.to_string().contains("claude-code"));
}

#[test]
fn import_only_writes_the_credential_and_the_account() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
    std::fs::write(
        dir.path().join(".codex/auth.json"),
        r#"{"tokens":{"access_token":"AT","refresh_token":"RT","account_id":"acc"}}"#,
    )
    .unwrap();
    let paths = zerith_core::Paths::with_root(dir.path().join("z")).unwrap();
    login_cmd(&paths, &["login".into(), "codex".into(), "--import".into()], dir.path()).unwrap();
    let s = zerith_core::settings::Settings::load(&paths).unwrap();
    assert_eq!(s.accounts[0].auth, zerith_core::settings::AuthMode::Oauth);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p zerith-cli login_flags 2>&1 | tail -12`
Expected: FAIL — `login_args` não existe.

- [ ] **Step 3: Write minimal implementation**

```rust
struct LoginArgs {
    tool: zerith_providers::config::OAuthTool,
    account: String,
    import: bool,
    no_browser: bool,
}

fn login_args(args: &[String]) -> Result<LoginArgs, Box<dyn std::error::Error>> {
    let tool_s = args.get(1).map(String::as_str).unwrap_or("");
    let tool = zerith_providers::config::OAuthTool::parse(tool_s).ok_or(
        "uso: zerith login <claude-code|codex|antigravity> [--account <id>] [--import] [--no-browser]",
    )?;
    let mut account = tool.as_str().to_string();
    let mut import = false;
    let mut no_browser = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--account" => {
                account = args.get(i + 1).cloned().ok_or("--account precisa de um valor")?;
                i += 2;
                continue;
            }
            "--import" => import = true,
            "--no-browser" => no_browser = true,
            other => return Err(format!("flag desconhecida: {other}").into()),
        }
        i += 1;
    }
    Ok(LoginArgs { tool, account, import, no_browser })
}

pub fn login_cmd(
    paths: &Paths,
    args: &[String],
    home: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use zerith_providers::oauth;
    let a = login_args(args)?;

    let cred = if a.import {
        match oauth::import_local(a.tool, home) {
            Some(mut c) => {
                if c.needs_refresh(oauth::now_secs()) {
                    match oauth::refresh(a.tool, &c) {
                        Ok(fresh) => c = fresh,
                        Err(e) => return Err(format!("credencial importada expirada e o refresh falhou: {e}").into()),
                    }
                }
                c
            }
            None => return Err(format!("nenhuma credencial local de {} encontrada", a.tool.as_str()).into()),
        }
    } else {
        let pkce = oauth::Pkce::generate();
        let state = oauth::random_state();
        // bind antes do navegador: porta fixa ocupada falha aqui, não depois do consentimento
        let (listener, port) = oauth::bind(a.tool)?;
        let url = oauth::authorize_url_for_port(a.tool, &pkce, &state, port);
        println!("Abra no navegador:\n  {url}\n");
        if !a.no_browser {
            let _ = oauth::open_browser(&url);
        }
        println!("Aguardando o retorno em http://127.0.0.1:{port} …");
        let code = oauth::wait_for_code(listener, oauth::redirect_path(a.tool), &state)?;
        oauth::exchange(a.tool, &code, &pkce.verifier, &oauth::redirect_uri(a.tool, port))?
    };

    zerith_core::secrets::save_oauth(
        &zerith_core::secrets::OsKeyring,
        &a.account,
        &cred,
    )?;

    let account = a.account.clone();
    let provider = match a.tool {
        zerith_providers::config::OAuthTool::ClaudeCode => "anthropic",
        zerith_providers::config::OAuthTool::Codex => "openai",
        zerith_providers::config::OAuthTool::Antigravity => "gemini",
    };
    let tool_name = a.tool.as_str().to_string();
    Settings::save_user(paths, |s| {
        let entry = zerith_core::settings::AccountEntry {
            id: account.clone(),
            provider: provider.to_string(),
            auth: zerith_core::settings::AuthMode::Oauth,
            oauth_tool: Some(tool_name.clone()),
            ..Default::default()
        };
        match s.accounts.iter_mut().find(|x| x.id == account) {
            Some(slot) => *slot = entry,
            None => s.accounts.push(entry),
        }
    })?;

    println!(
        "conta {} conectada via {} (expira em {} min)",
        a.account,
        a.tool.as_str(),
        (cred.expires_at - oauth::now_secs()).max(0) / 60
    );
    Ok(())
}
```

`main.rs`: `("login", _) => cmds::login_cmd(&paths, args, &home_dir())?,`; `home_dir()` = `std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))` convertido para `PathBuf`. Linha em `usage()`: `zerith login <claude-code|codex|antigravity> [--account <id>] [--import] [--no-browser]`.

Em `oauth.rs`, expor o que o CLI usa: `pub fn random_state()`, `pub fn redirect_path(tool)`, `pub fn authorize_url_for_port(tool, pkce, state, port) -> String` (o `authorize_url_for` privado já é isso, basta tornar público e renomear).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-cli login 2>&1 | tail -12`
Expected: PASS nos 3 testes.

- [ ] **Step 5: Smoke manual (sem gastar token)**

```bash
./target/debug/zerith.exe login codex --account codex-work --import
./target/debug/zerith.exe accounts list
```
Expected: usa `~/.codex/auth.json`, imprime "conta codex-work conectada via codex" e a conta aparece como `oauth` na lista. Nenhum token na saída.

- [ ] **Step 6: Commit**

```bash
git commit -m "cli: zerith login (import e pkce no navegador)" -- crates/zerith-cli/src/cmds.rs crates/zerith-cli/src/main.rs crates/zerith-providers/src/oauth.rs
```

---

## Task 6: Transporte Codex (Responses API)

**Files:**
- Create: `crates/zerith-providers/src/codex.rs`
- Create: `crates/zerith-providers/tests/fixtures/codex.sse`
- Modify: `crates/zerith-providers/src/config.rs` (`Protocol::Responses`, preset `codex`)
- Modify: `crates/zerith-providers/src/openai.rs` (dispatch em `complete`/`stream`)
- Test: `crates/zerith-providers/src/codex.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Client::headers()`/`post` (Task 1), `OAuthCredential.account_id`.
- Produces: `codex::body(&ChatRequest, stream) -> serde_json::Value`, `codex::parse_sse(data: &str, sink: &mut CodexStream)`; `Protocol::Responses`; `ProviderKind::Codex` com base `https://chatgpt.com/backend-api/codex` e protocolo `Responses`.

- [ ] **Step 1: Write the failing test**

`tests/fixtures/codex.sse`:

```
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Olá"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":" mundo"}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}

event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":3}}}
```

```rust
#[test]
fn sse_becomes_events_and_one_tool_call() {
    let raw = include_str!("../tests/fixtures/codex.sse");
    let mut st = CodexStream::default();
    let mut events = Vec::new();
    for line in raw.lines().filter_map(|l| l.strip_prefix("data: ")) {
        st.feed(line, &mut |e| events.push(e)).unwrap();
    }
    st.finish(&mut |e| events.push(e)).unwrap();
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            crate::StreamEvent::Delta(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Olá mundo");
    let calls = events.iter().find_map(|e| match e {
        crate::StreamEvent::ToolCalls(c) => Some(c.clone()),
        _ => None,
    });
    assert_eq!(calls.unwrap()[0].name, "read_file");
    assert!(matches!(events.last(), Some(crate::StreamEvent::Done { .. })));
}

#[test]
fn body_uses_instructions_and_flat_tools() {
    let req = crate::ChatRequest::new("gpt-5-codex", vec![crate::ChatMessage::system("sys")])
        .tools(vec![crate::ToolDef {
            name: "read_file".into(),
            description: "lê".into(),
            parameters: serde_json::json!({"type":"object"}),
        }]);
    let b = body(&req, true);
    assert_eq!(b["instructions"], "sys");
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["name"], "read_file");
    assert_eq!(b["stream"], true);
    assert!(b["input"].is_array());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p zerith-providers codex 2>&1 | tail -12`
Expected: FAIL — módulo `codex` inexistente.

- [ ] **Step 3: Write minimal implementation**

`codex.rs` (essencial):

```rust
//! `chatgpt.com/backend-api/codex`: the Responses API a Codex subscription
//! answers on. Different wire, different stream, same `ChatRequest`.

use crate::{ChatRequest, ChatMessage, ProviderError, Result, Role, StreamEvent, ToolCall};
use serde_json::{json, Value};

pub(crate) fn body(req: &ChatRequest, stream: bool) -> Value {
    let mut instructions = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => instructions.push(m.content.clone()),
            Role::User => input.push(json!({
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": m.content}]
            })),
            Role::Assistant => {
                if !m.content.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": m.content}]
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call", "call_id": c.id,
                        "name": c.name, "arguments": c.arguments
                    }));
                }
            }
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content
            })),
        }
    }
    let mut b = json!({
        "model": req.model,
        "input": input,
        "stream": stream,
        "store": false,
        "instructions": instructions.join("\n\n"),
    });
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters}))
            .collect();
    }
    b
}

#[derive(Default)]
pub(crate) struct CodexStream {
    text: String,
    calls: Vec<ToolCall>,
    usage: Option<crate::Usage>,
    done: bool,
}

impl CodexStream {
    pub(crate) fn feed(&mut self, data: &str, sink: &mut impl FnMut(StreamEvent)) -> Result<()> {
        let v: Value = serde_json::from_str(data).map_err(ProviderError::Json)?;
        match v["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    self.text.push_str(d);
                    sink(StreamEvent::Delta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    sink(StreamEvent::Reasoning(d.to_string()));
                }
            }
            "response.output_item.done" => {
                let item = &v["item"];
                if item["type"] == "function_call" {
                    self.calls.push(ToolCall {
                        id: item["call_id"].as_str().unwrap_or_default().to_string(),
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
                    });
                }
            }
            "response.completed" => {
                let u = &v["response"]["usage"];
                if u.is_object() {
                    self.usage = Some(crate::Usage {
                        prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        completion_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
                    });
                }
                self.done = true;
            }
            "response.failed" | "error" => {
                return Err(ProviderError::Malformed(v.to_string()));
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn finish(self, sink: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if !self.calls.is_empty() {
            sink(StreamEvent::ToolCalls(self.calls));
        }
        if !self.done {
            return Err(ProviderError::Malformed("stream codex sem response.completed".into()));
        }
        sink(StreamEvent::Done { usage: self.usage });
        Ok(())
    }
}

pub(crate) fn completion_from_body(text: &str, _: &ChatMessage) -> Result<crate::Completion> {
    let v: Value = serde_json::from_str(text)?;
    let mut out = crate::Completion {
        text: String::new(),
        reasoning: None,
        usage: None,
        finish_reason: None,
        tool_calls: Vec::new(),
    };
    for item in v["output"].as_array().into_iter().flatten() {
        match item["type"].as_str().unwrap_or("") {
            "message" => {
                for c in item["content"].as_array().into_iter().flatten() {
                    if let Some(t) = c["text"].as_str() {
                        out.text.push_str(t);
                    }
                }
            }
            "function_call" => out.tool_calls.push(ToolCall {
                id: item["call_id"].as_str().unwrap_or_default().to_string(),
                name: item["name"].as_str().unwrap_or_default().to_string(),
                arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
            }),
            _ => {}
        }
    }
    Ok(out)
}
```

`config.rs`: `Protocol::Responses` (`as_str` = `"responses"`), e

```rust
const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
```

`ProviderKind::Codex` com `parse("codex")`, `default_base_url() = CODEX_BASE`,
`default_protocol() = Protocol::Responses`, `needs_key() = true`, incluído em `PRESETS` (o teste
`presets.len() == 7` passa a esperar 8).

`openai.rs`, em `complete`/`stream`, antes do caminho OpenAI:

```rust
        if self.cfg.protocol == Protocol::Responses {
            return crate::codex::complete(self, req, cancel);
        }
```
```rust
        if self.cfg.protocol == Protocol::Responses {
            return crate::codex::stream(self, req, cancel, &mut on_event);
        }
```

E no `headers()` (Task 1), para Codex:

```rust
                if cred.tool == OAuthTool::Codex {
                    h.insert("accept".into(), "text/event-stream".into());
                    h.insert("openai-beta".into(), "responses=experimental".into());
                    h.insert("originator".into(), "codex_cli_rs".into());
                    if let Some(id) = &cred.account_id {
                        h.insert("chatgpt-account-id".into(), id.clone());
                    }
                }
```

`codex::complete` e `codex::stream` usam `client.post("responses", &body(req, stream), cancel)`; no stream, `crate::stream::for_each_data(resp, cancel, |d| st.feed(d, &mut on_event))`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-providers 2>&1 | tail -20`
Expected: PASS, incluindo os dois novos e o de presets ajustado para 8.

- [ ] **Step 5: Commit**

```bash
git commit -m "providers: transporte codex (responses api)" -- crates/zerith-providers/src/codex.rs crates/zerith-providers/tests/fixtures/codex.sse crates/zerith-providers/src/config.rs crates/zerith-providers/src/openai.rs
```

---

## Task 7: Transporte Antigravity (`v1internal`), atrás de flag

**Files:**
- Create: `crates/zerith-providers/src/antigravity.rs`
- Modify: `crates/zerith-providers/src/config.rs` (`Protocol::CloudCode`, preset `antigravity`)
- Modify: `crates/zerith-providers/src/openai.rs` (dispatch)
- Test: `crates/zerith-providers/src/antigravity.rs`

**Interfaces:**
- Consumes: `Protocol::CloudCode`, `OAuthCredential`.
- Produces: `antigravity::project(client) -> Result<String>` (`loadCodeAssist` → `onboardUser`), `antigravity::body(&ChatRequest, project, session_id) -> Value`, `antigravity::parse_sse`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn wrapped_body_carries_model_project_and_generation_config() {
    let req = crate::ChatRequest::new("gemini-3-pro", vec![crate::ChatMessage::user("oi")])
        .max_tokens(100);
    let b = body(&req, "proj-1", "sess-1");
    assert_eq!(b["model"], "gemini-3-pro");
    assert_eq!(b["project"], "proj-1");
    assert_eq!(b["request"]["sessionId"], "sess-1");
    assert_eq!(b["request"]["contents"][0]["role"], "user");
    assert_eq!(b["request"]["generationConfig"]["maxOutputTokens"], 100);
}

#[test]
fn sse_delta_and_usage_parse() {
    let line = r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}"#;
    let mut st = CloudStream::default();
    let mut ev = Vec::new();
    st.feed(line, &mut |e| ev.push(e)).unwrap();
    assert!(matches!(&ev[0], crate::StreamEvent::Delta(d) if d == "ok"));
    assert!(matches!(ev[1], crate::StreamEvent::Done { usage: Some(_) }));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p zerith-providers antigravity 2>&1 | tail -10`
Expected: FAIL — módulo inexistente.

- [ ] **Step 3: Write minimal implementation**

```rust
//! `daily-cloudcode-pa.googleapis.com/v1internal` — o endpoint que o Antigravity
//! usa. Envelope e SSE não são os do `generateContent` público.
//!
//! ponytail: sem client_id público não há login completo; o transporte já
//! funciona com um token válido colado no cofre. Flag: `settings.oauth.antigravity`.

use crate::{ChatRequest, ProviderError, Result, Role, StreamEvent};
use serde_json::{json, Value};

pub(crate) const BASE: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal";

pub(crate) fn body(req: &ChatRequest, project: &str, session_id: &str) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    let mut system = Vec::new();
    for m in &req.messages {
        let role = match m.role {
            Role::System => {
                system.push(m.content.clone());
                continue;
            }
            Role::Assistant => "model",
            _ => "user",
        };
        contents.push(json!({"role": role, "parts": [{"text": m.content}]}));
    }
    let mut request = json!({"contents": contents, "sessionId": session_id});
    if !system.is_empty() {
        request["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
    }
    if let Some(n) = req.max_tokens {
        request["generationConfig"] = json!({"maxOutputTokens": n});
    }
    let mut b = json!({"model": req.model, "project": project, "request": request});
    if matches!(req.model.as_str(), m if m.starts_with("claude")) {
        b["request"]["anthropic_version"] = json!("vertex-2023-10-16");
    }
    b
}

#[derive(Default)]
pub(crate) struct CloudStream {
    done: bool,
}

impl CloudStream {
    pub(crate) fn feed(&mut self, data: &str, sink: &mut impl FnMut(StreamEvent)) -> Result<()> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(()); // keep-alive / comentário do SSE
        };
        if let Some(parts) = v["candidates"][0]["content"]["parts"].as_array() {
            for p in parts {
                if let Some(t) = p["text"].as_str() {
                    sink(StreamEvent::Delta(t.to_string()));
                }
            }
        }
        if let Some(u) = v.get("usageMetadata").filter(|u| u.is_object()) {
            sink(StreamEvent::Done {
                usage: Some(crate::Usage {
                    prompt_tokens: u["promptTokenCount"].as_u64().unwrap_or(0),
                    completion_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0),
                    total_tokens: u["totalTokenCount"].as_u64().unwrap_or(0),
                }),
            });
            self.done = true;
        }
        Ok(())
    }

    pub(crate) fn finish(self, sink: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if !self.done {
            sink(StreamEvent::Done { usage: None });
        }
        Ok(())
    }
}

/// `loadCodeAssist` devolve o project id do usuário; sem ele o `generateContent`
/// responde 403.
pub(crate) fn project(client: &crate::Client) -> Result<String> {
    let resp = client.post(":loadCodeAssist", &json!({"metadata": {"pluginType": "GEMINI"}}), &crate::Cancel::default())?;
    let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| ProviderError::Transport(e.to_string()))?)?;
    v["cloudaicompanionProject"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ProviderError::Malformed("loadCodeAssist sem cloudaicompanionProject".into()))
}

pub(crate) fn parse_chat(text: &str) -> Result<crate::Completion> {
    let mut st = CloudStream::default();
    let mut out = crate::Completion {
        text: String::new(),
        reasoning: None,
        usage: None,
        finish_reason: None,
        tool_calls: Vec::new(),
    };
    st.feed(text, &mut |e| match e {
        StreamEvent::Delta(d) => out.text.push_str(&d),
        StreamEvent::Done { usage } => out.usage = usage,
        _ => {}
    })?;
    Ok(out)
}
```

`config.rs`: `Protocol::CloudCode` (`as_str` = `"cloud-code"`), `ProviderKind::Antigravity` com
`parse("antigravity")`, `default_base_url() = antigravity::BASE`, `default_protocol() = CloudCode`,
`needs_key() = true`, e entrada em `PRESETS` (teste passa a 9).

`openai.rs`: dispatch `if self.cfg.protocol == Protocol::CloudCode { .. }` em `complete`/`stream`;
o stream usa `:streamGenerateContent?alt=sse` e `crate::stream::for_each_data`. O `project` fica em
`OAuthCredential.account_id` (o login — quando existir client — grava lá); ausente = erro legível
`Malformed("antigravity sem project; rode `zerith login antigravity`")`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p zerith-providers 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -m "providers: transporte antigravity (v1internal)" -- crates/zerith-providers/src/antigravity.rs crates/zerith-providers/src/config.rs crates/zerith-providers/src/openai.rs
```

---

## Task 8: UI — botão e estado da conexão OAuth

**Files:**
- Modify: `crates/zerith-ui/src/screens/providers/accounts.rs`, `state.rs`, `actions.rs`
- Test: `crates/zerith-ui/tests/` (snapshot headless com `--scene providers-accounts`)

**Interfaces:**
- Consumes: `secrets::{load_oauth, save_oauth}`, `oauth::{bind, authorize_url_for_port, wait_for_code, exchange}`, `Settings.accounts[i].auth/oauth_tool`.
- Produces: em cada conta, um pill de origem (`API key` / `OAuth · expira em 3h`) e, quando `oauth`, um botão "Reconectar" que roda o mesmo fluxo do CLI numa thread e atualiza o estado.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_oauth_account_renders_its_tool_and_expiry() {
    let mut s = zerith_ui::Shell::default();
    s.settings.accounts.push(zerith_core::settings::AccountEntry {
        id: "claude-assinatura".into(),
        provider: "anthropic".into(),
        auth: zerith_core::settings::AuthMode::Oauth,
        oauth_tool: Some("claude-code".into()),
        ..Default::default()
    });
    let tree = zerith_ui::screens::providers::accounts::tree(&s);
    let dump = zerith_ui::tree_text(&tree);
    assert!(dump.contains("OAuth"));
    assert!(dump.contains("claude-code"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p zerith-ui an_oauth_account_renders 2>&1 | tail -12`
Expected: FAIL — o texto "OAuth" não aparece.

- [ ] **Step 3: Write minimal implementation**

Em `accounts.rs`, na linha da conta, ao lado do id:

```rust
    let origem = match acc.auth {
        AuthMode::Oauth => {
            let saldo = zerith_core::secrets::load_oauth(&zerith_core::secrets::OsKeyring, &acc.id)
                .ok()
                .flatten()
                .map(|c| {
                    let mins = (c.expires_at - zerith_providers::oauth::now_secs()).max(0) / 60;
                    format!("OAuth · {mins} min")
                })
                .unwrap_or_else(|| "OAuth · sem credencial".into());
            format!("{} · {saldo}", acc.oauth_tool.as_deref().unwrap_or("oauth"))
        }
        AuthMode::ApiKey => "API key".to_string(),
    };
    list.row(acc.id.clone()).caption(origem)
```

O botão "Reconectar" chama `actions::relogin(acc.id.clone(), tool)` que faz o mesmo que `login_cmd`
(código compartilhado: extraia `cmds::login_flow(paths, tool, account, import, no_browser) -> Result<OAuthCredential>`
e chame dos dois lados — o CLI imprime, a UI mostra no estado da tela).

- [ ] **Step 4: Run tests and render the screen**

Run: `cargo test -p zerith-ui 2>&1 | tail -12 && ./target/debug/zerith-ui.exe --theme dark --snap "C:/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/zerith/target/shots/providers-accounts-oauth.png" --scene providers-accounts`
Expected: PASS e PNG gerado mostrando `OAuth · <n> min`.

- [ ] **Step 5: Commit**

```bash
git commit -m "ui: estado oauth e reconectar na tela de contas" -- crates/zerith-ui/src/screens/providers/ crates/zerith-cli/src/cmds.rs
```

---

## Task 9: Documentação

**Files:**
- Create: `docs/oauth.md`
- Modify: `docs/providers.md` (seção "Chave de API" ganha o caminho OAuth), `README.md` (uma linha em features)

- [ ] **Step 1: Escrever `docs/oauth.md`**

Conteúdo obrigatório: tabela dos três provedores (endpoints, scopes, porta de callback), `zerith login` com e sem `--import`, onde a credencial fica (`cofre`, entrada `<id>.oauth`), quando renova (300 s antes / 401 → failover), o que fazer quando falha (`zerith login <tool>` de novo), e a limitação do Antigravity (sem client público; transporte pronto, login pendente).

- [ ] **Step 2: Verificar que a doc não mente**

Run: `cargo test -p zerith-providers -p zerith-core -p zerith-cli -p zerith-runtime 2>&1 | tail -8`
Expected: PASS — cada comando citado na doc existe (`zerith login`, `zerith accounts list`, `zerith secret set`).

- [ ] **Step 3: Commit**

```bash
git commit -m "docs: login oauth dos clis" -- docs/oauth.md docs/providers.md README.md
```

---

## Self-Review

**Cobertura da spec:** PKCE + listener + exchange + refresh → Task 2; import de credenciais locais → Task 2/5; `auth`/`oauth_tool` → Task 3; cofre + hidratação → Task 4; CLI → Task 5; Codex Responses → Task 6; Antigravity → Task 7; UI → Task 8; docs → Task 9. Coberto.

**Desvios da spec, explícitos:**
- *Refresh em 401*: a spec pedia renovar também no 401. O plano renova só de forma preventiva (300 s) e deixa o 401 cair no failover existente — mesma degradação com um caminho de código a menos. `ponytail:` anotado em `hydrate_oauth`. Se na prática o skew não bastar, o próximo passo é um `refresher` injetado no `Gateway`.
- *Antigravity*: transporte completo e testado, mas o login fica pendente por falta de `client_id` público (o 9router usa um client do CLI Google que não está no bundle instalado). Task 7 entrega o transporte; o login entra quando houver client.

**Type consistency:** `AuthMode` (providers) ≠ `AuthMode` (settings) de propósito, como `Protocol` ≠ `WireProtocol` no repo; a conversão vive em `candidate_from_account`. `OAuthCredential.tool` é `OAuthTool` em todo lugar; `expires_at` é sempre unix **segundos** (o import do Claude divide ms por 1000).

**Pendência conhecida:** Task 4 lista duas decisões de assinatura (`Gateway::candidates()`, `hydrate_oauth_with`). São seams de teste, não arquitetura nova — o executor resolve no próprio task e mantém os testes do Step 1 como contrato.
