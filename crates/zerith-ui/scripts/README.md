# Roteiros de entrada da janela

Entrada real (mouse e teclado) dirigida pelo modo headless do Rune, para
conferir o que testes de estado não alcançam: hover, anel de foco e atalhos
chegando pela fila de eventos da janela.

```powershell
$env:ZERITH_HOME = "$PWD\target\zhome"
.\target\debug\zerith-ui.exe --scene chat --theme light `
  --script crates\zerith-ui\scripts\focus.json --at 1.0 --snap target\snap\focus.png
```

| Roteiro | O que deve aparecer |
| --- | --- |
| `hover.json` | fundo suave em "Memória" na navegação |
| `focus.json` | anel de foco azul no item de navegação focado pelo Tab |
| `shortcuts.json` | Ctrl+5 abre Memória e Ctrl+K põe o cursor na busca |

O card de aprovação com um modelo de verdade é coberto por
`crates/zerith-runtime/tests/live_approval.rs` (ignorado por padrão).
