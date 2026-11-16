# Importar do Hermes Agent

Read-only: o banco de origem é aberto só para leitura e nunca é alterado.

```bash
zerith import detect                    # procura ~/.hermes/state.db (e perfis)
zerith import hermes [--profile <nome>]
zerith import hermes --jsonl export.jsonl [--profile <nome>]   # `hermes sessions export`
```

- Schema verificado contra hermes-agent rev `4162ecf1`.
- Colunas desconhecidas são preservadas em `raw`.
- Repetir não duplica: chave única por (`source_id`, perfil).
- Retomada: a repetição completa sessões que faltaram; **não há cursor por
  mensagem**.

`--jsonl` lê um arquivo de exportação: uma linha por sessão (com `messages`)
ou por mensagem, mesma chave de idempotência e `raw` para o que não é mapeado.
Sem `--profile`, o perfil gravado é `jsonl`. Cobertura: os testes de
`zerith-importers/tests/hermes_jsonl.rs`; **não** foi testado contra uma
exportação real do Hermes nesta máquina.

Relatório: sessões vistas/importadas/puladas, mensagens importadas/puladas,
falhas (uma por linha em stderr).
