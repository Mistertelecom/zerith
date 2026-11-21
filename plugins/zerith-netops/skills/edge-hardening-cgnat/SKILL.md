---
name: edge-hardening-cgnat
description: "Use para auditoria de segurança de borda ISP/Enterprise (CoPP, RPKI, filtros de bogons, SSH/SNMPv3, DDoS RTBH/FlowSpec) e arquitetura/troubleshooting de CGNAT (RFC 6598 100.64.0.0/10, PBA, Deterministic NAT, Hairpinning e logs de marco civil)."
---

# Hardening de Borda ISP & Arquitetura CGNAT

## 1. Hardening de Borda e Plano de Controle (Control-Plane)

- **Gerência Segura:**
  - Desabilitar Telnet, HTTP e FTP (`undo telnet server enable` / `no ip http server`).
  - Restringir SSHv2, SNMPv3 e NETCONF por ACL/Prefix-List apenas às sub-redes de gerência/bastion.
- **BGP de Borda (Trânsito, IX/PTT e Clientes):**
  - Filtrar **Bogons IPv4/IPv6** e prefixos mais específicos que `/24` (IPv4) ou `/48` (IPv6) na entrada de trânsito/IX.
  - Validar **RPKI (Origin Validation)** descartando prefixos `Invalid`.
  - Configurar `maximum-prefix` em todos os peers de IX e clientes downstream.
  - Suportar comunidade de **Blackhole (RTBH — RFC 7999, `65535:666`)** com `next-hop` apontando para `Null0`.

## 2. Arquitetura e Diagnóstico de CGNAT (`100.64.0.0/10`)

- **Dimensionamento PBA / Determinístico:**
  - Usar `100.64.0.0/10` (RFC 6598) na WAN dos assinantes para não colidir com a LAN RFC 1918 (`192.168.x.x` / `10.x.x.x`) do cliente.
  - Em **NAT Determinístico / PBA (Port Block Allocation)**: garantir pelo menos **1024 a 2048 portas por assinante** (razão 1:32 ou 1:64 por IPv4 público) com reserva de blocos estáticos ou log de alocação de bloco para conformidade legal (identificação por `IP Público + Porta Inicial..Final + Timestamp`).
- **Boas Práticas Operacionais de CGNAT:**
  - Habilitar **Endpoint-Independent Mapping (EIM)** e **Endpoint-Independent Filtering (EIF)** (Full-Cone NAT) para evitar problemas em jogos, VoIP e WebRTC.
  - Habilitar **Hairpinning** (comunicação entre dois clientes atrás do mesmo pool público).
  - Ajustar timeouts TCP Established (~3600–7200 s), TCP Transitory (~120–240 s) e UDP (~120–300 s) para evitar exaustão prematura de portas.
  - Garantir **Bypass de CGNAT** para tráfego interno e implantar **Dual-Stack (IPv6 `/56` ou `/64` via DHCPv6-PD)** para retirar 60–80% das sessões das caixas de CGNAT.
