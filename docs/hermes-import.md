# Importar do Hermes Agent

Read-only: o banco de origem é aberto só para leitura e nunca é alterado.

```bash
zerith import detect                    # procura ~/.hermes/state.db (e perfis)
zerith import hermes [--profile <nome>]
```

- Schema verificado contra hermes-agent rev `4162ecf1`.
- Colunas desconhecidas são preservadas em `raw`.
- Repetir não duplica: chave única por (`source_id`, perfil).
- Retomada: a repetição completa sessões que faltaram; **não há cursor por
  mensagem**.

`zerith-importers` também tem leitor de JSONL (`hermes sessions export`) como
biblioteca; o comando de CLI para ele não está neste documento porque não
foi verificado aqui.

Relatório: sessões vistas/importadas/puladas, mensagens importadas/puladas,
falhas (uma por linha em stderr).
