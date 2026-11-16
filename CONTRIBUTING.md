# Contribuir

## Antes do push

```bash
cd zerith
cargo fmt --all --check
cargo clippy --workspace --exclude zerith-ui --all-targets -- -D warnings
cargo test --workspace --exclude zerith-ui
cargo clippy -p zerith-ui --all-targets -- -D warnings   # compila Rune, demora
```

## Estilo

- Comentários em inglês; textos para o usuário em pt-BR.
- stdlib primeiro. Dependência nova só com justificativa no `Cargo.toml`
  (ver os comentários de `toml` e `keyring`). Nada de tokio/reqwest.
- Simplificação deliberada leva comentário `ponytail:` dizendo o limite e
  o caminho de upgrade.
- API pública de `zerith-core` só cresce (todo crate depende dele).

## Testes

- Sem `%TEMP%`/`/tmp` (ACL restrita em algumas máquinas): use
  `tempfile::Builder::new().tempdir_in(...)` sob `target/test-scratch` ou
  `CARGO_TARGET_TMPDIR`.
- Não altere variáveis de ambiente em testes paralelos: injete (`load_with_env`,
  `resolve_api_key_with`).
- Nada de rede ou cofre real em teste unitário.

Mais em [docs/testing.md](docs/testing.md).

## Docs

Só descreva o que existe. O que é plano vai marcado como **planejado**.
`docs/aceite.md` exige evidência executada para "Feito".

## Licença

Contribuições entram sob GPL-3.0-only. Atualize
`DEPENDENCY_LICENSES.md` (`python scripts/dependency-licenses.py`) ao mudar
dependências.
