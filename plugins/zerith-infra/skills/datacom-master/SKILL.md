---
name: datacom-master
description: "Use em switches e roteadores Datacom: separação estrita entre DmOS (DM4050/4170/4360/4370/4610) e DmSwitch (DM2104G2/3000/4000/4100), anéis EAPS/ERPS, transporte QinQ OLT-BNG, armadilha do PVID no DmSwitch, armadilha do `?`, DOM óptico e rollback seguro (commit confirmed vs copy running-config)."
---

# Datacom Master (DmOS & DmSwitch)

O erro operacional mais comum em Datacom é misturar sintaxe de **DmOS** (sistema moderno transacional) com **DmSwitch** (CLI legado direto na running-config). Identifique a plataforma antes do primeiro comando.

## 1. Separação estrita: DmOS vs DmSwitch
- **DmOS (`DM4050`, `DM4170`, `DM4360`, `DM4370`, `DM4610`):**
  - Paginação: `paginate false`
  - VLAN / L2: `dot1q vlan <id>`, `switchport`, `service-instance` / `l2vpn`
  - Mudança em duas fases: `commit check` → `commit confirmed 120` → validar → `commit`
  - Rollback: `rollback <id>`
- **DmSwitch (`DM2104G2`, `DM3000`, `DM4000`, `DM4100`):**
  - Paginação: `paginate false`
  - VLAN: `vlan <id>` + `set-member untagged ...` / `set-member tagged ...`
  - **Sem `commit confirmed`:** comandos aplicam imediatamente; persistência com `copy running-config startup-config`.
  - **Armadilha do `?`:** NUNCA envie `?` em `terminal_batch` no DmSwitch (causa `% 25: Invalid input` ou trava o parser de prompt).

## 2. Fluxo de auditoria com Zerith
1. Gere os comandos corretos para a plataforma:
   `net_audit(action="datacom_audit", model="dmos", mode="commands")` (ou `model="dmswitch"`).
2. Conecte via `net_connect` (use `legacy_ssh: true` se for DmSwitch antigo) e rode `terminal_batch`.
3. Audite automaticamente anéis, PVID, erros e DOM:
   `net_audit(action="datacom_audit", mode="parse", output=...)`.

## 3. Armadilha clássica do PVID no DmSwitch (+ prova na tabela MAC)
No DmSwitch, colocar uma porta como `set-member untagged` dentro de `vlan <id>` **não muda o PVID de entrada da porta**!
- Sem ajustar o PVID, pacotes que saem vão na VLAN correta, mas pacotes que **entram** da ONU/cliente continuam caindo na **VLAN 1 (nativa)** — tráfego morre silenciosamente.
- **Regra obrigatória:** sempre configure os dois juntos:
  1. Dentro de `vlan <id>`: `set-member untagged ethernet <porta>`
  2. Dentro de `interface ethernet <porta>`: `switchport native vlan <id>`
- **Prova definitiva:** rode `show mac-address-table` e confirme que o MAC do cliente/CPE aparece na VLAN `<id>` (e não na VLAN 1).

## 4. Anéis EAPS / ERPS e Transporte QinQ (OLT ↔ BNG)
- **EAPS / ERPS (`show eaps`, `show erps`):**
  - Em operação normal, o nó `Master` / `RPL Owner` mantém a porta secundária em estado `Blocked` e o anel em `Complete`/`Idle`.
  - Estado `Failed` ou `Protection` indica enlace rompido ou transceiver degradado; antes de destravar porta bloqueada, confirme que não há loop físico.
- **Transporte QinQ / Dot1q-tunnel:**
  - Valide MTU >= 1504 (ideal `>= 1522` ou jumbo `9000`) ponta a ponta para não descartar quadros com dupla tag (`S-VLAN` + `C-VLAN`) + PPPoE.
  - Confira DOM (`show interface transceiver`) e contadores CRC/input-errors antes de culpar o BNG.

## 5. Mudança segura, backup e diff
- Sempre execute `net_backup` antes e depois, e compare com `net_diff`.
- Use `net_audit(action="datacom_audit", model="dmos"|"dmswitch", mode="plan_change")` para gerar a receita exata de transação e rollback.
