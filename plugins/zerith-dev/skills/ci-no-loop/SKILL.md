---
name: ci-no-loop
description: "Use quando o CI falhar (GitHub Actions, checks de PR vermelhos, build de release quebrado) ou quando o usuário pedir para acompanhar, consertar ou esperar o CI. Lê o log da falha com o gh, reproduz localmente, corrige, faz push e acompanha até passar."
---

# CI no loop (GitHub Actions)

Requer o `gh` autenticado. Confira com `gh auth status`. Se não estiver, peça ao usuário para rodar `gh auth login`.

## 1. Ache a falha
```bash
gh run list --limit 5                     # últimas execuções da branch atual
gh pr checks                              # checks do PR da branch, se houver
gh run view <run-id> --log-failed         # só os passos que falharam
```
Leia o **primeiro** erro real do log, não o último: os erros seguintes costumam ser consequência dele.

## 2. Reproduza localmente
- Rode o mesmo comando do passo que falhou. Ele está no `.github/workflows/*.yml`, em `run:`.
- Se só falha no CI, compare o ambiente: sistema (`runs-on`), versão da toolchain, variáveis, cache e features. Diga ao usuário qual diferença explica a falha.

## 3. Corrija e confirme
- Corrija a causa e rode de novo o comando local até passar.
- Faça commit e push só se o usuário já autorizou push nesta conversa. Senão, mostre o diff e peça.

## 4. Acompanhe até o fim
```bash
gh run watch <run-id> --exit-status      # bloqueia até terminar; código != 0 se falhar
```
Para não travar a conversa, rode em segundo plano com `shell` e `run_in_background: true` e leia depois com `shell_output`.
Se falhar de novo, volte ao passo 1. **Pare após 3 tentativas** sem progresso e explique ao usuário o que já tentou.

## Não faça
- Não desligue teste, lint ou passo do workflow para o CI ficar verde.
- Não use `--no-verify` nem force push sem o usuário pedir.
- Não rode `gh run rerun` em falha determinística: só em falha instável (rede, timeout, runner), e diga que foi rerun.
