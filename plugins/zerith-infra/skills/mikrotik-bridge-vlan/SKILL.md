---
name: mikrotik-bridge-vlan
description: "Use quando o assunto for VLAN em MikroTik RouterOS 7 com bridge: vlan-filtering, pvid, porta de acesso ou tronco, frame-types, bridge vlan, VLAN de gerência, bonding/LACP na bridge, cliente sem IP ou na VLAN errada atrás de um MikroTik."
---

# VLAN na bridge do RouterOS 7

Leia antes de mudar e siga a [mudanca-segura](../mudanca-segura/SKILL.md). Ligar `vlan-filtering` sem a VLAN de gerência configurada derruba o seu acesso: use Safe Mode (`Ctrl+X`).

## 1. Duas tabelas, e o defeito costuma estar na outra
A VLAN de uma porta depende de **duas** coisas:

| Onde | O que decide | Comando |
|---|---|---|
| `/interface bridge port` | VLAN de **entrada** de quadro sem tag (`pvid`) e o que a porta aceita (`frame-types`) | `/interface bridge port print detail` |
| `/interface bridge vlan` | Por onde cada VLAN **sai**, com ou sem tag (`tagged` / `untagged`) | `/interface bridge vlan print` |

Uma porta listada como `untagged` da VLAN 10 em `bridge vlan` mas com `pvid=1` em `bridge port` **recebe** na VLAN 1: o cliente tem link e não pega DHCP. A tabela de VLAN parece certa e o erro está no `pvid`.

## 2. Receitas
Porta de acesso (VLAN 10):
```
/interface bridge port set [find interface=ether2] pvid=10 frame-types=admit-only-untagged-and-priority-tagged
/interface bridge vlan add bridge=br vlan-ids=10 tagged=ether1 untagged=ether2
```
Tronco (VLANs 10 e 20, tag nas duas):
```
/interface bridge port set [find interface=ether1] frame-types=admit-only-vlan-tagged
/interface bridge vlan set [find vlan-ids=10] tagged=ether1
```
Gerência na própria bridge (VLAN 99): a bridge precisa estar como `tagged` na VLAN, e o IP fica numa interface VLAN sobre a bridge:
```
/interface bridge vlan add bridge=br vlan-ids=99 tagged=br,ether1
/interface vlan add name=mgmt99 interface=br vlan-id=99
/ip address add address=10.10.99.10/24 interface=mgmt99
```

## 3. Diagnóstico
- `/interface bridge vlan print` traz `current-tagged` e `current-untagged`: é o que vale de fato. Uma linha `D` "added by pvid" mostra a VLAN que um `pvid` criou sozinho.
- `/interface bridge host print where interface=ether2`: o MAC do cliente aparece com a VLAN em que entrou (`vid`).
- `ingress-filtering=yes` (padrão no 7.x) descarta quadro de VLAN que a porta não tem em `bridge vlan`.
- `/tool sniffer quick interface=ether1` mostra se o quadro sai com tag.

## 4. Bonding/LACP na bridge
- `/interface bonding add name=bond1 slaves=ether1,ether2 mode=802.3ad lacp-rate=1sec transmit-hash-policy=layer-2-and-3`.
- A bridge e a tabela de VLAN usam `bond1`, nunca `ether1`/`ether2` soltos.
- O lado Cisco/Nexus precisa ter LACP ativo (`channel-group N mode active`) e a mesma lista de VLANs.

## 5. Armadilhas
- `protocol-mode=rstp` com `edge=auto`: porta de host pode levar alguns segundos para encaminhar.
- `vlan-filtering=no` ignora `bridge vlan` e `pvid`, e tudo vira uma VLAN só.
- O VLAN nativa do Cisco (1) casa com o `pvid=1` do tronco. Mudar só um lado joga o tráfego sem tag na VLAN errada.
