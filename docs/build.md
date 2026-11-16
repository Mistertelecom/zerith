# Build e execução

Tudo abaixo roda de `zerith/`, a raiz do workspace Rust. O workspace do
OpenHuman na raiz do repositório não é tocado por estes comandos.

## Requisitos

| Item | Versão |
| --- | --- |
| Rust | 1.99.1 (`rust-toolchain` do upstream) |
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
| `ZERITH_PROVIDER` | `9router` (padrão), `openai`, `anthropic`, `deepseek`, `gemini`, `openrouter`, `ollama`, `lmstudio`, `custom` |
| `ZERITH_BASE_URL` | URL do endpoint |
| `ZERITH_API_KEY` | Chave do provider |
| `ZERITH_MODEL` | Modelo padrão do chat |
| `ZERITH_USER` | Identidade do escopo de memória (`user:<id>`) |
| `ZERITH_SANDBOX` | `read-only`, `workspace-write` (padrão) ou `danger-full-access` |

No Windows, `ZERITH_HOME` precisa ser caminho Windows (`C:\...`), não MSYS
(`/c/...`): o SQLite devolve `Acesso negado (os error 5)` no caminho MSYS.

## Uso

```bash
zerith doctor                     # banco, schema, integrity_check, contagens
zerith chat "pergunta"            # turno com streaming, memória e persistência
zerith session list               # conversas
zerith memory list user:joao      # fatos confirmados e inferidos
zerith skills list                # SKILL.md encontradas, nada executado
zerith mcp tools <cmd...>         # handshake MCP e lista de ferramentas
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
| CLI | `zerith/target/debug/zerith.exe` |
| Janela | `zerith/target/debug/zerith-ui.exe` |
| Banco | `%APPDATA%\Zerith\zerith.db` |

O banco é criado no primeiro comando. `zerith doctor` abre, roda
`integrity_check` e imprime a versão do schema.

## Release

Ainda não há pipeline de release. O caminho pretendido é `cargo build
--release -p zerith-cli -p zerith-ui -j 1` por plataforma, com as mesmas
`[patch]` de `zerith/vendor/` — um `[patch]` de dependência não se aplica ao
workspace que depende dela, então os forks do Rune precisam continuar aqui.
