# 9Router

Provider padrão. O Zerith fala com o 9Router só pela API HTTP
OpenAI-compatible local; não lê o banco nem o frontend dele.

```bash
# 9Router rodando em localhost:20128
zerith providers probe            # ok  http 200  ... models
zerith providers models
zerith secret set 9router         # se o 9Router exigir chave
ZERITH_MODEL=<id> zerith chat "oi"
```

Outra porta: `ZERITH_BASE_URL=http://localhost:<porta>/v1`.

Evidência registrada em [aceite.md](aceite.md): `providers probe` →
`http 200, 59 models`; turno real com streaming e `reasoning_content`.

Problemas: ver [troubleshooting.md](troubleshooting.md).
