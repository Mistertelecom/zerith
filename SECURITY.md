# Segurança

## Reportar uma vulnerabilidade

Não abra issue pública. Use "Report a vulnerability" (GitHub Security
Advisories) no repositório. Inclua versão (`zerith version`), SO e passos
para reproduzir.

## Modelo de ameaça, honesto

- **Sem sandbox real.** Presets (`read-only`, `workspace-write`,
  `danger-full-access`) são checagens no host antes da operação;
  `zerith policy` reporta enforcement `partial`. Não são jail.
- **`shell` é recusada** em qualquer preset.
- **Leitura de arquivos:** `read_file`/`list_dir` ainda **não** passam por
  `Policy::check` (só escrita e shell passam). Em correção.
- **`Ask` vira recusa:** não há fluxo de aprovação; uma operação que
  pediria confirmação é negada. Em correção.
- **Plugins, MCP e hooks** rodam com os privilégios do usuário. Instalar
  plugin não executa nem confia; hooks são comandos que o próprio usuário
  escreveu em `settings.toml`.
- **Skills** nunca têm scripts executados na instalação.

## Segredos

- Chaves de API ficam no cofre do SO (Windows Credential Manager, macOS
  Keychain, Linux keyutils), serviço `zerith`: `zerith secret set <nome>`.
- `settings.toml` guarda só `api_key_ref` (o nome da entrada), nunca a chave.
- Sem cofre disponível o Zerith **falha com erro** — não grava em texto puro.
- `ZERITH_API_KEY` continua aceito (compatibilidade) e tem precedência.
- Erros passam por `redact` antes de ir para stderr; `config show` e
  `secret get` mascaram valores.
- Linux: o backend `linux-native` usa o keyring do kernel, que **não
  persiste após reiniciar**. Persistência via Secret Service está planejada.
