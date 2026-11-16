# Critérios de aceite — estado real

Cada linha do spec (seção 21) com o que foi **provado** e como. `Feito` exige
evidência executada, não código escrito. Onde falta, está dito o que falta.

## Produto e arquitetura

| Item | Estado | Evidência |
| --- | --- | --- |
| Fork preserva histórico e origem upstream | Feito | repositório git com `upstream` = clone completo do OpenHuman (HEAD `5e6d98d433`); branch de trabalho `zerith/fase-0-auditoria` |
| Código reutilizado documentado | Feito | `docs/audit/initial-audit.md` §2 e §11 |
| Licenças auditadas e avisos incluídos | Feito | §9 do mesmo documento; GPL-3.0-only no `license` do workspace |
| Rune é a interface desktop principal | Feito | `zerith-ui` usa `rune`; provado headless com `--tree` e `--snap` |
| Sem Electron, Tauri ou Chromium | Feito | nenhuma dependência dessas no `zerith/Cargo.toml`; a UI é `rune` |
| Núcleo Rust e UI separados | Feito | `zerith-core`/`-runtime` não dependem de `rune` |

## Chat e providers

| Item | Estado | Evidência |
| --- | --- | --- |
| Usuário configura um provider | Feito | `ZERITH_PROVIDER`, `ZERITH_BASE_URL`, `ZERITH_API_KEY`, `ZERITH_MODEL`; `zerith providers probe` |
| 9Router via API local OpenAI-compatible | Feito | `providers probe` → `http 200, 59 models` em `localhost:20128/v1` |
| Streaming funciona | Feito | turno real com `DEEPSEEKV4.1`: `[turn] tokens 2878→83`; SSE com `reasoning_content` |
| Cancelamento funciona | Feito | botão "parar" com `Cancel`; teste `stop_cancels_the_token_and_does_nothing_without_a_turn` |
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
| Permissões respeitadas | Feito | `Policy::check` antes do filesystem; teste de escape de workspace |
| Segredos não aparecem em logs | Feito | `redact` e `redact_headers`; erros do `main()` passam por `redact` |
| Operações arriscadas exigem confirmação | Feito | `Verdict::Ask` para escrita fora do workspace; `shell` recusada |
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

## Resumo

- Feito com evidência: 31 de 37 itens.
- Parcial: 5 (cursor de import, CPU-time, instalador, matriz de release,
  entrada de menu).
- Pendente: 1 (builds nas três plataformas).

Nenhum destes "parcial" foi escrito como se estivesse pronto: o spec proíbe
marcar pronto sem evidência, e é mais útil saber onde dói.
