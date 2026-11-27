---
name: cisco-iosxe-asr-master
description: "Use em roteadores Cisco ASR 1001, ASR 1001-X, ASR 1001-HX, ASR 1002-X, ASR 1006, Catalyst 9K e IOS-XE: arquitetura RP vs QFP/ESP, drops do QFP, armadilha da licença de throughput (2.5G/5G/10G), CoPP, BNG PPPoE, CGNAT/PBA, BGP full-table, archive e rollback com configure terminal revert timer."
---

# Cisco IOS-XE / ASR 1000 Master

Nos Cisco ASR 1000 (1001, 1001-X, 1001-HX, 1002-X, 1006) o plano de controle (**RP / IOSd**) e o plano de dados (**ESP / QFP — QuantumFlow Processor**) são separados. CPU da RP baixa não garante datapath saudável.

## 1. Fluxo de auditoria com Zerith
1. Gere o lote read-only ideal para o modelo:
   `net_audit(action="cisco_asr_audit", model="asr1001x", mode="commands")`
2. Colete via `net_connect` + `terminal_batch` (ou SNMP `net_snmp(preset="cisco_asr_health")`).
   - Se o ASR tiver IOS-XE antigo sem KEX/ciphers modernos, habilite `legacy_ssh: true` no inventário (`net_inventory`).
3. Analise a saída bruta de forma determinística:
   `net_audit(action="cisco_asr_audit", model="asr1001x", mode="parse", output=...)`

## 2. RP vs QFP/ESP e armadilha da licença de throughput
- **Licença de Throughput (ASR 1001-X / 1001-HX):** mesmo com portas 10G físicas, o ASR pode sair limitado em software a `2.5G`, `5G` ou `10G`. Confira sempre:
  `show platform hardware throughput level` e `show license summary`.
- **Utilização do QFP:** `show platform hardware qfp active datapath utilization`.
  - Processing Load `>= 75%` = atenção; `>= 90%` = risco iminente de perda de pacotes no ESP.
- **QFP Drops:** `show platform hardware qfp active statistics drop`.
  - Separe drops benignos (`ArpNotForUs`, `Ipv6NdNotForUs`, `L2Mc*`, `StpNotForwarding`) dos **críticos**: `TailDrop` (fila/QoS), `NatNoPort` (exaustão CGNAT/PBA), `Ipv4NoAdj`, `Ipv4NoRoute`, `Policer`, `PuntLimit`, `IpSec`.

## 3. CoPP, BNG PPPoE e CGNAT/PBA
- **CoPP (`show policy-map control-plane`):** confira `exceeded`/`violated` nas classes de BGP, OSPF, SSH, RADIUS e SNMP. Queda de BGP sob DDoS ou flood quase sempre passa por CoPP ou `PuntLimit` no QFP.
- **BNG PPPoE (`show pppoe summary`, `show subscriber session summary`):** acompanhe sessões PTA vs TRANS e memória da RP.
- **CGNAT / PBA (`show ip nat statistics`):** monitore `Hits` vs `Misses`, `Total active translations` e `NatNoPort` no QFP. Com Port Block Allocation (PBA), pool saturado derruba novas conexões mesmo com CPU baixa.

## 4. BGP Full-Table, CEF e Óptica DOM
- `show ip bgp summary`, `show bgp all summary`: verifique `InQ`/`OutQ` > 0 e sessões fora de `Established` (`Idle`, `Active`, `Connect`).
- Memória / CEF: `show memory platform` e `show ip cef summary` antes de ativar múltiplas full-tables.
- Óptica: `show interfaces transceiver detail` (alerta abaixo de `-20 dBm` ou acima de `+1 dBm`).

## 5. Árvore de decisão de troubleshooting
- **Perda de pacotes / lentidão em horário de pico:** `QFP utilization >= 75%` ou `TailDrop`/`NatNoPort` > 0 → verifique throughput license, QoS e pool CGNAT.
- **BGP/OSPF oscilando com ping normal:** verifique `CoPP violated/dropped`, `PuntLimit` no QFP e `InQ`/`OutQ`.
- **Assinante PPPoE não conecta:** `show pppoe summary` + `show aaa servers` + `NatNoPort` se for falta de IP/porta pós-autenticação.

## 6. Mudança segura e rollback automático (`archive` + `revert timer`)
- Antes de mudar: `net_backup` e confirme `show archive`.
- Gere a receita exata: `net_audit(action="cisco_asr_audit", model="asr1001x", mode="plan_change", revert_minutes=5)`.
- Sequência recomendada:
  1. `configure terminal revert timer 5`
  2. Aplique a alteração e saia (`end`).
  3. Valide acesso, BGP e `show platform hardware qfp active statistics drop`.
  4. Confirme com `configure confirm` e salve com `write memory`.
  5. Rode `net_backup` + `net_diff` para registrar a mudança.
- Se algo falhar antes do timer: `configure revert now`.
