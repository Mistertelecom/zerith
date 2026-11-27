---
name: gui-engineering
description: "Use ao criar, redesenhar ou corrigir interfaces gráficas (Web: React, Vue, Svelte, HTML/CSS, Tailwind, Tauri; Desktop: Tkinter, CustomTkinter, PyQt/PySide, WPF/WinForms, Electron). Garante design intencional sem cara genérica de IA, responsividade, thread de UI livre de I/O e verificação visual real."
category: software-development
tags: gui ui ux frontend interface design web desktop react tkinter pyqt css layout visual
---

# Engenharia de Interfaces Gráficas (Web e Desktop)

## 1. Direção visual antes do código (anti-slop)
- Defina primeiro **tokens claros**: superfícies (base, elevada, borda sutil), hierarquia tipográfica (3–4 níveis no máximo), escala de espaçamento consistente e **uma única cor de destaque** reservada para ação primária e foco.
- Evite vícios genéricos de IA: gradientes neon sem propósito, excesso de cards aninhados, sombras pesadas, badges coloridos demais e botões competindo pelo mesmo peso visual.
- Respeite rigorosamente o design system existente quando editar um projeto já iniciado (tokens CSS, componentes base, biblioteca de ícones).

## 2. Ergonomia e estados completos
Toda tela funcional deve cobrir os 4 estados:
1. **Carregando** (esqueleto ou spinner discreto, nunca tela branca piscando).
2. **Vazio** (mensagem útil + ação clara).
3. **Erro** (causa legível + botão de tentar novamente; sem bloquear a interface inteira).
4. **Sucesso / Dados densos** (truncamento elegante `ellipsis`, scroll contido, `tabular-nums` em números e alinhamento consistente).
- Garanta foco visível (`:focus-visible`), contraste legível em tema claro/escuro, navegação por teclado (`Esc`, `Enter`, `Tab`) e redimensionamento sem quebrar botões ou cortar texto.

## 3. Regras específicas para GUI Desktop (Tkinter, PyQt/PySide, WPF, Electron, Tauri)
- **Nunca faça rede, SSH, Telnet, disco pesado ou subprocessos na thread da UI:** rode em worker thread / async e atualize a interface via fila (`queue.Queue` + `after`, signals/slots ou dispatcher).
- **DPI e janelas:** defina tamanho mínimo (`minsize`), layout responsivo (`grid` com `weight` / layouts flexíveis) e suporte a escala de DPI no Windows.
- **Testes automatizados de GUI:**
  - Sempre injete/mocke hosts de rede para `127.0.0.1` e **mocke diálogos modais bloqueantes** (`messagebox.showerror`, `askyesno`, `QMessageBox`) para o teste nunca congelar esperando clique humano.
  - Rode testes de GUI com `timeout_secs` curto (`20`–`30`s) e **sem** `| tail`.
  - Se uma janela Desktop travar ou você precisar conferir o que está na tela do PC, use `desktop_inspect{action: "windows"}` (lista janelas e textos de diálogos modais `#32770` em texto puro) ou `desktop_inspect{action: "screenshot", title: "..."}` (captura PNG analisado por visão).
  - Para abrir a janela de verdade para o usuário usar, execute com `shell{run_in_background: true}` uma única vez.

## 4. Verificação visual obrigatória em interfaces Web
1. Suba o dev server com `shell{run_in_background: true}` (se ainda não estiver rodando).
2. Prefira **`browser{action: "batch", steps: [...]}`** para juntar `navigate` → `console` → `viewport` → `screenshot` → `eval` em 1 ou 2 turnos em vez de uma chamada por ação:
   - Abra a URL com `navigate`, cheque erros com `console`, tire print com `screenshot` (`full_page: true` quando necessário) e inspecione overflow/alinhamento com `eval`.
   - Teste responsividade alternando `viewport` entre mobile (`width: 390, height: 844`) e desktop (`width: 1440, height: 900`), e `reset: true` ao terminar.
3. Corrija qualquer corte, sobreposição, contraste fraco ou desalinhamento visível e faça uma nova verificação em lote confirmando a correção.
