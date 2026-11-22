# Login OAuth (Claude Code, Codex, Antigravity)

O Zerith usa as assinaturas que você já paga sem depender de proxy externo. A credencial
OAuth fica no **cofre do sistema** (Credential Manager / Keychain / secret-service), nunca em
`settings.toml` e nunca em log.

## Comandos

```bash
zerith login claude-code                      # abre o navegador (PKCE, porta efêmera)
zerith login codex --account codex-trabalho   # porta fixa 1455, /auth/callback
zerith login codex --import                   # reusa ~/.codex/auth.json, sem navegador
zerith login claude-code --import             # reusa ~/.claude/.credentials.json
zerith login antigravity                      # exige ZERITH_GOOGLE_CLIENT_ID (veja abaixo)
zerith accounts list                          # mostra a conta e a origem
```

`--account <id>` nomeia a conta criada (padrão: o nome da ferramenta). O comando grava a credencial
no cofre sob `<id>.oauth` e cria/atualiza:

```toml
[[accounts]]
id = "codex-trabalho"
provider = "codex"
auth = "oauth"
oauth_tool = "codex"
```

Consumo por API key continua igual: conta sem `auth` (ou `auth = "api_key"`) lê a chave do cofre
como antes.

## Provedores

| | claude-code | codex | antigravity |
|---|---|---|---|
| provider do settings | `anthropic` | `codex` | `antigravity` |
| authorize | `claude.ai/oauth/authorize` | `auth.openai.com/oauth/authorize` | `accounts.google.com/o/oauth2/v2/auth` |
| token | `platform.claude.com/v1/oauth/token` | `auth.openai.com/oauth/token` | `oauth2.googleapis.com/token` |
| PKCE | S256 | S256, porta fixa 1455 | S256 |
| API depois do login | `api.anthropic.com/v1/messages` (`Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`) | `chatgpt.com/backend-api/codex/responses` | `daily-cloudcode-pa.googleapis.com/v1internal` |
| import local | `~/.claude/.credentials.json` (`claudeAiOauth`) | `~/.codex/auth.json` (`tokens`) | não (token em DPAPI) |

## Renovação e falha

- A credencial é renovada **antes** de expirar (margem de 300 s) na hora de montar a conta.
- Se a renovação falhar, a credencial antiga é usada mesmo assim: o provedor responde 401 e o
  gateway faz failover para a próxima conta elegível — o mesmo caminho de qualquer conta morta.
- Credencial importada do arquivo de um CLI (`~/.codex/auth.json`, `~/.claude/.credentials.json`)
  nunca é renovada pelo Zerith (o refresh token rotaciona e deslogaria o CLI): o arquivo é relido e
  só um token mais novo dele é usado. Login pelo navegador é do Zerith e é renovado normalmente.
- Credencial sem `refresh_token` (Google/Anthropic só mandam na primeira autorização) mantém o
  token anterior quando a resposta de refresh não trouxer um novo.
- Quando nada funciona: `zerith login <tool>` de novo, ou `zerith accounts remove <id>`.

## Limites conhecidos

- **Windows Credential Manager** aceita 2560 bytes UTF-16 por entrada; o JSON da credencial é
  dividido em partes de 1024 caracteres (`<id>.oauth` guarda a contagem, `<id>.oauth.0…n` os
  pedaços). Um `zerith secret delete <id>.oauth` sozinho não limpa as partes — use o login seguinte
  (que reescreve tudo) ou remova `<id>.oauth.0…n` também.
- **Antigravity** não tem client OAuth público: exporte `ZERITH_GOOGLE_CLIENT_ID` com o client id do
  seu projeto Google antes de `zerith login antigravity`. O transporte da API (`v1internal`, SSE no
  formato Gemini) está em `crates/zerith-providers/src/antigravity.rs`; sem client secret, a
  renovação relê a conexão salva pelo 9router.
- **Codex** só responde na API Responses; o corpo e o SSE não são os do `chat/completions`
  (`crates/zerith-providers/src/codex.rs`).
