//! Backend de arquivo para servidores (opt-in): `<raiz>/secrets/<nome>`, um
//! arquivo por segredo, pasta 0700 e arquivos 0600 no Unix, gravação atômica.
//!
//! Existe porque o cofre do Linux (keyutils do kernel) some no reboot e pode
//! nem existir para um serviço systemd sem sessão. Nunca é escolhido sem o
//! usuário pedir (ver `secrets::choose`).
//!
//! Credenciais do systemd (`LoadCredential=`/`LoadCredentialEncrypted=`) em
//! `$CREDENTIALS_DIRECTORY/<nome>` têm prioridade e são somente leitura. Com
//! `$CREDENTIALS_DIRECTORY/zerith-master-key` (32 bytes, ou 64 hex) os arquivos
//! gravados aqui são cifrados com ChaCha20-Poly1305 (`ring::aead`), o nome do
//! segredo como dado associado (um arquivo renomeado não abre como outro).
//!
//! Crédito: Goose (`crates/goose/src/config/base.rs`, Apache-2.0) faz o mesmo
//! desvio — cofre do SO, ou `secrets.yaml` quando `GOOSE_DISABLE_KEYRING` está
//! ligado; aqui o arquivo é por segredo para dois processos não perderem
//! gravações um do outro. Formato das credenciais: documentação pública do
//! systemd ("System and Service Credentials").

use std::path::{Path, PathBuf};

use super::{SecretError, SecretStore};

/// Arquivo da chave mestra dentro de `$CREDENTIALS_DIRECTORY`.
pub const MASTER_KEY: &str = "zerith-master-key";
const MAGIC: &[u8] = b"ZSEC1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStore {
    pub dir: PathBuf,
    /// `$CREDENTIALS_DIRECTORY`, quando o systemd forneceu.
    pub creds: Option<PathBuf>,
}

fn io(what: &str, path: &Path, e: std::io::Error) -> SecretError {
    SecretError::Unavailable(format!("{what} {}: {e}", path.display()))
}

/// Nome do segredo como nome de arquivo: fora de `[A-Za-z0-9._-]` (e um `.`
/// inicial, que esconderia o arquivo) vira `%XX`.
fn file_name(name: &str) -> Result<String, SecretError> {
    if name.is_empty() {
        return Err(SecretError::Unavailable("nome de segredo vazio".into()));
    }
    let mut out = String::new();
    for (i, b) in name.bytes().enumerate() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || (b == b'.' && i > 0) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}

fn decode_name(file: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut it = file.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).ok()
}

impl FileStore {
    pub fn new(dir: PathBuf, creds: Option<PathBuf>) -> Self {
        Self { dir, creds }
    }

    fn cred(&self, name: &str) -> Result<Option<String>, SecretError> {
        let Some(dir) = &self.creds else {
            return Ok(None);
        };
        let path = dir.join(file_name(name)?);
        match std::fs::read_to_string(&path) {
            // `echo x | systemd-creds encrypt` deixa a quebra de linha.
            Ok(v) => Ok(Some(v.trim_end_matches(['\r', '\n']).to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io("não consegui ler a credencial", &path, e)),
        }
    }

    fn key(&self) -> Result<Option<ring::aead::LessSafeKey>, SecretError> {
        let Some(dir) = &self.creds else {
            return Ok(None);
        };
        let path = dir.join(MASTER_KEY);
        let raw = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io("não consegui ler a chave", &path, e)),
        };
        let bytes = if raw.len() == 32 {
            raw
        } else {
            let hex = String::from_utf8_lossy(&raw).trim().to_string();
            (0..hex.len())
                .step_by(2)
                .map(|i| {
                    hex.get(i..i + 2)
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                })
                .collect::<Option<Vec<u8>>>()
                .filter(|b| b.len() == 32)
                .ok_or_else(|| {
                    SecretError::Unavailable(format!(
                        "{}: a chave mestra precisa de 32 bytes (ou 64 caracteres hex)",
                        path.display()
                    ))
                })?
        };
        let key = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, &bytes)
            .map_err(|_| SecretError::Unavailable("chave mestra inválida".into()))?;
        Ok(Some(ring::aead::LessSafeKey::new(key)))
    }

    fn seal(&self, name: &str, value: &str) -> Result<Vec<u8>, SecretError> {
        use ring::aead::{Aad, Nonce, NONCE_LEN};
        use ring::rand::SecureRandom;
        let Some(key) = self.key()? else {
            return Ok(value.as_bytes().to_vec());
        };
        let mut nonce = [0u8; NONCE_LEN];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| SecretError::Unavailable("sem fonte aleatória".into()))?;
        let mut buf = value.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(name.as_bytes()),
            &mut buf,
        )
        .map_err(|_| SecretError::Unavailable("falha ao cifrar".into()))?;
        Ok([MAGIC, &nonce, &buf].concat())
    }

    fn open(&self, name: &str, data: Vec<u8>) -> Result<String, SecretError> {
        use ring::aead::{Aad, Nonce, NONCE_LEN};
        let Some(rest) = data.strip_prefix(MAGIC) else {
            // Gravado antes de existir a chave: lido como está.
            return String::from_utf8(data)
                .map_err(|_| SecretError::Unavailable(format!("segredo '{name}' não é UTF-8")));
        };
        let key = self.key()?.ok_or_else(|| {
            SecretError::Unavailable(format!(
                "segredo '{name}' está cifrado e falta $CREDENTIALS_DIRECTORY/{MASTER_KEY}"
            ))
        })?;
        if rest.len() < NONCE_LEN {
            return Err(SecretError::Unavailable(format!(
                "segredo '{name}' corrompido"
            )));
        }
        let (nonce, sealed) = rest.split_at(NONCE_LEN);
        let mut buf = sealed.to_vec();
        let nonce = Nonce::try_assume_unique_for_key(nonce)
            .map_err(|_| SecretError::Unavailable("nonce inválido".into()))?;
        let plain = key
            .open_in_place(nonce, Aad::from(name.as_bytes()), &mut buf)
            .map_err(|_| {
                SecretError::Unavailable(format!(
                    "segredo '{name}' não abriu com a chave mestra (chave trocada ou arquivo alterado)"
                ))
            })?;
        String::from_utf8(plain.to_vec())
            .map_err(|_| SecretError::Unavailable(format!("segredo '{name}' não é UTF-8")))
    }

    fn ensure_dir(&self) -> Result<(), SecretError> {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(&self.dir)
            .map_err(|e| io("não consegui criar", &self.dir, e))?;
        // Já existia com outra permissão: aperta, a pasta é nossa.
        #[cfg(unix)]
        std::fs::set_permissions(
            &self.dir,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .map_err(|e| io("não consegui proteger", &self.dir, e))?;
        // ponytail: no Windows (só com pedido explícito) o arquivo herda a ACL
        // da pasta do perfil (%APPDATA%, só do usuário); ACL própria se um dia
        // alguém rodar o serviço lá.
        Ok(())
    }

    fn read_only(&self, name: &str) -> Result<(), SecretError> {
        match self.cred(name)? {
            Some(_) => Err(SecretError::Unavailable(format!(
                "'{name}' vem do systemd ($CREDENTIALS_DIRECTORY), somente leitura; \
                 troque em LoadCredential*="
            ))),
            None => Ok(()),
        }
    }

    /// Nomes (nunca valores) das credenciais do systemd e dos arquivos.
    pub fn names(&self) -> Result<Vec<(String, &'static str)>, SecretError> {
        let mut out = Vec::new();
        let mut scan = |dir: &Path, src: &'static str| -> Result<(), SecretError> {
            let rd = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(io("não consegui listar", dir, e)),
            };
            for entry in rd.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file.starts_with('.') || file == MASTER_KEY {
                    continue;
                }
                if let Some(n) = decode_name(&file) {
                    if !out.iter().any(|(o, _)| o == &n) {
                        out.push((n, src));
                    }
                }
            }
            Ok(())
        };
        if let Some(c) = &self.creds {
            scan(c, "systemd")?;
        }
        scan(&self.dir, "arquivo")?;
        out.sort();
        Ok(out)
    }
}

impl SecretStore for FileStore {
    fn get(&self, name: &str) -> Result<String, SecretError> {
        if let Some(v) = self.cred(name)? {
            return Ok(v);
        }
        let path = self.dir.join(file_name(name)?);
        match std::fs::read(&path) {
            Ok(data) => self.open(name, data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(SecretError::NotFound(name.to_string()))
            }
            Err(e) => Err(io("não consegui ler", &path, e)),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.read_only(name)?;
        self.ensure_dir()?;
        let file = file_name(name)?;
        let path = self.dir.join(&file);
        let tmp = self.dir.join(format!(".{file}.{}.tmp", std::process::id()));
        let data = self.seal(name, value)?;
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            let mut f = o.open(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
            // Temporário e destino na mesma pasta: o rename é atômico.
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io("não consegui gravar", &path, e)
        })
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.read_only(name)?;
        let path = self.dir.join(file_name(name)?);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(SecretError::NotFound(name.to_string()))
            }
            Err(e) => Err(io("não consegui remover", &path, e)),
        }
    }

    fn list(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.names()?.into_iter().map(|(n, _)| n).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(creds: bool) -> (tempfile::TempDir, FileStore) {
        let t = tempfile::tempdir().unwrap();
        let c = creds.then(|| {
            let c = t.path().join("creds");
            std::fs::create_dir_all(&c).unwrap();
            c
        });
        let s = FileStore::new(t.path().join("secrets"), c);
        (t, s)
    }

    #[test]
    fn grava_le_remove_e_lista_sem_valores() {
        let (_t, s) = store(false);
        assert!(matches!(s.get("openai"), Err(SecretError::NotFound(_))));
        s.set("openai", "sk-1").unwrap();
        s.set("mcp.x/y.oauth", "linha1\nlinha2").unwrap();
        s.set("openai", "sk-2").unwrap();
        assert_eq!(s.get("openai").unwrap(), "sk-2");
        assert_eq!(s.get("mcp.x/y.oauth").unwrap(), "linha1\nlinha2");
        let names = s.list().unwrap();
        assert_eq!(names, vec!["mcp.x/y.oauth", "openai"]);
        assert!(!format!("{names:?}").contains("sk-"));
        // Nenhum temporário deixado para trás.
        assert_eq!(std::fs::read_dir(&s.dir).unwrap().count(), 2);
        s.delete("openai").unwrap();
        assert!(matches!(s.get("openai"), Err(SecretError::NotFound(_))));
        assert!(matches!(s.delete("openai"), Err(SecretError::NotFound(_))));
        assert_eq!(file_name(".x").unwrap(), "%2Ex");
    }

    #[cfg(unix)]
    #[test]
    fn pasta_0700_e_arquivo_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, s) = store(false);
        std::fs::create_dir_all(&s.dir).unwrap();
        std::fs::set_permissions(&s.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        s.set("k", "v").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&s.dir), 0o700);
        assert_eq!(mode(&s.dir.join("k")), 0o600);
    }

    #[test]
    fn credencial_do_systemd_tem_prioridade_e_e_somente_leitura() {
        let (_t, s) = store(true);
        s.set("openai", "do-arquivo").unwrap();
        std::fs::write(s.creds.as_ref().unwrap().join("openai"), "do-systemd\n").unwrap();
        assert_eq!(s.get("openai").unwrap(), "do-systemd");
        assert!(s.set("openai", "x").is_err());
        assert!(s.delete("openai").is_err());
        assert_eq!(s.names().unwrap(), vec![("openai".to_string(), "systemd")]);
    }

    #[test]
    fn com_chave_mestra_o_arquivo_fica_cifrado() {
        let (_t, s) = store(true);
        let creds = s.creds.clone().unwrap();
        s.set("antigo", "em-claro").unwrap();
        std::fs::write(creds.join(MASTER_KEY), "ab".repeat(32)).unwrap();
        s.set("openai", "sk-secreta").unwrap();
        let raw = std::fs::read(s.dir.join("openai")).unwrap();
        assert!(raw.starts_with(MAGIC));
        assert!(!String::from_utf8_lossy(&raw).contains("sk-secreta"));
        assert_eq!(s.get("openai").unwrap(), "sk-secreta");
        // O de antes da chave continua legível; a chave não aparece na lista.
        assert_eq!(s.get("antigo").unwrap(), "em-claro");
        assert_eq!(s.list().unwrap(), vec!["antigo", "openai"]);
        // Arquivo trocado de nome não abre como outro segredo.
        std::fs::copy(s.dir.join("openai"), s.dir.join("outro")).unwrap();
        assert!(s.get("outro").is_err());
        // Sem a chave, erro claro em vez de lixo.
        std::fs::remove_file(creds.join(MASTER_KEY)).unwrap();
        assert!(s
            .get("openai")
            .unwrap_err()
            .to_string()
            .contains(MASTER_KEY));
    }
}
