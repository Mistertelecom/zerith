---
name: systematic-debugging
description: "Use quando encontrar um bug, teste falhando, travamento, crash ou comportamento inesperado em qualquer linguagem (Rust, TypeScript/JS, Python, Go, C#, Java, C/C++). Reproduz primeiro, isola a causa raiz com evidência real, cria teste de regressão e verifica sem esconder falhas."
category: software-development
tags: debug debugging bug crash hang travamento erro stacktrace fix test
---

# Debug Sistemático

Nunca corrija sintomas no chute. Sem causa raiz comprovada, não edite código de produção.

## 1. Reproduza de forma confiável
- Rode o comando exato que falha **sem** `| tail` nem `| head` (pipes escondem o código de saída e retêm stdout em caso de travamento).
- Em testes de GUI ou processos que podem travar, passe `timeout_secs` curto (`20`–`30`s) e verifique diálogos modais (`messagebox`, `input()`, prompts interativos).
- Leia o **primeiro** erro real do stack trace ou compilador — erros posteriores quase sempre são cascata.

## 2. Mapeie antes de alterar
- Use `repo_map` (com `focus` no módulo afetado) e `grep` para localizar todos os pontos que tocam o estado problemático.
- Use `lsp` (`definition`, `references`, `diagnostics`) quando disponível em vez de adivinhar tipos ou assinaturas.
- Leia os trechos relevantes completos com `read_file` antes de propor hipótese.

## 3. Isole a causa raiz
- Formule **uma** hipótese verificável por vez e confirme com evidência (teste mínimo, log pontual ou inspeção direta do estado).
- Se 2 tentativas falharem sem explicar o porquê, pare, questione suas premissas e verifique entradas, encoding, concorrência, caminhos Windows/Unix e estado persistido.

## 4. Corrija com teste de regressão
- Prefira edições cirúrgicas (`edit_file`, `multi_edit`, `apply_patch`) sem refatorações paralelas fora do escopo.
- Escreva ou atualize um teste automatizado que **falha sem o fix e passa com o fix**.

## 5. Verifique de ponta a ponta
- Rode `diagnostics` + a suíte de testes/build do projeto diretamente (exit 0 real).
- Relate objetivamente: causa raiz (arquivo e linha), mudança feita e exatamente qual comando passou.
