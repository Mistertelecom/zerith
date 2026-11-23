---
name: cisco-nexus-nxos
description: "Use quando o assunto for switch Cisco Nexus com NX-OS (5000/5500/5600, 7000, 9000, 9000v/9300v): VLAN, tronco, SVI, port-channel, vPC, HSRP, OSPF, relay DHCP, feature, licença, diferenças do Nexus 5K para o 9000v."
---

# Cisco Nexus (NX-OS)

Leia antes de mudar e siga a [mudanca-segura](../mudanca-segura/SKILL.md). Os comandos abaixo valem para NX-OS 5.x a 10.x, salvo nota.

## 1. Ligue a feature antes do comando
No NX-OS quase tudo é desligado de fábrica. Um comando de uma feature desligada dá `% Invalid command`, e isso não quer dizer sintaxe errada.

| Para | Feature |
|---|---|
| SVI (`interface VlanN`) | `feature interface-vlan` |
| OSPF / BGP | `feature ospf` / `feature bgp` |
| Relay DHCP (`ip dhcp relay address`) | `feature dhcp` + `ip dhcp relay` |
| HSRP / VRRP | `feature hsrp` / `feature vrrp` |
| vPC / LACP | `feature vpc` / `feature lacp` |

`show feature | include <nome>`: `enabled(not-running)` indica falta de licença (Nexus 5K/7K sem `LAN_ENTERPRISE_SERVICES_PKG`). O 9000v roda em honor mode e não bloqueia.

## 2. Tronco: `add`, nunca a lista solta
- `switchport trunk allowed vlan 10` **substitui** a lista inteira. Para acrescentar use `switchport trunk allowed vlan add 20`. Para tirar, `remove`.
- Diagnóstico: `show interface trunk` (Allowed, STP Forwarding, Err-disabled). Uma VLAN que existe e tem SVI `up` mas não aparece em "STP Forwarding" no tronco não chega ao switch vizinho.
- VLAN nativa padrão é 1; os dois lados precisam concordar (`switchport trunk native vlan N`).

## 3. SVI e relay DHCP
- `show ip interface brief vrf all`: SVI `down` sem porta ativa na VLAN (autostate). Confira `show vlan id N` e o tronco.
- Máscara do SVI tem que bater com o escopo do DHCP. Com `/25` no SVI e `/24` no servidor, clientes acima de `.127` só falam com o gateway.
- Relay: `show ip dhcp relay` e `show ip dhcp relay statistics`. O endereço do relay é o do servidor, não o do gateway da VLAN dele. Prove com `tcpdump` no servidor ou com o log do dnsmasq/ISC.
- O servidor responde ao `giaddr` (IP do SVI). Se o gateway padrão do servidor estiver errado, a oferta some no caminho de volta, mesmo com o relay certo.

## 4. OSPF no NX-OS
- A configuração fica na interface: `ip router ospf 1 area 0.0.0.0`. `router ospf 1` só leva router-id, redistribuição e áreas.
- `show ip ospf interface brief`, `show ip ospf neighbors`, `show ip ospf interface <if>`: compare o "Network type" com o vizinho. `ip ospf network point-to-point` num lado e broadcast no outro sobe vizinho e não fecha o SPF.
- `ip ospf passive-interface` nos SVIs é normal: a rede é anunciada e não há vizinho por lá.
- Rotas: `show ip route ospf-1` (as externas aparecem como `type-1`/`type-2`).

## 5. vPC (5K/9K)
- `vpc domain N`, `peer-keepalive destination <ip-mgmt0-do-par> source <ip> vrf management`. Nunca pelo peer-link.
- `show vpc`, `show vpc consistency-parameters global` e `interface port-channel N`. Lista de VLANs, MTU e modo STP iguais nos dois, senão o vPC fica suspenso.
- HSRP com vPC: os dois encaminham pelo VIP; o ativo só responde ARP.

## 6. Salvar e boot
- `copy running-config startup-config`. Sem isso tudo some no reload.
- 9000v em laboratório: se parar em `loader >` depois de reiniciar, rode `boot nxos.<versão>.bin` e depois grave `boot nxos bootflash:nxos.<versão>.bin` + `copy run start`.
- No primeiro boot, responda `skip` ao POAP (pula senha e assistente) ou `yes` para configurar.

## 7. Nexus 5K real versus 9000v
- Não existe Nexus 5000 virtual: no laboratório o 9300v substitui. VLAN, tronco, SVI, port-channel, vPC, HSRP e OSPF têm a mesma sintaxe.
- Diferenças: o 5K tem FEX (`feature fex`, `fex associate`), FCoE e licença que bloqueia de verdade (OSPF sem `LAN_ENTERPRISE` fica `not-running`). O 5548/5596 precisa de módulo L3 para SVI e OSPF: `show module` mostra a placa de camada 3.
- Nomes de porta: o 5K usa `Ethernet1/1-48` e `Ethernet10x/1/y` para FEX; o 9000v usa `Ethernet1/1-64`.
