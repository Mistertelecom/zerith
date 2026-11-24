# Segredos num servidor (systemd)

No desktop as chaves ficam no cofre do sistema (Credential Manager, Keychain).
No Linux o cofre é o chaveiro do kernel (keyutils): some no reboot e pode nem
existir para um serviço sem sessão de login. Para isso há o backend de
**arquivo**, que só liga quando você pede.

## Qual backend está ativo

```bash
zerith secret backend   # "os" ou "file", e por quê
zerith doctor           # linha "cofre de credenciais" mostra o mesmo
```

Ordem de decisão (a primeira que valer):

1. `ZERITH_SECRETS_BACKEND=file` (ou `os`).
2. `settings.toml` do usuário:
   ```toml
   [secrets]
   backend = "file"
   ```
   O `.zerith/settings.toml` de um projeto não pode mudar isso.
3. `CREDENTIALS_DIRECTORY` definido (o systemd define quando a unidade tem
   `LoadCredential*=`): liga o arquivo automaticamente, **só no Linux**. No
   Windows/macOS o cofre do sistema funciona para serviços; lá o arquivo só
   com 1 ou 2.
4. Senão: cofre do sistema. Se ele falhar, erro — nunca cai no arquivo sozinho.

Valor desconhecido (`ZERITH_SECRETS_BACKEND=plain`) é erro em toda operação.

## Backend de arquivo

- `<raiz>/secrets/<nome>`, um arquivo por segredo (`<raiz>` = `ZERITH_HOME` ou
  `~/.local/share/zerith`). Pasta `0700`, arquivos `0600`, dono = usuário do
  processo. Gravação atômica (temporário + `rename`).
- `zerith secret set <nome>` lê uma linha da entrada; `--multiline` lê tudo até
  o fim (chave PEM, JSON). `zerith secret list` mostra só os nomes.

## Credenciais do systemd (recomendado)

Segredos cifrados pelo systemd (TPM2 e/ou chave do host em
`/var/lib/systemd/credential.secret`), entregues só ao serviço, em
`$CREDENTIALS_DIRECTORY/<nome>`. O Zerith lê dali **com prioridade** sobre o
arquivo e trata como somente leitura (`set`/`delete` recusam; troque na unidade).

```bash
# o nome da credencial é o nome do segredo no Zerith (ex.: openai, telegram_bot_token)
printf %s 'sk-...' | sudo systemd-creds encrypt --name=openai - /etc/zerith/openai.cred
# chave mestra opcional: cifra os arquivos que o Zerith grava (tokens OAuth renovados)
head -c 32 /dev/urandom | sudo systemd-creds encrypt --name=zerith-master-key - /etc/zerith/master.cred
```

```ini
# /etc/systemd/system/zerith.service
[Service]
User=zerith
Environment=ZERITH_HOME=/var/lib/zerith
StateDirectory=zerith
StateDirectoryMode=0700
LoadCredentialEncrypted=openai:/etc/zerith/openai.cred
LoadCredentialEncrypted=zerith-master-key:/etc/zerith/master.cred
ExecStart=/usr/local/bin/zerith daemon
```

Com `zerith-master-key` (32 bytes, ou 64 caracteres hex) cada arquivo em
`<raiz>/secrets/` é cifrado com ChaCha20-Poly1305 e o nome do segredo como dado
associado. Arquivos gravados antes da chave continuam legíveis; perder a chave
torna os cifrados ilegíveis (o erro diz isso — grave de novo).

Para gravar um segredo no arquivo fora do serviço, use o mesmo ambiente:

```bash
sudo -u zerith ZERITH_HOME=/var/lib/zerith ZERITH_SECRETS_BACKEND=file zerith secret set openai
```

(Sem `CREDENTIALS_DIRECTORY` aí a chave mestra não é vista: o valor fica sem
cifra. Prefira `LoadCredentialEncrypted` para chaves fixas.)

## Riscos

- **Sem chave mestra, o arquivo é texto puro.** A proteção é a permissão `0600`
  e o usuário dedicado; root, backup da pasta ou disco sem cifra expõem tudo.
- Com chave mestra, quem lê `$CREDENTIALS_DIRECTORY` (root, o próprio serviço)
  decifra. Protege contra cópia do disco/backup, não contra o host comprometido.
- No Windows o arquivo herda a ACL da pasta do perfil; não há `0600`.
- `ZERITH_API_KEY` continua valendo acima de tudo, mas variável de ambiente
  aparece em `/proc/<pid>/environ` e em `systemctl show`; prefira credencial.

Crédito: desvio "cofre ou arquivo" inspirado no Goose
(`crates/goose/src/config/base.rs`, `GOOSE_DISABLE_KEYRING` + `secrets.yaml`);
formato das credenciais na documentação do systemd ("System and Service
Credentials", `systemd-creds(1)`).
