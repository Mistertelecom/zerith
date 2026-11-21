---
name: ftth-vlan-bng-integration
description: Arquitetura de VLANs FTTH (QinQ S-VLAN/C-VLAN, VLAN por PON/Slot), PPPoE Intermediate Agent, DHCP Option 82, TR-069 (GenieACS) e integração OLT <-> Switch de Agregação <-> BNG/BRAS.
category: devops
---

# Integração FTTH OLT ↔ Agregação ↔ BNG/BRAS (QinQ, PPPoE & TR-069)

Use esta skill para projetar ou corrigir falhas de comunicação entre a **OLT** e o concentrador **BNG/BRAS** (Huawei NE8000/NE40E, MikroTik CCR, Cisco ASR, Juniper MX, Accel-PPP).

## 1. Diagnóstico de Falha de Autenticação PPPoE (PADI / PADO)

Quando o assinante está com sinal óptico normal (`working` / `online`), mas o PPPoE não autentica:
1. **Verifique aprendizado de MAC na OLT:**
   - Se o MAC da WAN da ONU **não aparece** no `service-port` da OLT: erro no `ont-srvprofile`, porta da ONU em modo bridge/route incorreto ou VLAN untagged/tagged divergente no perfil OMCI.
2. **Se o MAC aparece na OLT, mas não chega PADI no BNG:**
   - Verifique se a VLAN (ou `S-VLAN` QinQ) está permitida na porta de uplink da OLT (`port vlan <vlan> 0/<uplink-slot> 0`) e no trunk do switch de agregação até a subinterface do BNG (`vlan-type dot1q` / `qinq termination pe-vid ce-vid`).
3. **Isolamento L2 e Segurança na OLT:**
   - Mantenha isolamento de portas ativo entre ONUs da mesma PON (user-bridging desligado), `anti-macspoofing` e `pppoe intermediate-agent` (Circuit-ID / Remote-ID) habilitados para rastrear a caixa/porta PON exata no RADIUS.
