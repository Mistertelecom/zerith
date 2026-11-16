# Configuração

`zerith_core::Settings::load(&paths, Some(project_dir))`. Camadas, da menor
para a maior precedência:

| Camada | Arquivo | Rótulo em `config show` |
| --- | --- | --- |
| Padrões embutidos | — | `default` |
| Usuário | `<raiz de dados>/settings.toml` | `user` |
| Projeto | `<projeto>/.zerith/settings.toml` | `project` |
| Projeto-local (não versionar) | `<projeto>/.zerith/settings.local.toml` | `local` |
| Ambiente | `ZERITH_PROVIDER`, `ZERITH_BASE_URL`, `ZERITH_MODEL`, `ZERITH_USER`, `ZERITH_SANDBOX`, `ZERITH_PROFILE` | `env` |

Regras de mescla: tabelas mesclam por chave; **listas concatenam** entre
camadas (regras de permissão, hooks, providers); escalares são substituídos.
No CLI o diretório de projeto é o diretório atual.

Arquivo inválido (sintaxe ou tipo): a camada inteira é ignorada, o erro
cita arquivo e linha (`settings.toml:4: configuração inválida, camada
ignorada: ...`) e o app sobe com as outras camadas.

## Exemplo

```toml
provider = "openai"
model = "gpt-4.1"
profile = "balanced"        # economy | balanced | performance

[[providers]]
name = "openai"
protocol = "openai"         # openai | anthropic
base_url = "https://api.openai.com/v1"
default_model = "gpt-4.1"
timeout_ms = 60000
context_limit = 128000
api_key_ref = "openai"      # nome da entrada no cofre; nunca a chave

[permissions]
mode = "workspace-write"
allow = ["read_file", "list_dir"]
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

`Profile::limits()` devolve esses números. **Planejado:** runtime e planos
ainda usam as constantes próprias (`HISTORY_LIMIT = 40`, `--agents`).

## O que já é usado

| Campo | Usado por |
| --- | --- |
| `model` | `zerith chat`, `zerith -p` (quando `ZERITH_MODEL` não está definido) |
| `providers[].api_key_ref` | `zerith chat`, `zerith -p` (busca no cofre) |
| `memory.paused` | `zerith -p` |
| `ui.*` | `Settings::save_user` grava; a janela ainda não lê (**planejado**) |
| `permissions.*`, `hooks.*` | só biblioteca (**planejado** no runtime) |
| `provider`, `base_url` | o provider ainda vem de `ProviderConfig::from_env` (mesmas variáveis) |

## Gravar

`Settings::save_user(&paths, |s| s.ui.theme = Theme::Dark)` edita só a
camada do usuário (nunca copia valores de projeto/env para ela), grava em
arquivo temporário e renomeia. Um `settings.toml` corrompido **não** é
sobrescrito: o erro volta para quem chamou.

## Ver

`zerith config show` lista cada chave mesclada com a camada de origem;
valores com nome de segredo aparecem como `****`.
