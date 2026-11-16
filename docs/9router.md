# Gateway Nativo (substitui o antigo 9Router)

O Zerith não usa nem exige mais o proxy externo **9Router** (`localhost:20128/v1`). Toda a camada de roteamento, múltiplas contas por provedor, rastreamento de cota e failover automático passou a viver dentro do próprio Zerith (`zerith-providers::Gateway` + `zerith-sessions::usage` + `zerith-runtime::resolve_gateway`).

Veja o guia completo em [providers.md](providers.md).

## Resumo rápido

```bash
zerith accounts add claude-1 anthropic --priority 1 --quota-tokens 200000 --window-hours 5
zerith secret set claude-1
zerith accounts add ds-fast deepseek --priority 1 --model deepseek-4.1-flash
zerith secret set ds-fast
zerith routing main anthropic/claude-opus-5-5
zerith routing subagent deepseek/deepseek-4.1-flash
zerith usage --hours 24
```

## Compatibilidade com proxies locais / OpenAI-compatible

Se você ainda quiser apontar o Zerith para qualquer gateway HTTP OpenAI-compatible externo (vLLM, LiteLLM, proxy local próprio), basta usar `ZERITH_PROVIDER=custom` e `ZERITH_BASE_URL=http://localhost:<porta>/v1` (ou adicionar uma conta `custom` com `--url http://localhost:<porta>/v1`).

Problemas: ver [troubleshooting.md](troubleshooting.md).
