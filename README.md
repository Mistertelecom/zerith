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
| `zerith-core` | Identidade, `Paths` (por SO), erros, configurações em camadas (`accounts`, `routing`, `token_saver`) |
| `zerith-sessions` | SQLite, migrações versionadas (`V1`–`V7`), sessões, mensagens, eventos, checkpoints, rewind e rastreamento de uso/cotas/economia |
| `zerith-memory` | `MemoryBackend` + `SqliteMemory` (FTS5) + extração determinística |
| `zerith-providers` | Gateway nativo multi-conta com failover automático, presets nativos (`openai`, `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`, `lmstudio`) e `ModelSpec` |
| `zerith-runtime` | Um turno de agente, gateway + quota tracker, delegação multi-modelo de subagentes e Token Saver (`Ponytail`, `Caveman`, `RTK`) |
| `zerith-importers` | Import read-only do Hermes (`~/.hermes/state.db`) |
| `zerith-skills` | Formato Agent Skills, catálogo GitHub, instalação com rollback |
| `zerith-mcp` | Cliente MCP (stdio e HTTP), configuração `[[mcp.servers]]`, ferramentas `mcp__<servidor>__<ferramenta>` no laço do agente |
| `zerith-tools` | Registry de ferramentas com permissões, timeouts e política |
| `zerith-plugins` | Manifestos de plugin (inclui pacotes de skills/agentes/hooks/MCP), ativação e revisão explícita |
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
zerith version | paths | policy | bench | backup | restore <file>
zerith doctor [--offline]           # banco, configuração, sandbox, cofre, provider, MCP, plugins
zerith session list|new|show|rename
zerith search <texto>               # busca de texto completo nas mensagens
zerith export <sessão> [--format md|jsonl] [--out ARQ]
zerith checkpoints <sessão>         # pontos de restauração
zerith rewind <sessão> [--turn N] [--code|--conversation|--both] [--purge]
zerith chat [--resume [id|nome]|--continue|--session id] [--fork] [--model id] <prompt>
zerith -p <prompt> [--output-format text|json|stream-json] [--max-turns N]
        [--max-budget-usd X --price-in P --price-out P] [--allowed-tools ...]
        [--disallowed-tools ...] [--permission-mode MODO] [--json-schema ...]
zerith agents list
zerith skills list|install|update|remove|enable|disable|rollback|validate|catalog
zerith memory list|recall|remember|edit|forget|export|import|pause|resume|status
zerith mcp list|add|remove|approve|tools|call
zerith plugin list|show|install|enable|disable|trust|untrust|remove|tools
zerith providers [show|probe|models|chat <model> <prompt>]
zerith accounts [list|add <id> <provider> ...|remove|enable|disable|reset-cooldown <id>]
zerith routing [show|main|subagent|explore|plan <model>|role <agent> <model>|combo <name> <m1,m2,...>]
zerith tokensaver [show|ponytail <nível>|caveman <nível>|rtk <on|off>]
zerith usage [--hours N] | quota [--hours N]
zerith import detect | hermes [--profile P] [--jsonl ARQUIVO]
```

`<sessão>` aceita o id, o título (sem diferenciar maiúsculas) ou um prefixo de
id único com 4+ caracteres; título repetido é erro que lista os candidatos.

Códigos de saída: `0` sucesso; `1` erro, schema não atendido, orçamento ou limite
de passos; `130` Ctrl+C e `143` SIGTERM/fechamento do console (um segundo sinal
encerra na hora). O cancelamento só é visto entre pedaços do stream e entre
chamadas de ferramenta.

## Providers e Gateway Local Nativo

O Zerith conecta diretamente aos provedores via Gateway Local Nativo Multi-Conta
(com suporte a combos, limites de cota, failover automático e delegação de
subagentes entre modelos/contas). Não depende de 9Router externo. Quando
variáveis de ambiente estiverem definidas, elas continuam no topo da precedência:

| Variável | Efeito |
| --- | --- |
| `ZERITH_PROVIDER` | `openai` (padrão), `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`, `lmstudio`, `custom` |
| `ZERITH_BASE_URL` | Sobrescreve a URL do endpoint |
| `ZERITH_API_KEY` | Chave; exigida só no chat, não em `probe`/`models` |
| `ZERITH_MODEL` | Modelo padrão do chat |
| `ZERITH_ACCOUNT_ID` | ID explícito da conta no gateway |
| `ZERITH_SUBAGENT_MODEL` | Sobrescreve `[routing] subagent_model` para subagentes |
| `ZERITH_PONYTAIL` | Nível do protocolo Ponytail (`off`, `lite`, `full`, `ultra`) |
| `ZERITH_CAVEMAN` | Nível do protocolo Caveman (`off`, `lite`, `full`, `ultra`, `wenyan`) |
| `ZERITH_USER` | Dono do escopo de memória (padrão: usuário do SO) |

`probe` e `models` funcionam sem chave de propósito: um servidor local
(LM Studio, Ollama) que lista modelos não deve ser escondido por falta de
credencial que ele nem pede. Detalhes do gateway, cotas e Token Saver estão em
[docs/providers.md](docs/providers.md).

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

Cliente stdio e HTTP, sem shell na linha de comando do servidor. Servidores de
`[[mcp.servers]]` viram ferramentas `mcp__<servidor>__<ferramenta>` no laço do
agente (hoje em `zerith -p`), com recursos `@servidor:recurso` e prompts como
comandos. Servidor definido por um projeto só roda depois de
`zerith mcp approve`. Detalhes e limites em [docs/mcp.md](docs/mcp.md).

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
zerith plugin install ./meu-plugin    # disabled, unreviewed
zerith plugin show meu-plugin         # o que ele executa
zerith plugin enable meu-plugin       # ainda não executa
zerith plugin trust meu-plugin --yes  # revisão explícita
zerith plugin tools meu-plugin        # agora inicia e lista ferramentas
zerith plugin disable meu-plugin      # desativa sem apagar
zerith plugin remove meu-plugin
```

Um plugin também pode ser um pacote de skills, agentes, hooks e servidores MCP
([docs/plugins.md](docs/plugins.md)). Isso não é sandbox: o plugin roda com os
privilégios do usuário. O manifesto também não é autorização — é metadado.

## Rewind

`zerith rewind` desfaz o que um turno fez nos arquivos e na conversa. Só vê
edições feitas pelas ferramentas de arquivo, nunca as de comandos de shell, e
sobrescreve o que está no disco. Limites e API em
[docs/checkpoints.md](docs/checkpoints.md).

## Método de agente

O mapa entre o método do Claude Code, a arquitetura do DeepSeek Harness e o
que o Zerith tem de fato (implementado x planejado) está em
[docs/agent-method.md](docs/agent-method.md).

## Capturas de tela

Ainda não há capturas versionadas. Caminhos reservados:
`docs/img/chat.png`, `docs/img/skills.png`, `docs/img/memoria.png`.
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
job que renderiza `--tree` e publica a árvore como artefato. Uma tag
`zerith-v*` dispara `.github/workflows/zerith-release.yml` (zip no Windows,
`tar.gz` no Linux e no macOS, com `.sha256`; sem instalador e sem assinatura;
ainda nunca executado).

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
