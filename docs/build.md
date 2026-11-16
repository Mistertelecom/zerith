# Build e execução

Tudo abaixo roda da raiz do repositório, que é o workspace Cargo.

## Requisitos

| Item | Versão |
| --- | --- |
| Rust | 1.99.1 (`rust-toolchain.toml`) |
| Toolchain | MSVC no Windows; GNU precisa de MinGW binutils |
| SQLite | embutido (`rusqlite` com `bundled`) — nada a instalar |
| GPU | Vulkan/DX12/Metal via wgpu, só para a janela |

## Comandos

```bash
cd zerith
cargo build -j 1                       # núcleo, CLI e providers
cargo build -p zerith-ui -j 1          # janela Rune (compila wgpu/winit)
cargo test --workspace --exclude zerith-ui
cargo clippy --workspace --exclude zerith-ui --all-targets -j 1 -- -D warnings
cargo fmt --all --check
```

`-j 1` não é preferência: nesta máquina o build paralelo estoura a memória e
o rustc reporta erros falsos de toolchain (`only metadata stub found for rlib
dependency core`, `failed to mmap rmeta ... os error 1455`). Com `-j 1` passa.
`cargo clean` não resolve esses erros — é memória, não cache.

## Modos headless da janela

```bash
cargo run -p zerith-ui -- --tree            # árvore de elementos
cargo run -p zerith-ui -- --snap ui.png     # render para PNG
```

Servem para provar que a UI desenha sem display — é assim que a janela é
verificada em CI.

## Configuração

| Variável | Efeito |
| --- | --- |
| `ZERITH_HOME` | Sobrescreve a raiz de dados (testes, instalação portátil) |
| `ZERITH_PROVIDER` | `openai` (padrão), `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`, `lmstudio`, `custom` |
| `ZERITH_BASE_URL` | URL do endpoint |
| `ZERITH_API_KEY` | Chave do provider |
| `ZERITH_MODEL` | Modelo padrão do chat |
| `ZERITH_ACCOUNT_ID` | ID explícito da conta para atribuir uso/roteamento |
| `ZERITH_SUBAGENT_MODEL` | Sobrescreve `[routing] subagent_model` |
| `ZERITH_PONYTAIL` | Nível Ponytail (`off`, `lite`, `full`, `ultra`) |
| `ZERITH_CAVEMAN` | Nível Caveman (`off`, `lite`, `full`, `ultra`, `wenyan`) |
| `ZERITH_USER` | Identidade do escopo de memória (`user:<id>`) |
| `ZERITH_SANDBOX` | `read-only`, `workspace-write` (padrão) ou `danger-full-access` |

No Windows, `ZERITH_HOME` precisa ser caminho Windows (`C:\...`), não MSYS
(`/c/...`): o SQLite devolve `Acesso negado (os error 5)` no caminho MSYS.

## Uso

```bash
zerith doctor                     # banco, schema, integrity_check, contagens
zerith accounts list              # contas do gateway local e prioridades
zerith routing show               # orquestrador, subagentes e combos
zerith tokensaver show            # níveis Ponytail/Caveman/RTK e economia
zerith usage [--hours 24]         # conta/modelo ativo, cota restante e economia
zerith quota [--hours 24]         # alias de `usage`
zerith chat "pergunta"            # turno com streaming, memória e persistência
zerith session list               # conversas
zerith memory list user:joao      # fatos confirmados e inferidos
zerith memory edit <id> "novo texto"
zerith memory export user:joao    # JSON de tudo no escopo
zerith skills list                # SKILL.md encontradas, nada executado
zerith skills catalog [--refresh] # estrelas, licenças e datas do GitHub
zerith tools                      # catálogo com permissões e timeouts
zerith tools call read_file '{"path":"docs/build.md"}'
zerith plugin list               # metadados; nada é executado
zerith mcp list                   # servidores configurados
zerith mcp tools <servidor>       # handshake MCP e lista de ferramentas
zerith checkpoints <sessão>       # pontos de rewind
zerith rewind <sessão> --both     # desfaz arquivos e conversa de um turno
zerith doctor --offline           # diagnóstico sem tocar a rede
zerith plan run plano.txt --agents 2
zerith policy check <caminho>     # veredito do preset ativo
zerith bench                      # números em banco descartável
zerith backup                     # cópia consistente via API do SQLite
zerith restore <arquivo>
zerith import hermes              # read-only, idempotente
```

## Artefatos

| Saída | Caminho |
| --- | --- |
| CLI | `target/debug/zerith.exe` |
| Janela | `target/debug/zerith-ui.exe` |
| Banco | `%APPDATA%\Zerith\zerith.db` |

O banco é criado no primeiro comando. `zerith doctor` abre, roda
`integrity_check` e imprime a versão do schema.

## Release

Uma tag `zerith-v*` dispara `.github/workflows/zerith-release.yml`, que
compila `cargo build --release -p zerith-cli -p zerith-ui` em Windows, Linux e
macOS e empacota (zip no Windows, `tar.gz` nos outros, com `.sha256`). O
workflow **nunca foi executado**; ver `docs/release.md`. Localmente:
`cargo build --release -p zerith-cli -p zerith-ui -j 1`, com as mesmas `[patch]`
de `vendor/` — um `[patch]` de dependência não se aplica ao workspace que
depende dela, então as cópias corrigidas do Rune precisam ficar aqui.

## Segmentos que dependem do SO

- Ctrl+C/SIGTERM: `zerith -p` instala um tratador sem dependência
  (`SetConsoleCtrlHandler` no Windows, `signal` no Unix) e sai com 130 ou 143.
  Compilado e testado só no Windows; o ramo Unix **não foi compilado aqui**.
- Testes de MCP por stdio usam PowerShell e só rodam no Windows; os de HTTP e
  os do adaptador rodam em qualquer SO.
