# Skills

Formato Agent Skills: diretório com `SKILL.md` (frontmatter `name`,
`description`, opcionais `license`, `compatibility`). Ficam em
`<raiz de dados>/skills`.

```bash
zerith skills validate <dir>     # inventário, scripts (não executados), alertas
zerith skills install <dir>
zerith skills enable|disable <nome>
zerith skills update <nome> <dir>
zerith skills rollback <nome>
zerith skills remove <nome>
zerith skills catalog [--refresh]  # repos reais do GitHub; GITHUB_TOKEN opcional
```

- Nada é executado na instalação. Alertas para `curl`, `| sh`, `rm -rf`,
  `Invoke-Expression`.
- No prompt entram só nome + descrição (orçamento `SKILLS_BUDGET_CHARS = 3000`);
  o corpo só quando o usuário nomeia a skill.
- O catálogo grava `fetched_at` e a URL de cada repositório.
