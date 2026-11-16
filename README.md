# Zerith

Harness de agentes de IA nativo, escrito em Rust: chat com ferramentas e
aprovação humana, memória local, skills, MCP, planos com subagentes e uma
janela desktop desenhada pela GPU (Rune), sem webview, Electron ou Tauri.

O Zerith é um projeto próprio. Estudou ideias de outros harnesses (OpenHuman,
Claude Code, DeepSeek Harness, Hermes) e reimplementou em Rust o que fazia
sentido; nenhum deles é dependência em tempo de execução. As referências e o
que veio de cada uma estão em [UPSTREAMS.md](UPSTREAMS.md).

## Crates

| Crate | Papel |
| --- | --- |
| `zerith-core` | Identidade, `Paths` (por SO), erros |
| `zerith-sessions` | SQLite, migrações versionadas, sessões e mensagens |
| `zerith-memory` | `MemoryBackend` + `SqliteMemory` (FTS5) + extração determinística |
| `zerith-providers` | Endpoints OpenAI-compatible e Anthropic (9Router, Ollama, LM Studio) |
| `zerith-runtime` | Um turno de agente: histórico → provider → mensagem gravada |
| `zerith-importers` | Import read-only do Hermes (`~/.hermes/state.db`) |
| `zerith-skills` | Formato Agent Skills, catálogo GitHub, instalação com rollback |
| `zerith-mcp` | Cliente MCP sobre stdio (JSON-RPC 2.0) |
| `zerith-tools` | Registry de ferramentas com permissões, timeouts e política |
| `zerith-plugins` | Manifestos de plugin, ativação e revisão explícita |
| `zerith-security` | Presets de sandbox, vereditos, redação de segredos |
| `zerith-cli` | Binário `zerith` |
| `zerith-ui` | Janela nativa em Rune: chat, histórico, agentes, skills, memória, provedores, configurações |

Um banco só: `zerith-sessions` é dono do schema; `zerith-memory` usa o mesmo
arquivo e as mesmas migrações.

## Comandos

```bash
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

## Permissões, segurança e benchmark

`zerith policy` mostra preset (`read-only`, `workspace-write`,
`danger-full-access`), nível de enforcement e o veredito para um caminho
(`zerith policy check <path>`). O enforcement é reportado como `partial`
porque o host ainda não tem jail próprio: preset não é garantia. Erros são
redigidos antes de ir para stderr (chaves, headers, hex longo).

`zerith bench` mede abertura+migração, 1000 appends, paginação de 50 e recall
FTS5 sobre 200 fatos, num banco descartável. `zerith backup` usa a API de
backup do SQLite (nunca cópia de arquivo WAL vivo) e `zerith restore` valida
`integrity_check` e guarda o banco atual antes de substituir. O turno pode ser
cancelado: a janela troca "enviar" por "parar" e o token chega ao loop de rede.

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

## Ferramentas e plugins

O registry (`zerith-tools`) declara, por ferramenta, o que o spec exige:
nome, descrição, schema de parâmetros, resultado esperado, permissão,
timeout, se é cancelável, se é local e a origem. `zerith tools` imprime tudo
isso — uma ferramenta que esconde o timeout fica visível na hora.

| Ferramenta | Permissão | Timeout | Observação |
| --- | --- | --- | --- |
| `read_file` | read | 5 s | dentro do workspace |
| `write_file` | write | 5 s | fora do workspace exige aprovação |
| `list_dir` | read | 5 s | diretórios terminam em `/` |
| `memory_recall` | read | 2 s | `[confirmed|inferred] texto` |
| `shell` | shell | 30 s | **recusada**: o host não tem jail |
| `mcp_call` | network | 60 s | recusada sem transporte conectado |

`shell` é recusada mesmo em `danger-full-access`, e o motivo é o do spec: sem
sandbox real, "permitido" seria uma promessa que o host não cumpre. O caminho
para executar comando é MCP ou plugin, que rodam em outro processo.

### Plugins

Um plugin é um diretório com `plugin.json` (nome, versão, `command`, `args`).
Descobrir plugin lê o manifesto e **nada mais**: instalar não executa e não
confia. `plugins install` grava como `disabled` + `unreviewed`; habilitar não
basta — `plugins trust` marca a revisão humana, e só então `plugins tools`
abre o processo. Um plugin que falha morre no próprio processo, como um
servidor MCP, e não derruba a aplicação.

```bash
zerith plugins install ./meu-plugin   # disabled, unreviewed
zerith plugins list
zerith plugins enable meu-plugin      # ainda não executa
zerith plugins trust meu-plugin       # revisão explícita
zerith plugins tools meu-plugin       # agora inicia e lista ferramentas
zerith plugins disable meu-plugin     # desativa sem apagar
zerith plugins remove meu-plugin
```

Isso não é sandbox: o plugin roda com os privilégios do usuário. O manifesto
também não é autorização — é metadado.
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

## Aceite e release

`docs/aceite.md` traz cada critério da seção 21 do spec com o estado real e a
evidência; `docs/release.md` cobre instalação, desinstalação, atualização,
benchmarks e o que ainda falta (assinatura, instaladores, matriz executada).

## Nota de build no Windows

`[profile.dev] debug = false` está ligado. O toolset MSVC desta máquina falha
com `cl.exe D8050` ao compilar o SQLite bundled com `/Z7`. Sem debug info o
build passa; o custo é backtrace sem números de linha em dev. Remova a seção
quando o toolset for atualizado.
