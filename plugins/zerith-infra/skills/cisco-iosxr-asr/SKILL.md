---
name: cisco-iosxr-asr
description: "Use em roteadores Cisco ASR 9000, NCS ou IOS-XR: route-policy RPL e prefix-set, BGP iBGP/RR e timers, IS-IS e LDP, L2VPN xconnect e pseudowire, BNG PPPoE com RADIUS, MgmtEth, LPTS e management-plane, commit confirmed."
---

# Cisco ASR 9000 / IOS-XR

IOS-XR trabalha com **configuração em duas fases**: nada vale até o `commit`. Use isso a seu favor.

## 1. Mudança segura no IOS-XR
- Entre com `configure` e confira o que vai mudar com `show configuration` (o pendente) antes de qualquer commit.
- Para mudança de risco, use **`commit confirmed 300`** e confirme com `commit` só depois de validar o acesso. Sem confirmação, o roteador volta sozinho.
- Rollback: `show configuration commit list`, depois `rollback configuration last 1` (ou `to <commitID>`).
- Antes de mexer: `show running-config` do trecho afetado, salvo no backup. Para um bloco: `show running-config router bgp`.
- `commit replace` substitui a configuração inteira: **nunca** use em produção sem um motivo muito claro.

## 2. Route-policy (RPL): a armadilha do drop implícito
- Toda route-policy termina com **drop implícito**. Uma rota só é aceita se passar por um `pass` (ou `done`).
- `set` (community, local-preference, MED), `prepend as-path` e `set med` **não aceitam a rota sozinhos**. Uma policy que só faz `prepend` e não tem `pass` não anuncia nada.
- Padrão correto:
  ```
  route-policy RP-OUT
    if destination in PS-NOSSOS then
      prepend as-path 64512 2
      pass
    else
      drop
    endif
  end-policy
  ```
- Verifique o efeito real antes e depois:
  - `show bgp ipv4 unicast neighbors <IP> advertised-count`
  - `show bgp ipv4 unicast policy route-policy <RP> ` (simula)
  - `show rpl route-policy <RP> attachpoints` (onde ela está aplicada)
- Mudou a policy? Aplique sem derrubar a sessão: `clear bgp ipv4 unicast <IP> soft out`.

## 3. BGP
- Resumo: `show bgp ipv4 unicast summary`; vizinho: `show bgp neighbor <IP>` (mostra hold time negociado, último erro e motivo da queda).
- **Timers**: a sessão usa o **menor hold time** dos dois lados. Configuração diferente nas pontas (`timers 10 30` x `timers 60 180`) gera hold de 30 s: com CPU alta, keepalives atrasam e a sessão cai com `hold time expired`. Padronize nas duas pontas.
- iBGP com RR: `update-source Loopback0` nas duas pontas, alcançabilidade das loopbacks via IGP antes de tudo (`show isis neighbors`, `show route <loopback>`).
- eBGP de trânsito: `maximum-prefix`, filtro de bogons na entrada, só os próprios prefixos na saída.

## 4. IS-IS e LDP
- `show isis neighbors`, `show isis database`, `show mpls ldp neighbor`, `show mpls forwarding`.
- MTU do enlace de core igual nas duas pontas; com MPLS, o MTU precisa caber os labels.

## 5. L2VPN / pseudowire
- `show l2vpn xconnect detail`, `show l2vpn xconnect group <G> xc-name <X> detail`.
- **MTU do attachment circuit deve ser igual nas duas pontas do mesmo pw-id.** Com MTU diferente o PW pode não subir, ou subir e descartar quadros grandes: ping pequeno passa e o tráfego real (VPN, backup) quebra.
- Teste com `ping <ip> size <n> donotfrag` pelo serviço do cliente para achar o limite.

## 6. BNG PPPoE e RADIUS
- `show subscriber session all summary`, `show radius`, `show radius server-groups`, `show aaa servers`.
- `source-interface` do grupo RADIUS precisa apontar para uma interface **que existe e tem IP alcançável** pelo servidor. Se a interface não existe, o roteador não envia Access-Request: assinantes novos não autenticam e os já conectados ficam.
- Confira no servidor (FreeRADIUS `radiusd -X`) se os pedidos chegam e com qual IP de origem; o NAS precisa estar cadastrado com esse IP.

## 7. Gerência (não se tranque para fora)
- Caminho de gerência dedicado: `MgmtEth0/RSP0/CPU0/0` (e o par na outra RSP).
- **ACL de SSH vai na gerência ou no management-plane, nunca num enlace de core/trânsito.** Uma ACL de "permit só 10.250.0.0/24 na porta 22" aplicada num enlace de core também filtra todo o resto que passa por lá e bloqueia o NOC que chega por outro caminho.
- Prefira o controle centralizado:
  ```
  control-plane
   management-plane
    out-of-band
     interface MgmtEth0/RSP0/CPU0/0
      allow SSH peer
       address ipv4 10.250.0.0/24
  ```
- LPTS (`lpts pifib hardware police`) limita a taxa de pacotes para a CPU. Taxa de SSH muito baixa deixa o SSH lento ou recusando sob carga; confira com `show lpts pifib hardware police location all | include SSH`.
- Antes de mudar ACL/management-plane: tenha console (ou o outro RSP) e use `commit confirmed`.
