# Permissões

## O que é aplicado hoje

`zerith-security` define três presets (`ZERITH_SANDBOX` ou
`permissions.mode`): `read-only`, `workspace-write` (padrão),
`danger-full-access`. `zerith-tools` consulta `Policy::check` antes de:

- `write_file` — fora do workspace o veredito é `Ask`;
- `shell` — sempre recusada.

Limites conhecidos (marcados "Parcial" em [aceite.md](aceite.md)):

- `read_file` e `list_dir` **não** passam por `Policy::check`.
- `Verdict::Ask` é tratado como recusa (`needs approval: ...`); não há
  fluxo de aprovação.
- Enforcement é `partial`: checagem no host, não jail.

`zerith policy [check <path>]` mostra o preset e o veredito de escrita.

## Regras `allow` / `ask` / `deny` (biblioteca; planejado no runtime)

Em `settings.toml`:

```toml
[permissions]
allow = ["read_file", "shell(git status)"]
ask   = ["write_file"]
deny  = ["write_file(*.env)", "shell(rm *)"]
```

- `Ferramenta` casa qualquer chamada; `Ferramenta(padrão)` casa quando o
  argumento (caminho, comando, URL) casa o glob. Só `*` é curinga.
- As listas **somam** entre camadas (usuário + projeto + local).
- `Permissions::decide(tool, arg)` checa `deny` → `ask` → `allow`; um
  `deny` em qualquer camada não é desfeito por `allow` em outra. `None` =
  nenhuma regra casou, vale o preset.

**Planejado:** `zerith-tools` chamar `decide` antes do preset. Hoje as
regras são lidas e exibidas (`zerith config show`), mas não aplicadas.
