---
name: network-consulting
description: Workflows profissionais de consultoria em redes (BGP, OSPF, MPLS, VRF, VLAN, firewall) usando inventário, SSH via cofre e lotes seguros.
---

# Consultoria de Redes — Fluxo Operacional

Use este guia ao diagnosticar, auditar ou operar roteadores, switches e firewalls com `net_inventory`, `net_connect`, `terminal_batch`, `terminal_send` e diagnósticos locais (`ping`, `traceroute`, `mtr`, `dig`).

## 1. Descoberta e Conexão Segura
1. Chame `net_inventory` (com filtro `tag`, `vendor`, `role` ou `query`) para descobrir os equipamentos cadastrados em `.zerith/inventory.toml` e entender se são `core`, `border`, `firewall`, `distribution` ou `lab`.
2. Conecte com `net_connect` (passando `device: "<nome>"`).
   - O Zerith injeta `secret_ref` pelo cofre do sistema (nunca exponha senhas em texto).
   - Paginação (`terminal length 0`, `screen-length 0 temporary`, `set cli screen-length 0`) é desativada automaticamente ao conectar.
3. Feche a sessão com `terminal_close` ao concluir o trabalho.

## 2. Coleta Diagnóstica em Lote (`terminal_batch`)
Prefira `terminal_batch` com comandos puramente de leitura por vendor (reaproveita aprovação de sessão sem interromper cada comando):

- **BGP / Borda (`cisco_ios` / `cisco_xr` / `huawei_vrp` / `juniper_junos`):**
  - Cisco: `show ip bgp summary`, `show bgp ipv4 unicast summary`, `show ip route summary`, `show logging | inc BGP`
  - Huawei: `display bgp peer`, `display ip routing-table statistics`, `display interface brief`, `display logbuffer`
  - JunOS: `show bgp summary`, `show route summary`, `show system alarms`
  - MikroTik: `/routing bgp session print`, `/ip route print count-only`, `/interface print stats`
- **OSPF / L2 / VRF:**
  - Verificar vizinhança (`show ip ospf neighbor` / `display ospf peer brief`), MTU/erros de interface (`show interfaces` / `display interface`), VRF (`show ip vrf` / `display ip vpn-instance`) e tabelas MAC/ARP (`show mac address-table` / `display mac-address`).
- **Lembrando sobre saúde e limites:** Nunca cole tabelas gigantes sem filtro quando puder consultar por prefixo/VRF/peer (`show ip bgp neighbors <ip>`). O Token Saver (RTK) comprime automaticamente dumps extensos mantendo anomalias e resumo.

## 3. Protocolo Obrigatório de Janela de Mudança (Mutações)
Antes de aplicar **qualquer alteração** em `core`, `border`, `firewall` ou produção:
1. **Pré-check:** Colete estado antes (`show bgp summary`, `show running-config interface ...`, rotas e contadores) via `terminal_batch`.
2. **Plano & Diff exato:** Apresente claramente os comandos exatos que serão aplicados e o impacto esperado.
3. **Plano de Rollback explícito:** Defina os comandos exatos de reversão antes da mudança (ex.: `commit confirmed 5` no JunOS / IOS-XR, `rollback`, ou remoção exata da configuração).
4. **Aprovação:** Comandos de mutação e comandos destrutivos (`reload`, `erase`, `reset saved-configuration`, `shutdown`) sempre pedem aprovação explícita do operador.
5. **Pós-check:** Valide sessões, logs e rotas logo após a mudança e salve configuração apenas quando validado.
