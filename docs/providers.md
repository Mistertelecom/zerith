# Providers

Dois protocolos: OpenAI-compatible (`POST {base}/chat/completions`) e
Anthropic (`POST {base}/messages`). Tipos conhecidos com URL padrão:

| `ZERITH_PROVIDER` | URL padrão | Chave |
| --- | --- | --- |
| `9router` (padrão) | `http://localhost:20128/v1` | sim |
| `openai` | `https://api.openai.com/v1` | sim |
| `anthropic` | `https://api.anthropic.com/v1` | sim |
| `deepseek` | `https://api.deepseek.com/v1` | sim |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/openai` | sim |
| `openrouter` | `https://openrouter.ai/api/v1` | sim |
| `ollama` | `http://localhost:11434/v1` | não |
| `lmstudio` | `http://localhost:1234/v1` | não |
| `custom` | defina `ZERITH_BASE_URL` | sim |

## Chave de API

Ordem: `ZERITH_API_KEY` → cofre do SO (serviço `zerith`, entrada
`api_key_ref` do provider em `settings.toml`, ou o nome do provider).

```bash
zerith secret set openai          # lê o valor do stdin (fora do histórico do shell)
zerith secret get openai          # mostra mascarado: sk-a…wxyz
zerith secret delete openai
```

Sem cofre disponível: erro explícito, nada em texto puro. `zerith chat` e
`zerith -p` usam o cofre; `zerith providers probe|models` continuam só com
env (servidor local não precisa de chave).

## Modelo

`--model` > `ZERITH_MODEL` > `model` em `settings.toml`.

## Ainda não

- `[[providers]]` em `settings.toml` é lido, mas a escolha de
  `protocol`/`base_url`/`timeout_ms`/`context_limit` ainda vem de
  `ProviderConfig::from_env` (**planejado**).
