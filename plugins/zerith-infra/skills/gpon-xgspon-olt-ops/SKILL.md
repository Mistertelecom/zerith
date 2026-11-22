---
name: gpon-xgspon-olt-ops
description: Operação, provisionamento e troubleshooting de OLTs GPON/XGS-PON: Huawei (MA5800/MA5600T), ZTE (C300/C320/C600/C620), Fiberhome (AN5516/AN6000), Parks, VSOL, Datacom (DmOS) e Nokia. Diagnóstico óptico (dBm, LOSi, Dying Gasp, Rogue ONU).
category: devops
---

# Operação & Troubleshooting de OLTs GPON / XGS-PON (Multi-Vendor)

Use esta skill sempre que trabalhar em **OLTs** e redes de acesso óptico **FTTH (GPON / XGS-PON)**.

## 1. Huawei SmartAX (`MA5600T` / `MA5800`)

- **Descoberta e status de ONTs:**
  ```text
  display ont autofind all
  display ont info summary 0/1/0
  display ont info 0 1 0 <ont-id>
  display ont optical-info 0 1 <pon> <ont-id>
  display ont register-info 0 1 <pon> <ont-id>
  display service-port port 0/1/<pon> ont <ont-id>
  display mac-address port 0/1/<pon> ont <ont-id>
  ```
- **Interpretação de causa de queda (`Last down cause`):**
  - `dying-gasp`: queda de energia elétrica na ONU do assinante.
  - `LOSi` / `LOBi`: rompimento de drop/fibra, conector sujo ou atenuação excessiva (`< -28 dBm`).
  - `LOFi` / `SF` / `SDi`: degradação óptica severa, reflexão ou **ONU Rogue** interferindo no time-slot TDMA da porta PON.

## 2. ZTE ZXA10 (`C300` / `C320` / `C600` / `C620`)

- **Comandos de diagnóstico em C300/C320:**
  ```text
  show gpon onu uncfg
  show gpon onu state gpon-olt_1/<slot>/<pon>
  show gpon onu detail-info gpon-onu_1/<slot>/<pon>:<onu-id>
  show pon power attenuation gpon-onu_1/<slot>/<pon>:<onu-id>
  show mac gpon onu gpon-onu_1/<slot>/<pon>:<onu-id>
  ```
- **Comandos de diagnóstico em C600/C620 (Titan):**
  ```text
  show pon onu uncfg
  show gpon onu state gpon_olt-1/<slot>/<pon>
  show pon power attenuation gpon_onu-1/<slot>/<pon>:<onu-id>
  ```

## 3. Fiberhome (`AN5516` / `AN6000`)

- **AN5516** (CLI própria, em modos: `cd onu`, `cd gpononu`, `cd service`):
  ```text
  cd onu
  show discovery slot <slot> link <pon>
  show authorization slot <slot> link <pon>
  cd ..
  cd gpononu
  show optic_module slot <slot> link <pon> onu <onu-id>
  ```
- **AN6000** usa CLI parecida com a da Cisco (`show`, `configure`, `interface`). Os nomes exatos mudam por versão: descubra com `?`.
- Muitas operações do AN5516 são feitas pelo ANM2000/UNM2000. Se o comando não existir na CLI, diga isso em vez de inventar.

## 4. Parks e VSOL

As duas usam CLI no estilo Cisco (`enable`, `configure terminal`, `interface gpon <slot>/<pon>`), mas a sintaxe varia bastante entre modelos e firmwares. **Não chute comandos.** Descubra assim:
1. Rode `show version` para anotar modelo e firmware no inventário (`net_inventory`, campo `tags`, por exemplo `fw:<versão>`).
2. Use `show ?`, `show gpon ?` ou `show onu ?` e `interface gpon <slot>/<pon>` seguido de `show ?` para listar o que existe.
3. Procure, nesta ordem: ONUs não provisionadas (auto-find ou discovery), estado e lista das ONUs da PON, potência óptica Rx/Tx da ONU e MAC aprendido no service-port ou na VLAN.
4. Depois de descobrir a sintaxe certa de um modelo, use-a na conversa inteira e cite-a no relatório para o usuário reaproveitar.

Na VSOL, a potência óptica costuma aparecer em `dBm` e a distância em metros na mesma saída de informação da ONU. Na Parks, os status das ONUs seguem os estados GPON padrão (O1 a O7, em que O5 é operacional).

## 5. Datacom (`DmOS`) e faixas ópticas

- **Datacom DmOS:**
  - `show interface gpon <chassis>/<slot>/<port> onu`
  - `show interface gpon <chassis>/<slot>/<port> onu <id> optical-transceiver-diagnosis`
- **Faixas de Nível Óptico Saudável (GPON Classe B+ / C+):**
  - **Ideal:** `-15 dBm` a `-24 dBm`
  - **Alerta (Atenuação alta):** `-25 dBm` a `-27 dBm`
  - **Crítico (Queda/CRC/LOSi):** abaixo de `-27.5 dBm` ou acima de `-8 dBm` (saturação do fotodiodo).
