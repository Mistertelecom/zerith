# Providers, Gateway Nativo, Delegação e Token Saver

O Zerith fala diretamente com APIs **OpenAI-compatible** (`POST {base}/chat/completions`) e **Anthropic** (`POST {base}/messages`) através de um **Gateway Local Nativo Multi-Conta** embutido (`zerith-providers::Gateway` + `zerith-runtime::resolve_gateway`). Não depende mais de proxy externo como o 9Router.

## Provedores embutidos (`ProviderKind`)

| Nome (`ZERITH_PROVIDER` / `provider`) | Protocolo | URL padrão | Chave |
| --- | --- | --- | --- |
| `openai` (padrão) | OpenAI | `https://api.openai.com/v1` | sim |
| `anthropic` (alias `claude`) | Anthropic | `https://api.anthropic.com/v1` | sim |
| `deepseek` | OpenAI | `https://api.deepseek.com/v1` | sim |
| `gemini` (alias `google`) | OpenAI | `https://generativelanguage.googleapis.com/v1beta/openai` | sim |
| `openrouter` | OpenAI | `https://openrouter.ai/api/v1` | sim |
| `ollama` | OpenAI | `http://localhost:11434/v1` | não |
| `lmstudio` (alias `lm-studio`) | OpenAI | `http://localhost:1234/v1` | não |
| `custom` | configurável | defina `ZERITH_BASE_URL` ou `base_url` | sim |

## Gateway Multi-Conta, Cotas e Failover Automático

É possível conectar **mais de uma conta por provedor** via `[[accounts]]` em `settings.toml` ou pelo CLI (`zerith accounts`) e tela **Contas / Provedores** em `zerith-desktop`:

```toml
[[accounts]]
id = "claude-principal"
provider = "anthropic"
priority = 1
enabled = true
quota_tokens = 200000
quota_requests = 500
window_hours = 5
api_key_ref = "claude-principal"

[[accounts]]
id = "claude-reserva"
provider = "anthropic"
priority = 2
enabled = true
quota_tokens = 200000
window_hours = 5
api_key_ref = "claude-reserva"

[[accounts]]
id = "deepseek-sub"
provider = "deepseek"
priority = 1
enabled = true
default_model = "deepseek-chat"
quota_tokens = 2000000
window_hours = 24
api_key_ref = "deepseek-sub"
```

- **Failover antes do primeiro token:** erros HTTP `401`, `402`, `403`, `408`, `429`, `5xx`, `RateLimited`, `NoKey` ou falha de transporte colocam a conta em cooldown (`account_cooldowns` no SQLite) e comutam automaticamente para a próxima conta elegível antes que o primeiro delta seja emitido.
- **Quota Tracker e `usage`:** cada tentativa e consumo é registrado em `usage_records` (migração `V7` em `zerith-sessions`). Use `zerith usage [--hours N]` (ou `zerith quota`) e o painel **Quota & Uso (24h)** na UI para ver qual conta/modelo está ativo agora, porcentagem restante de cota e economia por protocolo do Token Saver.

## Delegação Multi-Modelo / Cross-Account (`[routing]`)

Um modelo principal de um provedor/conta (ex.: `anthropic/claude-opus-5-5`) pode orquestrar o turno principal e delegar subagentes, exploração ou planos para outro modelo/conta mais econômico (ex.: `deepseek/deepseek-4.1-flash`):

```toml
[routing]
main_model = "anthropic/claude-opus-5-5"
subagent_model = "deepseek/deepseek-4.1-flash"
explore_model = "deepseek/deepseek-4.1-flash"
plan_model = "anthropic/claude-opus-5-5"

[routing.role_models]
reviewer = "claude-reserva:anthropic/claude-sonnet-4"

[[routing.combos]]
name = "fast"
models = ["claude-principal:anthropic/claude-sonnet-4", "deepseek-sub:deepseek/deepseek-chat"]
```

Formatos de alvo aceitos por `ModelSpec`: `modelo`, `<provider>/<modelo>` ou `<account_id>:<provider>/<modelo>`, além do nome de um combo. Via CLI: `zerith routing main ...`, `zerith routing subagent ...`, `zerith routing explore ...`, `zerith routing plan ...`, `zerith routing role ...`, `zerith routing combo ...`.

## Token Saver (`[token_saver]`)

Conjunto de três protocolos com medição independente em `usage_records`:

- **RTK** (`rtk_enabled = true|false`): filtragem determinística em saídas de ferramentas (`git-log`, `git-diff`, `git-status`, `build-output`, `grep`, `tree`, `find`, `dedup-log`, `smart-truncate`) antes de enviar ao modelo.
- **Ponytail** (`off | lite | full | ultra`): economia de engenharia ("lazy senior dev": YAGNI, reuso, stdlib antes de abstrações, diffs mínimos).
- **Caveman** (`off | lite | full | ultra | wenyan`): compressão telegráfica de prosa nas respostas preservando código, caminhos, comandos e avisos críticos.

Via CLI: `zerith tokensaver [show|ponytail <nível>|caveman <nível>|rtk <on|off>]` e `zerith usage`.

## Chave de API

Ordem: `ZERITH_API_KEY` → cofre do SO (serviço `zerith`, tentando `account.api_key_ref`, `account.id`, `provider.api_key_ref` e nome do provedor).

```bash
zerith secret set claude-principal   # lê o valor do stdin (fora do histórico do shell)
zerith secret get claude-principal   # mostra mascarado: sk-a…wxyz
zerith secret delete claude-principal
```

Sem cofre disponível: erro explícito, nada em texto puro. `zerith providers probe|models` continuam funcionando sem chave quando o servidor local não a exige (ex.: Ollama, LM Studio).

## Ordem de resolução de modelo

`--model` > `ZERITH_MODEL` > `[routing] main_model` > `model` / `default_model` em `settings.toml`.
