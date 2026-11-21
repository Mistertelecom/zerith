---
name: bgp-ospf-mpls-tshoot
description: "Use quando o usuário pedir diagnóstico ou troubleshooting de BGP, OSPF, IS-IS, MPLS, LDP, L2VPN/VPLS, EVPN, BFD ou BNG/PPPoE em roteadores Cisco IOS-XE/XR, Huawei VRP, Juniper Junos ou MikroTik RouterOS."
---

# Diagnóstico Estruturado BGP / OSPF / MPLS / BNG Multi-Vendor

Siga sempre uma ordem **somente-leitura primeiro** (Coleta → Correlação → Causa-Raiz → Plano de Correção com Rollback).

## 1. Matriz Rápida de Comandos por Fabricante

| Verificação | Cisco IOS-XE / IOS-XR | Huawei VRP / VRPv8 | Juniper Junos | MikroTik RouterOS v7 |
| :--- | :--- | :--- | :--- | :--- |
| **Resumo BGP** | `show bgp ipv4 unicast summary` | `display bgp peer` | `show bgp summary` | `/routing/bgp/session/print` |
| **Rotas Recebidas** | `show bgp ipv4 unicast neighbors <IP> received-routes` | `display bgp routing-table peer <IP> received-routes` | `show route receive-protocol bgp <IP>` | `/routing/route/print where belongs-to~"bgp"` |
| **Rotas Anunciadas** | `show bgp ipv4 unicast neighbors <IP> advertised-routes` | `display bgp routing-table peer <IP> advertised-routes` | `show route advertising-protocol bgp <IP>` | `/routing/bgp/advertisements/print` |
| **Vizinhos OSPF** | `show ip ospf neighbor` | `display ospf peer brief` | `show ospf neighbor` | `/routing/ospf/neighbor/print` |
| **Sessões BFD** | `show bfd neighbors details` | `display bfd session all` | `show bfd session` | `/routing/bfd/session/print` |
| **MPLS / LDP** | `show mpls ldp neighbor` | `display mpls ldp peer` | `show ldp neighbor` | `/mpls/ldp/neighbor/print` |
| **Óptica / DDM (SFP)** | `show interfaces <IF> transceiver detail` | `display transceiver interface <IF> verbose` | `show interfaces diagnostics optics <IF>` | `/interface/ethernet/monitor <IF> once` |

## 2. Checklist de Causa-Raiz

1. **Camada Física e Enlace (L1/L2):**
   - Checar potência óptica (`Rx Power` em dBm), erros de CRC/FCS, `input errors`, `drops` e `MTU` (especialmente com MPLS/QinQ/VLAN tags onde 1500 bytes trunca pacotes LDP/OSPF `ExStart`/`Exchange`).
2. **IGP e Loopbacks (OSPF / IS-IS):**
   - Confirmar alcançabilidade `/32` ou `/128` da loopback do peer antes de investigar MPLS/iBGP.
   - Se OSPF travar em `ExStart`/`Exchange`, testar `ping` com bit DF (`do-not-fragment`) no tamanho do MTU da interface.
3. **BGP (eBGP / iBGP / Route Reflector):**
   - Estados `Active`/`Connect`: validar TCP/179, ACLs de CoPP, `ebgp-multihop`, `update-source` e senha MD5/TCP-AO.
   - Prefixo ausente na FIB: conferir `next-hop-self`, `route-policy` / `route-map`, filtro RPKI (`roa-validation`) e limite `maximum-prefix`.
4. **MPLS / L2VPN / EVPN:**
   - Validar label binding local e remoto (`display mpls lsp` / `show mpls forwarding-table`) e status do VC/PW (`Up/Up`, MTU de serviço idêntico nas duas pontas).
