---
name: netflow-syslog-telemetry
description: Análise de tráfego NetFlow v9, IPFIX, sFlow (Akvorado, Elastiflow, nfdump), detecção de ataques DDoS volumétricos, mitigação BGP FlowSpec/RTBH e consultas PromQL/LogQL.
category: devops
---

# Telemetria de Rede (NetFlow / sFlow / IPFIX), Syslog & Mitigação DDoS

Use esta skill para analisar picos de tráfego, identificar ataques DDoS (UDP Flood, DNS/NTP Amplification, SYN Flood), investigar saturação de trânsito IP/IX (PTT) e auditar logs centralizados.

## 1. Diagnóstico de Anomalias de Tráfego e DDoS

- **Identificar Top Talkers / ASNs / Prefixos de destino sob ataque:**
  - Verifique a proporção **pps (pacotes por segundo) vs bps (bits por segundo)**:
    - Alto `pps` com pacotes pequenos (`64–128 bytes`) indica `SYN Flood` ou `TCP ACK/RST Flood`.
    - Alto `bps` nas portas UDP `53`, `123`, `1900`, `11211`, `389` indica ataque de amplificação.
- **Mitigação Rápida na Borda (`Huawei VRP`, `Cisco IOS-XR/XE`, `Juniper Junos`, `MikroTik`):**
  - **RTBH (Remote Triggered Black Hole):** anunciar o `/32` atacado com a community de blackhole da operadora de trânsito (somente para proteger a capacidade do link principal quando a volumetria exceder a banda contratada).
  - **BGP FlowSpec / Traffic Policy cirúrgica:** descartar ou limitar (`car` / `policer`) apenas a porta/protocolo UDP abusado preservando o restante dos serviços do IP `/32`.
