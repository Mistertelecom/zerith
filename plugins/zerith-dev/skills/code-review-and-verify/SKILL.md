---
name: code-review-and-verify
description: "Use antes de finalizar uma implementação, abrir PR ou fazer commit, ou quando o usuário pedir revisão de código, auditoria de diff ou verificação de qualidade. Revisa bugs reais, bordas, segurança, compatibilidade cross-platform e valida com diagnostics e testes reais."
category: software-development
tags: review code-review diff audit verify pr commit quality security refactor
---

# Revisão de Código e Verificação Antes de Concluir

## 1. Inspecione o diff real
- Rode `git status -s` e `git diff` (ou leia os arquivos alterados) para confirmar exatamente o que mudou.
- Remova sobras de debug, arquivos temporários, variáveis não usadas e alterações acidentais fora do escopo pedido.

## 2. Checklist de revisão (foco em bugs reais, não nitpicks)
- **Corretude e bordas:** listas vazias, nulos/None, divisão por zero, UTF-8 multibyte, caminhos com espaços ou `\` no Windows, race conditions e limpeza de recursos/handles.
- **Segurança:** validação de entrada, caminhos (`..` / traversal), comandos shell sem injeção, nenhum segredo/token/caminho pessoal hardcoded em código ou logs.
- **Contratos e compatibilidade:** serialização retrocompatível (`serde(default)`, campos opcionais), contratos entre backend e frontend sincronizados, blocos `#[cfg(unix)]` / `#[cfg(windows)]` válidos nas duas plataformas.
- **UI/Frontend (quando aplicável):** hooks React sempre acima de returns condicionais, uso exclusivo de tokens de tema (sem hex/rgb soltos se o projeto usar tokens), acessibilidade e estados de erro.

## 3. Verificação objetiva (nunca assuma que funciona)
- Execute `diagnostics` para checar tipos e compilação do projeto (e subprojetos em monorepos).
- Rode a suíte de testes relevante e o build **sem** pipes (`| tail` / `| head`).
- Só declare "pronto" depois de ver `exit 0` real no comando executado após a última edição. Se algo não pôde ser executado no ambiente, diga explicitamente o que não foi verificado.
