# Testes

```bash
cargo test --workspace                       # todos os crates (incluindo zerith-desktop)
cargo test -p zerith-core -p zerith-cli
cargo test -p zerith-desktop
```

## Regras

- Diretórios temporários sob `target/test-scratch` ou `CARGO_TARGET_TMPDIR`,
  nunca `%TEMP%`/`/tmp` (ACL restrita em algumas máquinas).
- Não mexa no ambiente do processo em testes paralelos; use as variantes
  injetáveis: `Settings::load_with_env`, `secrets::resolve_api_key_with`
  (backend `SecretStore` em memória).
- Sem rede e sem cofre real em testes unitários.
- Hooks: comandos mínimos por SO com `#[cfg(windows)]` / `#[cfg(not(windows))]`.

## Cobertura de `zerith-core` / `zerith-cli`

| Área | Testes |
| --- | --- |
| Camadas de settings, concatenação, origem por chave | `settings::tests::layers_override_scalars_and_concatenate_arrays` |
| Arquivo corrompido: arquivo+linha, app segue | `settings::tests::a_corrupt_layer_is_reported_with_file_and_line_and_skipped` |
| `save_user` | `settings::tests::save_user_touches_only_the_user_layer_and_refuses_corrupt_files` |
| Ordem de resolução da chave | `secrets::tests::env_wins_then_keyring_then_none_and_failure_is_an_error` |
| Hooks: deny JSON, exit 2, timeout com kill, stdin, matcher | `hooks::tests::*` (5) |
| Instruções: subida até a raiz git, `AGENTS.md` > `CLAUDE.md`, limite | `instructions::tests::*` |
| Parsing do CLI (`chat`, `-p`, flags) | `zerith-cli` `tests::*` (4) |

O cofre real foi verificado à mão no Windows
(`zerith secret set/get/delete`), não em teste automatizado.

## Antes do push

Ver [../CONTRIBUTING.md](../CONTRIBUTING.md).
