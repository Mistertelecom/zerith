---
name: billion-context
description: "Use quando o usuário falar de conversa longa, contexto cheio, resumo automático, billion-context ou bili no Zerith: como ligar, instalar, conferir se está ativo e o que muda."
---

# billion-context no Zerith

billion-context é um proxy local de compressão de contexto, de ranxianglei
(<https://github.com/ranxianglei/billion-context>, licença MIT com termo de
atribuição). O Zerith já vem integrado a ele; este plugin só ensina a usar.

## Como funciona

- Ligado por padrão em **Ajustes › Geral › Conversas longas (billion-context)**
  (`[context] billion_context = true` no `settings.toml`).
- Com o programa instalado, a janela sobe o proxy sozinha e o chat passa por ele;
  o CLI pega carona no proxy da janela quando ela está aberta.
- Com o proxy ativo o Zerith não resume a conversa por conta própria, só perto de
  95% da janela do modelo, para o provedor nunca recusar.
- Sem o programa, ou se ele cair, o chat vai direto e o Zerith resume sozinho.
  Nada trava.
- A assinatura Claude e o protocolo Gemini vão sempre direto.

## Instalar

Botão **Instalar** na mesma linha dos Ajustes, ou no terminal:

```bash
npm install -g billion-context
```

Precisa do Node.js 22 ou mais novo.

## Conferir

- A linha dos Ajustes mostra Ativo / Iniciando / Falhou / Não instalado.
- O log fica em `<pasta de dados do Zerith>/bili/bili.log`; cada pedido do chat
  aparece como uma linha `forward POST`.
