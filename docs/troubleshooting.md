# Problemas comuns

| Sintoma | Causa provável | O que fazer |
| --- | --- | --- |
| `no model: pass --model or set ZERITH_MODEL` | nenhum modelo em flag/env/settings | `--model <id>`, `ZERITH_MODEL` ou `model = "..."` em `settings.toml` |
| `providers probe` → falha de conexão | 9Router/Ollama/LM Studio parado ou outra porta | inicie o servidor; `ZERITH_BASE_URL` |
| `http 401` | chave ausente/errada | `zerith secret set <provider>` ou `ZERITH_API_KEY` |
| `cofre de credenciais do sistema indisponível` | sem Credential Manager/Keychain/keyutils (ex.: container) | use `ZERITH_API_KEY`; o Zerith não grava chave em texto puro |
| `settings.toml:N: configuração inválida, camada ignorada` | TOML ou valor inválido | corrija a linha; o app segue com as outras camadas |
| `needs approval: ...` | escrita fora do workspace (`Ask` ainda vira recusa) | trabalhe dentro do workspace ou mude o preset |
| `shell` recusada | por desenho: não há sandbox real | use MCP ou plugin |
| `cl.exe D8050` no build (Windows) | toolset MSVC + `/Z7` no SQLite bundled | já contornado com `[profile.dev] debug = false` |
| Testes falham com acesso negado em `%TEMP%` | ACL restrita | testes usam `target/test-scratch`; não aponte para `%TEMP%` |

Diagnóstico geral: `zerith doctor`, `zerith paths`, `zerith config show`,
`zerith policy`.
