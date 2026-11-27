---
name: noc-zabbix-grafana-master
description: Operação avançada de NOC com Zabbix 7.0 LTS (`net_zabbix`) e Grafana (`net_grafana`): triagem composta em 1 chamada, trends/SLA, janelas de manutenção, geração determinística de templates Zabbix YAML/JSON com histerese e dashboards NOC 4-tier (Grafana v39 + formato portátil 24 colunas).
category: devops
---

# Masterclass NOC: Zabbix 7.0 LTS & Grafana (`net_zabbix` + `net_grafana`)

Use esta skill sempre que precisar investigar incidentes em massa no Zabbix, gerar relatórios de SLA, abrir janelas de manutenção antes de mudanças, criar templates Zabbix 7.0 LTS para ISP/NOC ou inspecionar, gerar e publicar dashboards profissionais no Grafana.

---

## 1. Triagem Rápida no Zabbix com `net_zabbix` (Sem Queimar Turnos)

Priorize chamadas compostas e específicas em vez de encadear dezenas de `items`/`history` avulsos:

1. **Triagem por Host em 1 Chamada (`action: "host_summary"`):**
   - Informe `host` (ou `hostids`) para obter em um único JSON: status do host, disponibilidade de interfaces (`up`/`down` e erros SNMP/Agent), grupos, templates vinculados, problemas ativos e itens com erro/não suportados + métricas-chave (`icmpping`, CPU, BGP, DOM, PON).
2. **Visão Global / Turno NOC (`action: "sla_report"` e `"problems"`):**
   - `sla_report` agrega disponibilidade da frota (`available == 1` vs `2` / `0`), contagem de incidentes por severidade (`0..5`) e Top 10 hosts/triggers ofensores.
   - `triggers` com `only_true: true`, `min_severity: 3` e `groups` filtra rapidamente triggers ativas expandidas (`expandDescription: true`).
3. **`trends` vs `history`:**
   - **Janelas curtas (< 24h):** use `action: "history"` (auto-detecta `value_type` quando `history_type` é omitido).
   - **Janelas longas (7d / 30d / 90d):** use **`action: "trends"`** (`trend.get` retorna séries horárias com `num`, `value_min`, `value_avg`, `value_max`), suportando `time_from: "7d"` ou `"30d"` e `time_till: "now"` sem varrer tabelas massivas de histórico.
4. **Manutenção & Reconhecimento (`maintenance` e `ack`):**
   - Antes de qualquer mudança em campo ou core, crie janela com `action: "maintenance", op: "create"` (`name`, `duration_mins` ou `active_since`/`active_till`, `hostids`/`groups`, `description`).
   - Use `action: "ack"` com `eventids`, `message`, `acknowledge: true` (e `close: true` apenas quando permitido pela trigger).

---

## 2. Boas Práticas Zabbix 7.0 LTS (`generate_template`)

Use `net_zabbix(action="generate_template", preset="...", format="yaml", output_path="...")` (funciona **100% offline**, sem `token_secret`) para gerar templates `zabbix_export` v7.0 prontos para importação:

- **Presets disponíveis:**
  - `cisco_asr_1001x`: RP CPU, QFP/ESP Load, LLD IF-MIB 64-bit, LLD BGP4-MIB, LLD DOM óptico (`CISCO-ENTITY-SENSOR-MIB`).
  - `datacom_dmos`: CPU/Memória DmOS, LLD IF-MIB 64-bit, LLD DOM SFP+/QSFP28 para anel metro.
  - `ubiquiti_rf`: RSSI, Noise Floor, CCQ %, RF TX/RX Capacity e LLD Ethernet 64-bit.
  - `gpon_olt`: CPU OLT, LLD de portas PON (potência Tx e ONUs online), LLD de ONUs (sinal Rx dBm e estado operacional) e uplinks 64-bit.
- **Regras de ouro implementadas:**
  - **Contadores 64-bit:** sempre `ifHCInOctets` / `ifHCOutOctets` com pré-processamento `CHANGE_PER_SECOND` + `MULTIPLIER(8)` (`bps`).
  - **Pré-processamento anti-ruído:** `DISCARD_UNCHANGED_HEARTBEAT` em status operacionais (`ifOperStatus`, `bgpPeerState`, `icmpping`).
  - **Histerese obrigatória (`recovery_mode: RECOVERY_EXPRESSION`):** evite flapping usando banda morta na recuperação (ex.: DOM Rx dispara `< {$DOM_RX_WARN}` e só recupera `> {$DOM_RX_WARN} + 2`; saturação de interface dispara `> 80%` e recupera `< 70%`; ICMP exige 3 falhas para abrir e 3 OK consecutivos para fechar).

---

## 3. Operação & Automação do Grafana com `net_grafana`

- **Descoberta e Diagnóstico:**
  - `health`: valida `/api/health` e `/api/org`.
  - `datasources`: lista UIDs e tipos configurados (Prometheus, `alexanderzobnin-zabbix-datasource`, Loki, Influx).
  - `search` / `get_dashboard` / `folders`: localiza dashboards e pastas existentes antes de atualizar.
  - `alerts`: consulta regras provisionadas e alertas ativos no Alertmanager do Grafana.
  - `query`: executa consultas diretas em `/api/ds/query` (`queries` ou atalho `datasource_uid` + `expr`, `from`, `to`).
- **Anotações de Mudança (`annotations`):**
  - Registre toda janela de manutenção, troca de SFP/GBIC, mudança de BGP ou failover usando `action: "annotations", op: "create", text: "...", tags: ["noc", "change"]`. Isso desenha marcos visuais diretamente sobre os gráficos de tráfego e latência.
- **Publicação Idempotente (`save_dashboard` / `generate_dashboard` com `publish: true`):**
  - Sempre defina um `uid` determinístico e `overwrite: true` com `message` descritiva para manter histórico de revisões limpo no Grafana.

---

## 4. Arquitetura de Dashboards NOC em 4 Tiers (Grid 24 Colunas)

Ao gerar dashboards com `net_grafana(action="generate_dashboard", preset="...", format="both")`:

1. **Presets prontos:** `isp_backbone_cisco_asr`, `isp_olt_gpon`, `isp_datacom_ring`, `isp_ubiquiti_rf`, `linux_sre_host`, `noc_executive_overview`, além de `custom`.
2. **Hierarquia visual obrigatória em 4 Tiers (Rows colapsáveis):**
   - **Tier 1 — Saúde Crítica & KPIs Executivos:** painéis `stat` / `gauge` (`w: 4, h: 4`, 6 por linha = 24 cols) para SLA %, sessões/ONUs ativas, perda de pacotes, CPU/QFP e temperatura com thresholds Verde → Amarelo → Laranja → Vermelho.
   - **Tier 2 — Tráfego & Capacidade:** painéis `timeseries` (`w: 12, h: 8`, 2 por linha = 24 cols) com **RX positivo / TX espelhado (`negative-Y`)**, tabela de legenda (`last`, `mean`, `max`) e linha de Percentil 95 (**P95**).
   - **Tier 3 — Qualidade, Drops & Camada Física:** painéis `timeseries` (`w: 8, h: 7`, 3 por linha = 24 cols) para QFP/QoS drops, erros CRC/FCS, potência óptica DOM (dBm) ou RSSI/Noise/Chain Balance RF.
   - **Tier 4 — Estado Temporal & Diagnóstico:** `state-timeline` (`w: 12, h: 8`) para flapping/indisponibilidade + `table` (`w: 12, h: 8`) ordenável com células coloridas por threshold.
3. **Variáveis de Template:** `$datasource`, `$site` (POP/Grupo), `$host` e foco por domínio (`$interface`, `$pon_port`, `$rf_link`, `$device`).
4. **Formato Dual (`grafana_json` vs `noc_portable_json`):**
   - `grafana_json`: `schemaVersion: 39`, pronto para importar ou publicar (`publish: true`) no Grafana.
   - `noc_portable_json`: schema `zerith-noc-portable/v1` neutro de fornecedor preservando o mesmo grid exato de 24 colunas (`gridPos: {x, y, w, h}`), `rows`, `panels`, `targets`, `unit`, `thresholds` e `variables`.

---

## 5. Segurança e Credenciais

- **Nunca informe tokens em texto puro** nos argumentos das ferramentas (`token`, `password`, `api_key`, `secret` são recusados).
- Grave credenciais no cofre (`zerith secret set <nome>`) e passe apenas **`token_secret`** (e opcionalmente **`url_secret`**).
- Os geradores locais (`net_zabbix` `generate_template` e `net_grafana` `generate_dashboard` com `publish: false`) operam **offline** sem exigir `token_secret`.
