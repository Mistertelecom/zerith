# Configuração

`zerith_core::Settings::load(&paths, Some(project_dir))`. Camadas, da menor
para a maior precedência:

| Camada | Arquivo | Rótulo em `config show` |
| --- | --- | --- |
| Padrões embutidos | — | `default` |
| Usuário | `<raiz de dados>/settings.toml` | `user` |
| Projeto | `<projeto>/.zerith/settings.toml` | `project` |
| Projeto-local (não versionar) | `<projeto>/.zerith/settings.local.toml` | `local` |
| Ambiente | `ZERITH_PROVIDER`, `ZERITH_BASE_URL`, `ZERITH_MODEL`, `ZERITH_USER`, `ZERITH_SANDBOX`, `ZERITH_MODE`, `ZERITH_PROFILE`, `ZERITH_SUBAGENT_MODEL`, `ZERITH_PONYTAIL`, `ZERITH_CAVEMAN` | `env` |

Regras de mescla: tabelas mesclam por chave; **listas concatenam** entre
camadas (regras de permissão, hooks, providers); escalares são substituídos.
No CLI o diretório de projeto é o diretório atual.

Arquivo inválido (sintaxe ou tipo): a camada inteira é ignorada, o erro
cita arquivo e linha (`settings.toml:4: configuração inválida, camada
ignorada: ...`) e o app sobe com as outras camadas.

## Exemplo

```toml
provider = "anthropic"
model = "claude-opus-5-5"
profile = "balanced"        # economy | balanced | performance

[[accounts]]
id = "claude-principal"
provider = "anthropic"
priority = 1
enabled = true
quota_tokens = 200000
quota_requests = 500
window_hours = 5
api_key_ref = "claude-principal"  # nome da entrada no cofre; nunca a chave

[[accounts]]
id = "deepseek-sub"
provider = "deepseek"
priority = 1
enabled = true
default_model = "deepseek-4.1-flash"
quota_tokens = 2000000
window_hours = 24
api_key_ref = "deepseek-sub"

[routing]
main_model = "anthropic/claude-opus-5-5"
subagent_model = "deepseek/deepseek-4.1-flash"
explore_model = "deepseek/deepseek-4.1-flash"

[token_saver]
ponytail = "full"           # off | lite | full | ultra
caveman = "lite"            # off | lite | full | ultra | wenyan
rtk_enabled = true

[[providers]]
name = "openai"
protocol = "openai"         # openai | anthropic
base_url = "https://api.openai.com/v1"
default_model = "gpt-4.1"
timeout_ms = 60000
context_limit = 128000
api_key_ref = "openai"      # nome da entrada no cofre; nunca a chave

[permissions]
mode = "workspace-write"    # preset: read-only | workspace-write | danger-full-access
default_mode = "default"    # default | acceptEdits | plan | dontAsk | bypassPermissions
allow = ["read_file", "list_dir", "shell(cargo test *)"]
ask = ["write_file"]
deny = ["write_file(*.env)"]

[[hooks.PreToolUse]]
matcher = "shell|write_file"
command = "python scripts/guard.py"
timeout_ms = 10000

[ui]
theme = "system"            # dark | light | system
reduced_motion = false

[memory]
paused = false
budget_chars = 2000
```

## Perfis

| Perfil | Subagentes | Histórico (mensagens) | Cache (entradas) |
| --- | --- | --- | --- |
| `economy` | 1 | 20 | 64 |
| `balanced` | 2 | 40 | 256 |
| `performance` | 8 | 80 | 1024 |

`Profile::limits()` devolve esses números, e eles valem de verdade:

- **Histórico:** mensagens reenviadas ao modelo por turno
  (`AgentOptions::history`), na janela, em `zerith chat` e `zerith -p`.
- **Subagentes:** concorrência padrão de `zerith plan run`; `--agents N`
  ainda sobrepõe.
- **Cache:** reservado; nenhum cache em memória lê esse número ainda.

O perfil é relido a cada turno, então trocar na janela vale já no próximo.

## O que já é usado

| Campo | Usado por |
| --- | --- |
| `model` / `routing.main_model` | `zerith chat`, `zerith -p`, `zerith-ui` (quando `--model`/`ZERITH_MODEL` não estão definidos) |
| `accounts[]` | Gateway local nativo (`zerith-runtime::resolve_gateway`, CLI e `zerith-ui`), failover, cotas e cofre |
| `routing.*` | Delegação de subagentes (`subagent_model`, `explore_model`, `plan_model`, `role_models`) e `combos` |
| `token_saver.*` | Prompt e compressão RTK por turno (`ponytail`, `caveman`, `rtk_enabled`) |
| `providers[].api_key_ref` | `zerith chat`, `zerith -p`, `zerith providers` e `zerith-ui` (busca no cofre) |
| `memory.paused` | `zerith -p`, `zerith chat`, `zerith-ui` |
| `profile` | turnos (histórico) e `zerith plan run` (subagentes) |
| `ui.*` | a janela lê e grava (tema, movimento reduzido) |
| `permissions.*`, `hooks.*` | `Policy::from_settings` + `Guards` e runner de hooks em `zerith -p`, `zerith chat` e `zerith-ui` |
| `provider`, `base_url` | `provider_config` em CLI/headless e `configured_providers` em `zerith-ui` (`ZERITH_PROVIDER`/`ZERITH_BASE_URL` ainda têm precedência) |

## Gravar

`Settings::save_user(&paths, |s| s.ui.theme = Theme::Dark)` edita só a
camada do usuário (nunca copia valores de projeto/env para ela), grava em
arquivo temporário e renomeia. Um `settings.toml` corrompido **não** é
sobrescrito: o erro volta para quem chamou.

## Ver

`zerith config show` lista cada chave mesclada com a camada de origem;
valores com nome de segredo aparecem como `****`.
