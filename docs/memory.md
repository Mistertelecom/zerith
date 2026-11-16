# Memória

Fatos no mesmo arquivo SQLite das sessões (`zerith-memory`, FTS5 + bm25).

- **Escopo:** `user:<ZERITH_USER>` ou `user:local` (`zerith_memory::default_scope`),
  igual no CLI e na janela.
- **Tipo:** `fact`, `preference`, `decision`, `instruction`, `entity`, `project`.
- **Confiança:** `confirmed` (o usuário disse) ou `inferred`; nunca exibidos
  como a mesma coisa.

## Captura

Só pedido explícito: `lembre que …`, `lembre-se que …`, `remember that …`.

## Recall

Uma vez por turno, até 12 fatos e 2000 caracteres, inserido depois do
prompt fixo. Falha de recall não derruba o turno.

`[memory] paused = true` em `settings.toml` desliga a memória em `zerith -p`.
`budget_chars` é lido, mas o runtime ainda usa a constante
`MEMORY_BUDGET_CHARS = 2000` (**planejado**).

## Administrar

```bash
zerith memory list user:local
zerith memory recall user:local "editor"
zerith memory edit <id> "novo texto"
zerith memory forget <id>          # tombstone: não ressuscita
zerith memory export user:local
```

A janela tem o painel **Memória**.
