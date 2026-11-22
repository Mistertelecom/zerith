---
name: zabbix-librenms-noc
description: Integração e troubleshooting de NOC com Zabbix API (JSON-RPC), LibreNMS, templates SNMPv2c/v3 (IF-MIB 64-bit, sensores ópticos dBm GBIC/SFP/PON, BGP4-MIB) e correlação de alarmes com acesso direto via SSH.
category: devops
---

# NOC & Monitoramento com Zabbix, LibreNMS & SNMP

Use esta skill sempre que o usuário pedir para verificar alarmes no **Zabbix** ou **LibreNMS**, criar/ajustar templates SNMP de roteadores/OLTs/Proxmox ou correlacionar alertas do NOC com diagnóstico via `net_connect`.

## 1. Consultas Diretas à API JSON-RPC do Zabbix

Quando o servidor MCP do Zabbix estiver ativo ou via `curl`/script em `ZABBIX_URL/api_jsonrpc.php` (autenticado com header `Authorization: Bearer <ZABBIX_TOKEN>`):

- **Listar incidentes/problemas ativos por severidade (`problem.get`):**
  ```json
  {
    "jsonrpc": "2.0",
    "method": "problem.get",
    "params": {
      "output": ["eventid", "name", "severity", "clock"],
      "recent": true,
      "severities": [3, 4, 5],
      "sortfield": ["eventid"],
      "sortorder": "DESC",
      "limit": 50
    },
    "id": 1
  }
  ```
- **Fluxo de Correlação NOC → Equipamento:**
  1. Identifique o host com alarme no Zabbix (`host.get` / `problem.get`).
  2. Conecte imediatamente no equipamento do cliente via `net_connect(device="...", client="...")`.
  3. Valide o estado real na CLI (sinal óptico, flap de interface, vizinhança BGP/OSPF ou carga de CPU/memória).

## 2. Boas Práticas de SNMP em Roteadores, Switches e OLTs

- **Sempre use contadores de 64 bits (`IF-MIB::ifHCInOctets` `.1.3.6.1.2.1.31.1.1.1.6` e `ifHCOutOctets` `.1.3.6.1.2.1.31.1.1.1.10`)**: contadores de 32 bits (`ifInOctets`) estouram em menos de 34 segundos em interfaces de `10 Gbps`+.
- **Monitoramento Óptico DOM (Transceivers SFP/SFP+/QSFP):**
  - Alerta preventivo quando Rx Power cair abaixo de `-22 dBm` (ou variar mais de `3 dB` em relação à linha de base) antes que o link caia por `LOS`.
