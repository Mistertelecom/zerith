# Login OAuth (Claude Code, Codex, Antigravity) e redesign da janela

Data: 2026-11-16
Status: aguardando revisão

## Problema

1. O Zerith só fala com provedores por **chave de API**. As assinaturas que o usuário já paga
   (Claude Code, ChatGPT/Codex, Antigravity) exigem OAuth, como o 9router faz.
2. A janela está funcionalmente completa, mas visualmente pobre: densidade baixa, hierarquia
   fraca, cromo cru (erro do SO em texto no rodapé), telas com 40% de vazio.

Ordem combinada: **OAuth primeiro**, redesign depois.

## Fatos verificados

- `zerith-providers` tem dois protocolos (`OpenAi`, `Anthropic`) e um `Gateway` multi-conta com
  failover antes do primeiro token (`gateway.rs`, `runtime::resolve_gateway`).
- Credencial vem de `ZERITH_API_KEY` ou do cofre do SO (`zerith_core::secrets`, serviço `zerith`).
- `Client::authored` (`openai.rs`) aplica `Authorization: Bearer` ou `x-api-key`; não há noção de
  modo de autenticação.
- Blueprint do 9router (extraído do bundle instalado):

| | claude-code | codex | antigravity |
|---|---|---|---|
| authorize | `https://claude.ai/oauth/authorize` | `https://auth.openai.com/oauth/authorize` | `https://accounts.google.com/o/oauth2/v2/auth` |
| token | `https://api.anthropic.com/v1/oauth/token` | `https://auth.openai.com/oauth/token` | `https://oauth2.googleapis.com/token` |
| client_id | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` | `app_EMoamEEZ73f0CkXaXp7hrann` | (Google, client do CLI) |
| PKCE | S256 | S256, porta fixa **1455**, callback `/auth/callback` | S256 |
| scopes | `org:create_api_key user:profile user:inference` | `openid profile email offline_access` | `cloud-platform userinfo.email userinfo.profile cclog experimentsandconfigs` |
| refresh | JSON | form + `scope` | form |
| API depois | `https://api.anthropic.com/v1/messages` | `https://chatgpt.com/backend-api/codex` | `https://daily-cloudcode-pa.googleapis.com/v1internal` |

- Credenciais locais: `~/.codex/auth.json` tem `refresh_token` válido (importável).
  `~/.claude/.credentials.json` hoje só tem `mcpOAuth` (sem login de assinatura).
  Antigravity guarda token em DPAPI (`AppData/Roaming/Antigravity`), não em texto → só OAuth próprio.
- Dependências já na árvore e reaproveitáveis: `ring` 0.17 (SHA-256 + random), `base64` 0.22,
  `ureq` 2. Nenhum crate novo.

## Fase A — Login OAuth

### Modelo

```toml
[[accounts]]
id = "claude-assinatura"
provider = "anthropic"
auth = "oauth"            # novo; ausente = "api_key" (comportamento de hoje)
oauth_tool = "claude-code" # claude-code | codex | antigravity
priority = 1
```

- `AccountEntry` ganha `auth: AuthMode` e `oauth_tool: Option<String>`; `ProviderEntry` ganha só
  `auth` (o provedor global pode ser OAuth).
- Credencial vive no cofre: entrada `<account-id>.oauth`, valor JSON
  `{access_token, refresh_token, expires_at, account_id, tool}`. Nunca em `settings.toml`,
  nunca em log, nunca em erro (redação já existe em `zerith-security`).
- Chave de API continua funcionando: `auth` ausente ⇒ caminho atual, sem migração de settings.

### Fluxo

`zerith login <claude-code|codex|antigravity> [--account <id>] [--import]`

1. `--import` (padrão quando existe): lê `~/.codex/auth.json` (codex) ou
   `~/.claude/.credentials.json` no bloco `claudeAiOauth` (claude-code). Se o token estiver
   expirado, segue direto para o refresh. Antigravity não tem import: vai para o passo 2.
2. PKCE: `code_verifier` = 32 bytes de `ring::rand::SystemRandom`, `code_challenge` =
   base64url(SHA-256(verifier)), `state` = 16 bytes.
3. Abre o navegador (`cmd /c start` / `open` / `xdg-open`) na URL de authorize; sobe um
   `std::net::TcpListener` loopback efêmero (codex: porta fixa 1455, `/auth/callback`) que aceita
   **uma** requisição, confere `state` e devolve uma página curta de "pode fechar".
4. Troca `code` no token endpoint (form ou JSON conforme a tabela), grava a credencial no cofre e
   cria/atualiza a conta em `settings.toml`.
5. Saída no terminal: provedor, conta, expiração e modelo padrão. Nunca o token.

Refresh: antes de cada chamada, se `expires_at - now < 5 min` ou o provedor responder 401/403, o
gateway renova com o `refresh_token`, regrava no cofre e repete **uma** vez. Falha de refresh =
`AccountCooldown` e failover para a próxima conta (comportamento atual, sem código novo).

### Protocolos

`Protocol` ganha `Responses` e `CloudCode`; `AuthMode::Oauth` muda só como o header é montado.

- **Anthropic (claude-code)**: mesmo `POST {base}/messages`, mas `Authorization: Bearer <token>` +
  `anthropic-beta: oauth-2025-04-20`. Base padrão `https://api.anthropic.com/v1`.
- **Codex**: `POST https://chatgpt.com/backend-api/codex/responses`, headers `Authorization`,
  `chatgpt-account-id`, `originator: codex_cli_rs`, `OpenAI-Beta: responses=experimental`; corpo
  no formato Responses (`input`, `tools` planos) e SSE com `response.output_text.delta` e
  `response.output_item.done` (function calls). Arquivo novo `codex.rs`.
- **Antigravity**: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent`
  (`:streamGenerateContent?alt=sse` para streaming) com envelope `{model, project, request}`;
  `project` vem de `v1internal:loadCodeAssist` / `onboardUser`. Arquivo novo `antigravity.rs`.
  É a integração mais frágil das três: fica por último e atrás de flag.

### Arquivos

| Arquivo | Mudança |
|---|---|
| `crates/zerith-providers/src/oauth.rs` | novo: PKCE, tabela de provedores, `login`, `refresh`, listener loopback |
| `crates/zerith-providers/src/codex.rs` | novo: transporte Responses |
| `crates/zerith-providers/src/antigravity.rs` | novo: transporte v1internal |
| `crates/zerith-providers/src/config.rs` | `AuthMode`, `Protocol::{Responses, CloudCode}`, presets `claude-code`/`codex`/`antigravity` |
| `crates/zerith-providers/src/openai.rs` | `authored` por modo; dispatch de protocolo |
| `crates/zerith-core/src/settings.rs` | `AccountEntry.auth`, `AccountEntry.oauth_tool`, `ProviderEntry.auth` |
| `crates/zerith-runtime/src/lib.rs` | carrega credencial do cofre, injeta no `ProviderConfig`, renova em 401 |
| `crates/zerith-cli/src/main.rs` | subcomando `login`, `accounts add --auth oauth` |
| `crates/zerith-ui/src/screens/providers/` | botão "Entrar com OAuth", estado (ok / expira em Xh / falhou) |
| `docs/oauth.md`, `docs/providers.md` | documentação do fluxo e dos três provedores |

### Testes (sem framework novo)

- PKCE contra o vetor do RFC 7636 (verifier/challenge).
- Header por modo: `Bearer` + beta para Anthropic OAuth, `x-api-key` para API key.
- Listener loopback: aceita o `state` correto uma vez, rejeita o errado, encerra depois.
- Refresh: parse JSON vs form; credencial expirada renova e regrava.
- Codex/Antigravity: SSE de fixture local (`tests/fixtures/*.sse`) vira `StreamEvent` correto.

## Fase B — Redesign (spec própria depois da Fase A)

Diagnóstico do render atual (`zerith-ui --snap`): erro do SO em texto cru no rodapé, dois títulos
concorrentes no empty state, chips desalinhados do card, painel Provedores 40% vazio, tabs/presets/
toggles/botões todos com o mesmo peso visual, `não definida` ambíguo no campo de chave, PT e EN
misturados.

Direção (referências: Claude Desktop, Linear, Raycast): sidebar mais estreita com rodapé de conta,
elevação real (borda + sombra) em vez de deltas de 3% de luminância, escala tipográfica com
contraste de peso, estado de conexão legível em texto (não só bolinha), ações primária/secundária
distintas, cromo do app (titlebar, status, erros) como componentes e não como texto solto.
Tokens novos em `ui.rs`, telas reescritas em ordem: Chat → Provedores → Configurações → Histórico.

## Fora de escopo

- OAuth para os demais provedores do 9router (Cursor, Kiro, Copilot, Qwen…).
- Auto-import de credencial do Antigravity (DPAPI; exigiria hook no app deles).
- Empacotar o 9router dentro do Zerith.
