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
| `zerith-memory` | `MemoryBackend` + `SqliteMemory` (FTS5) |
| `zerith-cli` | Binário `zerith` |

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
```

## Nota de build no Windows

`[profile.dev] debug = false` está ligado. O toolset MSVC desta máquina falha
com `cl.exe D8050` ao compilar o SQLite bundled com `/Z7`. Sem debug info o
build passa; o custo é backtrace sem números de linha em dev. Remova a seção
quando o toolset for atualizado.
