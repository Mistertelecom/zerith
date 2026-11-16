# Zerith — núcleo Rust

Workspace Rust do Zerith. Fica **separado** do workspace do OpenHuman (a raiz
do repositório) de propósito: o fork usa `[patch]` apontando para `vendor/` e é
um pnpm workspace na raiz; misturar crates novos nele obrigaria a editar tabelas
de patch de terceiros.

## Crates

| Crate | Papel |
| --- | --- |
| `zerith-core` | Identidade, `Paths` (por SO), erros |
| `zerith-sessions` | SQLite, migrações versionadas, sessões e mensagens |
| `zerith-memory` | `MemoryBackend` + `SqliteMemory` (FTS5) + extração determinística |
| `zerith-providers` | Endpoints OpenAI-compatible e Anthropic (9Router, Ollama, LM Studio) |
| `zerith-runtime` | Um turno de agente: histórico → provider → mensagem gravada |
| `zerith-importers` | Import read-only do Hermes (`~/.hermes/state.db`) |
| `zerith-cli` | Binário `zerith` |
| `zerith-ui` | Janela nativa em Rune (chat + lista de sessões) |

Um banco só: `zerith-sessions` é dono do schema; `zerith-memory` usa o mesmo
arquivo e as mesmas migrações.

## Comandos

```bash
cd zerith
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo run --bin zerith -- doctor
```

## Onde ficam os dados

| SO | Raiz |
| --- | --- |
| Windows | `%APPDATA%\Zerith` |
| macOS | `~/Library/Application Support/Zerith` |
| Linux | `$XDG_DATA_HOME/zerith` (ou `~/.local/share/zerith`) |

`ZERITH_HOME` sobrepõe tudo (testes, instalação portátil).

## CLI

```text
zerith version
zerith paths
zerith doctor                       # abre o banco, roda integrity_check
zerith session list [--limit N]
zerith session new <title>
zerith session show <id>
zerith memory remember <scope> <text> [--inferred]
zerith memory recall <scope> <query>
zerith memory list <scope>
zerith memory forget <id>
zerith providers [probe|models|chat <model> <prompt>]
zerith import detect
zerith import hermes [--profile <name>]
zerith chat [--session <id>] [--model <id>] <prompt>
```

## Providers

A escolha do endpoint vem do ambiente, um leitor só
(`ProviderConfig::from_env`), usado pelo CLI e pela janela:

| Variável | Efeito |
| --- | --- |
| `ZERITH_PROVIDER` | `9router` (padrão), `openai`, `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`, `lmstudio`, `custom` |
| `ZERITH_BASE_URL` | Sobrescreve a URL do endpoint |
| `ZERITH_API_KEY` | Chave; exigida só no chat, não em `probe`/`models` |
| `ZERITH_MODEL` | Modelo padrão do chat |
| `ZERITH_USER` | Dono do escopo de memória (padrão: usuário do SO) |

`probe` e `models` funcionam sem chave de propósito: um servidor local
(LM Studio, Ollama, 9Router) que lista modelos não deve ser escondido por falta
de credencial que ele nem pede.

## Planos e subagentes

Um plano é uma lista de tarefas (`título | prompt`, uma por linha) gravada em
`agent_tasks` antes de rodar. Cada tarefa vira uma sessão própria, então o que
um subagente fez é uma conversa legível. `zerith plan run <arquivo|-> --agents N`
respeita dependências em cadeia, limita a concorrência a 8, marca `skipped`
quando a dependência falhou e devolve tarefas `running` para `pending` no
início (crash não trava o plano). `zerith plan list` mostra o que ficou pela
metade; a janela tem o painel "Execução".

## Skills e MCP

### Skills

Formato Agent Skills (`SKILL.md` com frontmatter YAML). O Zerith **lê e
valida**, nunca executa nada na instalação: `zerith skills validate <dir>`
mostra inventário, scripts encontrados e flags (`curl`, `| sh`, `rm -rf`,
`Invoke-Expression`). A janela lista o que está em `<root>/skills`.

### MCP

Cliente stdio, um objeto JSON-RPC por linha, sem shell na linha de comando
do servidor. `zerith mcp tools <cmd...>` completa o handshake e lista as
ferramentas; `zerith mcp call <cmd...> -- <tool> <json>` executa uma. O
processo filho é morto quando o cliente sai.

## Memória

Fatos vivem no mesmo banco das sessões, com escopo (`user:<id>`), tipo
(`fact`, `preference`, `decision`, `instruction`, `entity`, `project`) e
confiança (`confirmed` = o usuário disse; `inferred` = o sistema concluiu).
Os dois nunca são exibidos como se fossem o mesmo.

Captura: só o que o usuário pede explicitamente — `lembre que …`,
`lembre-se que …`, `remember that …`. Sem marcador, nada é gravado; adivinhar
seria transformar inferência em memória confirmada.

Recall: uma vez por turno, contra o texto do usuário, com orçamento de
`MEMORY_BUDGET_CHARS = 2000` caracteres e no máximo `MEMORY_HITS = 12` fatos. O
bloco entra **depois** do prompt fixo do sistema, preservando o prefix cache do
provider. Falha de recall não derruba o turno.

Administração: `zerith memory list|recall|forget` no CLI e o painel **Memória**
na janela, onde o botão `x` apaga o fato (tombstone: o texto é zerado e a chave
de dedupe não o ressuscita).

## Janela

```bash
cargo run -p zerith-ui              # abre a janela
cargo run -p zerith-ui -- --snap ui.png   # render headless, sem display
cargo run -p zerith-ui -- --tree          # árvore de elementos, sem display
```

A janela desenha em Rune — sem webview, sem Chromium, sem JavaScript. O turno
roda em thread de trabalho e volta por `AppHandle::update`, então um modelo
lento nunca trava o frame.

## Testes

`cargo test --workspace --exclude zerith-ui` — a UI fica de fora do caminho
padrão porque compila o Rune (wgpu, winit) e leva minutos. Ela tem testes
próprios (`cargo test -p zerith-ui`, que não abrem janela) e é verificada de
ponta a ponta pelos modos headless acima.

Antes de qualquer push: `cargo fmt --all --check`,
`cargo clippy --workspace --exclude zerith-ui --all-targets -j 1 -- -D warnings`
e `cargo clippy -p zerith-ui --all-targets -j 1 -- -D warnings`.

O CI (`.github/workflows/zerith.yml`) roda os três em Linux e Windows, mais um
job que renderiza `--tree` e publica a árvore como artefato.

Detalhes de toolchain, memória e caminhos: [docs/build.md](docs/build.md).

## Nota de build no Windows

`[profile.dev] debug = false` está ligado. O toolset MSVC desta máquina falha
com `cl.exe D8050` ao compilar o SQLite bundled com `/Z7`. Sem debug info o
build passa; o custo é backtrace sem números de linha em dev. Remova a seção
quando o toolset for atualizado.
