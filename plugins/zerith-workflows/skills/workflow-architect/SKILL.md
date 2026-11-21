---
name: workflow-architect
description: "Use quando o usuário pedir para criar, editar, agendar, testar ou automatizar workflows visuais, rotinas diárias/cron, relatórios agendados, alertas no WhatsApp (Evolution API/WAHA), Telegram, Discord, Slack ou monitoramentos NOC/SSH/HTTP/API no Zerith."
---

# Arquiteto de Workflows Globais do Zerith

Sempre que o usuário pedir uma automação, rotina agendada, monitoramento ou alerta em canais (WhatsApp, Telegram, Discord, Slack, Webhook ou arquivo), construa o fluxo diretamente usando as ferramentas nativas de workflow — ele aparecerá imediatamente no Canvas Visual do Zerith.

## 1. Fluxo Obrigatório de Trabalho

1. **Inspecione canais e fluxos existentes primeiro:**
   - Chame `channel_list` para descobrir quais canais globais o usuário já cadastrou (`evolution_api`, `waha`, `whatsapp_cloud`, `telegram`, `discord_webhook`, `slack_webhook`, `generic_webhook`, `desktop_notify`).
   - Chame `workflow_list` se o usuário quiser alterar ou evitar duplicar um workflow existente.
2. **Projete um grafo econômico em tokens e confiável:**
   - **Gatilho (`trigger_cron` ou `trigger_manual`):** ex. `"0 8 * * *"` para todo dia às 08:00 (`tz_offset_minutes: -180` para Horário de Brasília, salvo se o usuário informar outro fuso).
   - **Coleta zero-token (`shell`, `ssh_command`, `http_request`):** faça ping, consultas SNMP/API, scripts ou comandos SSH em equipamentos antes de chamar IA.
   - **Filtro/Extração zero-token (`transform`, `condition`):** use `transform` (`json_path`, `filter_lines`, `template`) e `condition` (`not_empty`, `contains`, `not_contains`, `equals`) com ramos `branch: "true"` / `"false"` para evitar chamadas desnecessárias à IA ou spam de alertas quando tudo estiver saudável.
   - **Análise opcional (`ai_agent`):** use `prompt` referenciando `{{prev}}` ou `{{node.<id>}}` apenas quando precisar interpretar logs ou redigir relatório executivo.
   - **Entrega (`notify_channel` ou `save_file`):** vincule o `channel_id` real retornado por `channel_list` (se ainda não existir canal configurado, deixe `channel_id: ""` e oriente o usuário a cadastrar o canal em **Workflows → Canais**).
3. **Salve e teste:**
   - Chame `workflow_create_or_update`. Por padrão salve com `enabled: true` se o usuário pediu para agendar/ativar imediatamente, ou `enabled: false` se ainda depender de configurar canal/credencial.
   - Quando seguro (ou quando houver nós de coleta/condição sem ações destrutivas), valide executando `workflow_run` e mostre o resumo real da execução.

## 2. Esquema Exato dos Nós

- `trigger_cron`: `{ "cron": "0 8 * * *", "tz_offset_minutes": -180 }`
- `trigger_manual`: `{}`
- `shell`: `{ "command": "...", "timeout_secs": 30 }`
- `ssh_command`: `{ "host": "192.168.1.1", "user": "root", "port": 22, "command": "...", "timeout_secs": 30 }`
- `http_request`: `{ "method": "GET", "url": "https://...", "headers": {}, "body": "", "allow_error_status": true }`
- `transform`: `{ "mode": "json_path" | "filter_lines" | "template", "pattern": "...", "template": "{{prev}}" }`
- `condition`: `{ "mode": "not_empty" | "contains" | "not_contains" | "equals", "pattern": "..." }`
- `ai_agent`: `{ "model": "", "system": "...", "prompt": "... {{prev}}" }`
- `notify_channel`: `{ "channel_id": "<id>", "target": "", "message": "... ({{date}} {{time}})\n{{prev}}" }`
- `save_file`: `{ "path": "reports/out-{{date}}.md", "append": false }`

## 3. Variáveis de Template Disponíveis
- `{{prev}}`: saída do nó anterior ligado pela aresta ativa.
- `{{node.<id>}}`: saída de qualquer nó anterior pelo `id`.
- `{{date}}` (`YYYY-MM-DD`) e `{{time}}` (`HH:MM:SS`).
