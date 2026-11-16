# Critérios de aceite — estado real

Cada linha do spec (seção 21) com o que foi **provado** e como. `Feito` exige
evidência executada, não código escrito. Onde falta, está dito o que falta.

## Produto e arquitetura

| Item | Estado | Evidência |
| --- | --- | --- |
| Código de terceiros documentado | Feito | `UPSTREAMS.md`, `THIRD_PARTY_NOTICES.md`, `DEPENDENCY_LICENSES.md` |
| Licenças auditadas e avisos incluídos | Feito | `THIRD_PARTY_NOTICES.md`; GPL-3.0-only em `LICENSE` e no `license` do workspace |
| Rune é a interface desktop principal | Feito | `zerith-ui` usa `rune`; provado headless com `--tree` e `--snap` |
| Sem Electron, Tauri ou Chromium | Feito | nenhuma dependência dessas no `Cargo.toml`; a UI é `rune` |
| Núcleo Rust e UI separados | Feito | `zerith-core`/`-runtime` não dependem de `rune` |

## Chat e providers

| Item | Estado | Evidência |
| --- | --- | --- |
| Usuário configura um provider | Feito | `ZERITH_PROVIDER`, `ZERITH_BASE_URL`, `ZERITH_API_KEY`, `ZERITH_MODEL`; `zerith providers probe` |
| 9Router via API local OpenAI-compatible | Feito | `providers probe` → `http 200, 59 models` em `localhost:20128/v1` |
| Streaming funciona | Feito | turno real com `DEEPSEEKV4.1`: `[turn] tokens 2878→83`; SSE com `reasoning_content` |
| Cancelamento funciona | Parcial — em correção; não reverificado nesta rodada | botão "parar" com `Cancel`; teste `stop_cancels_the_token_and_does_nothing_without_a_turn`. **O token só é checado entre linhas SSE**: um servidor que não envia nada (conexão presa, primeiro byte lento) não é interrompido até o timeout |
| Erros de rede úteis | Feito | `ProviderError::Http{status,body}`; `main()` imprime com `redact` |
| Conversas persistidas | Feito | dois turnos na mesma sessão, `session show` com 4 mensagens |

## Memória

| Item | Estado | Evidência |
| --- | --- | --- |
| Continua após reiniciar | Feito | SQLite em `ZERITH_HOME`; fato gravado num processo e recuperado no seguinte |
| Busca e recuperação funcionam | Feito | FTS5 + `bm25`; `recall_200_facts` 0.59 ms |
| Usuário edita e exclui memórias | Feito | `memory edit <id> <texto>` (provado: "neovim" → "helix", recall acha o texto novo), `memory forget` e botão de apagar no painel |
| Dados locais por padrão | Feito | um arquivo SQLite; nada sai da máquina |
| Recuperação respeita o orçamento | Feito | `MEMORY_BUDGET_CHARS = 2000`, `MEMORY_HITS = 12`, teste `the_block_stops_at_the_budget_instead_of_growing_forever` |

## Hermes

| Item | Estado | Evidência |
| --- | --- | --- |
| Detecta instalação válida | Feito | `import detect`; sem Hermes nesta máquina retorna vazio |
| Importa do banco | Feito | 5 testes em `zerith-importers/tests/hermes.rs` |
| Preserva mensagens, ordem e metadados | Feito | todas as colunas em `raw`; teste `unknown_columns_survive_in_raw` |
| Importação repetida não duplica | Feito | `sessions_source_key` único; teste `a_second_run_imports_nothing...` |
| Importação interrompida pode ser retomada | Parcial | idempotência por sessão (`source_id`+perfil) faz a repetição completar o que faltou; **não há cursor por mensagem** |
| Originais inalterados | Feito | aberto read-only; teste `the_source_database_is_not_modified` |

## Skills

| Item | Estado | Evidência |
| --- | --- | --- |
| Catálogo busca dados reais | Feito | 8 repos via API do GitHub, estrelas reais (mattpocock/skills 325219) |
| Estrelas e datas com fonte e timestamp | Feito | `fetched_at` no cache; URL do repo por entrada |
| Instalar, ativar, atualizar, remover | Feito | `.audit/sk2.sh`: install → disable → enable → update → rollback → remove |
| Conteúdo não é executado automaticamente | Feito | `scan` lista scripts e marca `(not executed)`; nada invoca script |
| Conteúdo carregado sob demanda | Feito | `catalog()` só nome+descrição; corpo só quando o usuário nomeia a skill |

## Segurança

| Item | Estado | Evidência |
| --- | --- | --- |
| Permissões respeitadas | Parcial — em correção; não reverificado nesta rodada | `Policy::check` antes de escrita e shell; teste de escape de workspace. **Leituras (`read_file`, `list_dir`) não passavam pela política**; regras `allow/ask/deny` de `settings.toml` existem só como biblioteca |
| Segredos não aparecem em logs | Feito | `redact` e `redact_headers`; erros do `main()` passam por `redact`; chaves no cofre do SO (`zerith secret`), `config show`/`secret get` mascaram |
| Operações arriscadas exigem confirmação | Parcial — em correção; não reverificado nesta rodada | `Verdict::Ask` para escrita fora do workspace; `shell` recusada. **`Ask` é tratado como recusa: não há caminho de aprovação** — o usuário nunca é perguntado |
| Plugins e Skills tratados como não confiáveis | Feito | instalar = `disabled` + `unreviewed`; `trust` separado; scripts nunca executados |
| Sandbox não anunciada como garantia | Feito | `Enforcement::Partial` impresso por `zerith policy` |

## Desempenho

| Item | Estado | Evidência |
| --- | --- | --- |
| Benchmarks de RAM/CPU publicados | Parcial | `zerith bench` mede tempo e RSS; **CPU-time não é medido** (documentado em `docs/release.md`) |
| Idle sem polling | Feito | janela acorda por evento; turno em thread própria |
| Listas paginadas | Feito | `sessions(100)`, `messages(200, offset)`; a UI pede 200 por sessão |
| Histórico completo não fica em RAM | Feito | só a página carregada |
| Concorrência limitada | Feito | `run_plan` com `clamp(1, 8)` |

## Distribuição

| Item | Estado | Evidência |
| --- | --- | --- |
| Builds em Windows, Linux e macOS | **Pendente** | workflow `zerith-release.yml` existe; só Windows foi compilado aqui |
| Instalação e desinstalação documentadas | Parcial | `docs/release.md` descreve instalação portátil; **não há instalador** |
| Backup e restauração testados | Feito | `.audit/bk.sh`: backup, apagar banco, restore, `doctor` OK |
| Configuração e dados sobrevivem à atualização | Feito | migrações versionadas; banco vive fora da pasta dos binários |

## CLI, sessões, MCP e plugins

| Item | Estado | Evidência |
| --- | --- | --- |
| Retomar, continuar e bifurcar sessões (`--resume`, `--continue`, `--fork`, nome) | Feito | `args::tests::select_session_creates_resumes_forks_and_explains_failures`, `sessions_resolve_by_id_title_or_unique_prefix_and_rename_keeps_order`; fumaça no binário: `session rename`, `session show "<título>"` |
| Exportar, buscar e renomear sessões | Feito | `export`/`search` rodados no binário; `parse_export`, `parse_search`, `parse_rename` testados |
| Rewind de arquivos e conversa (armazenamento e restauração) | Feito | 9 testes em `zerith-sessions/tests/checkpoints.rs` com diretórios temporários: restaura, remove o que o turno criou, vence o turno mais antigo, rewind suave + `--purge`, tetos de tamanho, retenção de 100 turnos |
| Rewind desfaz o que o agente editou | **Pendente** | as ferramentas `write_file`/`edit_file` ainda não chamam `Checkpoint::record_file` (ver `docs/checkpoints.md`); sem isso o checkpoint guarda só o ponto de corte da conversa. Edições por `shell` nunca serão rastreadas |
| `-p` com `--max-turns`, `--allowed-tools`, `--permission-mode`, `--json-schema`, `--max-budget-usd` | Parcial | parsers, validador de schema, custo e política testados (`args`, `schema`, `headless`); **nenhum teste executa um turno `-p` contra um modelo**. Orçamento exige preços informados pelo usuário; o schema cobre só `type`/`enum`/`required`/`properties`/`items` |
| Código de saída 130/143 em Ctrl+C / SIGTERM | Parcial | código do primeiro sinal testado; o tratador real foi **compilado só no Windows** e não foi acionado por um sinal de verdade; o ramo Unix não foi compilado |
| `doctor` estendido (configuração, cofre, sandbox, provider, MCP, plugins, banco) | Feito | executado no binário com `--offline`; o teste de alcance do provider (sem `--offline`) não foi rodado |
| MCP no laço do agente (`mcp__<servidor>__<ferramenta>`) | Parcial | `zerith-mcp/tests/tools.rs` contra servidor HTTP no próprio teste: registro com schema, chamada, `isError`, servidor fora do ar, recursos, prompts. **Não** testado por stdio via adaptador; só `zerith -p` registra as ferramentas (janela e `chat` não) |
| Servidor MCP de projeto exige aprovação do usuário | Feito | `a_project_cannot_approve_its_own_server`, `approval_from_the_user_file_lets_the_project_server_run` |
| Plugins em pacote (skills, agentes, hooks, MCP) | Parcial | manifesto, validação de caminhos e confiança testados (`zerith-plugins/tests/library.rs`); só o MCP de plugin é carregado (em `-p`), o runtime **não** carrega skills, agentes e hooks de plugin |
| Importar JSONL do Hermes pelo CLI | Parcial | `import hermes --jsonl` chama o importador já testado em `hermes_jsonl.rs`; nunca visto com uma exportação real |

## Resumo

Contagem refeita a partir das tabelas acima (não de memória):

- Feito com evidência: 39 de 52 itens.
- Parcial: 11.
- Pendente: 2.

Três linhas de segurança e cancelamento (`Cancelamento funciona`, `Permissões
respeitadas`, `Operações arriscadas exigem confirmação`) ficaram como estavam e
estão marcadas como **não reverificadas**: o runtime estava em edição por outra
frente quando esta tabela foi atualizada e seus testes não puderam ser
executados. O estado delas pode ter melhorado; não foi provado aqui.

Nenhum destes "parcial" ou "pendente" foi escrito como se estivesse pronto: o
spec proíbe marcar pronto sem evidência, e é mais útil saber onde dói.
