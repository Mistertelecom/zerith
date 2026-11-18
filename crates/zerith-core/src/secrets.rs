//! API keys in the OS credential store, service name `zerith`.
//!
//! Windows Credential Manager, macOS Keychain, Linux kernel keyutils (the
//! `keyring` crate's native backends). There is no plaintext fallback: when
//! the store is unavailable the caller gets an error saying so, never a key
//! silently written to disk.

use std::fmt;

pub const SERVICE: &str = "zerith";

/// Env var that overrides every stored key (kept for compatibility).
pub const API_KEY_ENV: &str = "ZERITH_API_KEY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// The store answered: there is no such entry.
    NotFound(String),
    /// The store could not be used at all.
    Unavailable(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::NotFound(n) => {
                write!(f, "segredo '{n}' não encontrado no cofre do sistema")
            }
            SecretError::Unavailable(why) => write!(
                f,
                "cofre de credenciais do sistema indisponível ({why}); \
                 defina {API_KEY_ENV} ou habilite o cofre — nada foi gravado em texto puro"
            ),
        }
    }
}

impl std::error::Error for SecretError {}

/// Where secrets live. The real one is `OsKeyring`; tests inject a map.
pub trait SecretStore {
    fn get(&self, name: &str) -> Result<String, SecretError>;
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

pub struct OsKeyring;

impl OsKeyring {
    fn entry(name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, name).map_err(|e| map_err(name, e))
    }
}

fn map_err(name: &str, e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoEntry => SecretError::NotFound(name.to_string()),
        other => SecretError::Unavailable(other.to_string()),
    }
}

impl SecretStore for OsKeyring {
    fn get(&self, name: &str) -> Result<String, SecretError> {
        Self::entry(name)?
            .get_password()
            .map_err(|e| map_err(name, e))
    }
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        Self::entry(name)?
            .set_password(value)
            .map_err(|e| map_err(name, e))
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        Self::entry(name)?
            .delete_credential()
            .map_err(|e| map_err(name, e))
    }
}

pub fn set_secret(name: &str, value: &str) -> Result<(), SecretError> {
    OsKeyring.set(name, value)
}

pub fn get_secret(name: &str) -> Result<String, SecretError> {
    OsKeyring.get(name)
}

pub fn delete_secret(name: &str) -> Result<(), SecretError> {
    OsKeyring.delete(name)
}

/// `<account>.oauth`: the cofre entry that holds an OAuth credential header.
/// Same cofre as the API keys — one place to look, one place to lock.
pub fn oauth_entry(account_id: &str) -> String {
    format!("{account_id}.oauth")
}

/// Windows Credential Manager caps one credential at 2560 chars, and a Codex
/// JWT pair is longer than that. So the JSON is stored in `n` chunks and the
/// `<id>.oauth` entry keeps only the count; a legacy single-JSON entry is still
/// read, so an older install keeps working.
// ponytail: fixed 1024-char chunks (Windows caps one credential at 2560 UTF-16
// bytes = 1280 chars), rewritten whole on every save. Add
// incremental writes only if a save ever gets slow enough to notice.
const OAUTH_CHUNK_CHARS: usize = 1024;

fn chunk_key(entry: &str, i: usize) -> String {
    format!("{entry}.{i}")
}

pub fn save_oauth(
    store: &dyn SecretStore,
    account_id: &str,
    cred: &zerith_providers::OAuthCredential,
) -> Result<(), SecretError> {
    let json = serde_json::to_string(cred)
        .map_err(|e| SecretError::Unavailable(format!("credencial não serializável: {e}")))?;
    let chars: Vec<char> = json.chars().collect();
    // Clear first: a shorter credential must not leave tail chunks behind.
    clear_oauth(store, account_id)?;
    let entry = oauth_entry(account_id);
    let mut n = 0;
    for chunk in chars.chunks(OAUTH_CHUNK_CHARS) {
        let piece: String = chunk.iter().collect();
        store.set(&chunk_key(&entry, n), &piece)?;
        n += 1;
    }
    store.set(&entry, &n.to_string())
}

pub fn load_oauth(
    store: &dyn SecretStore,
    account_id: &str,
) -> Result<Option<zerith_providers::OAuthCredential>, SecretError> {
    let entry = oauth_entry(account_id);
    let header = match store.get(&entry) {
        Ok(h) => h,
        Err(SecretError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let json =
        if header.trim_start().starts_with('{') {
            header // legacy: the whole credential in one entry
        } else {
            let n: usize = header
                .trim()
                .parse()
                .map_err(|_| SecretError::Unavailable("cabeçalho oauth inválido".into()))?;
            let mut buf = String::new();
            for i in 0..n {
                buf.push_str(&store.get(&chunk_key(&entry, i)).map_err(|_| {
                    SecretError::Unavailable(format!("parte {i} da credencial sumiu"))
                })?);
            }
            buf
        };
    serde_json::from_str(&json)
        .map(Some)
        .map_err(|e| SecretError::Unavailable(format!("credencial oauth corrompida: {e}")))
}

pub fn clear_oauth(store: &dyn SecretStore, account_id: &str) -> Result<(), SecretError> {
    let entry = oauth_entry(account_id);
    if let Ok(header) = store.get(&entry) {
        if let Ok(n) = header.trim().parse::<usize>() {
            for i in 0..n {
                let _ = store.delete(&chunk_key(&entry, i));
            }
        }
    }
    match store.delete(&entry) {
        Ok(()) | Err(SecretError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The key for a provider: `ZERITH_API_KEY` first, then the keyring entry
/// named `key_ref` (defaults to the provider name).
///
/// `Ok(None)`: no key anywhere — fine for local servers, the provider decides
/// whether that is an error. `Err`: the keyring itself failed.
pub fn resolve_api_key(
    provider: &str,
    key_ref: Option<&str>,
) -> Result<Option<String>, SecretError> {
    resolve_api_key_with(&OsKeyring, |k| std::env::var(k).ok(), provider, key_ref)
}

pub fn resolve_api_key_with(
    store: &dyn SecretStore,
    env: impl Fn(&str) -> Option<String>,
    provider: &str,
    key_ref: Option<&str>,
) -> Result<Option<String>, SecretError> {
    if let Some(k) = env(API_KEY_ENV).filter(|k| !k.trim().is_empty()) {
        return Ok(Some(k));
    }
    match store.get(key_ref.unwrap_or(provider)) {
        Ok(k) => Ok(Some(k)),
        Err(SecretError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Env var and keyring entry of the OpenRouter key used for image/video
/// generation. Deliberately not `resolve_api_key`: `ZERITH_API_KEY` belongs to
/// the chat provider and is the wrong key for OpenRouter.
pub const OPENROUTER_ENV: &str = "OPENROUTER_API_KEY";
pub const OPENROUTER_ENTRY: &str = "openrouter";

/// `OPENROUTER_API_KEY`, then the keyring entry `openrouter`. `Ok(None)`: no
/// key anywhere. `Err`: the keyring failed and the env var was not set.
pub fn resolve_openrouter_key() -> Result<Option<String>, SecretError> {
    resolve_openrouter_key_with(&OsKeyring, |k| std::env::var(k).ok())
}

pub fn resolve_openrouter_key_with(
    store: &dyn SecretStore,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, SecretError> {
    if let Some(k) = env(OPENROUTER_ENV).filter(|k| !k.trim().is_empty()) {
        return Ok(Some(k.trim().to_string()));
    }
    match store.get(OPENROUTER_ENTRY) {
        Ok(k) if !k.trim().is_empty() => Ok(Some(k.trim().to_string())),
        Ok(_) | Err(SecretError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `sk-a…wxyz`: enough to tell two keys apart, not enough to use one.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "****".into();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Mem(RefCell<HashMap<String, String>>, bool);

    impl SecretStore for Mem {
        fn get(&self, n: &str) -> Result<String, SecretError> {
            if self.1 {
                return Err(SecretError::Unavailable("sem serviço".into()));
            }
            self.0
                .borrow()
                .get(n)
                .cloned()
                .ok_or_else(|| SecretError::NotFound(n.into()))
        }
        fn set(&self, n: &str, v: &str) -> Result<(), SecretError> {
            self.0.borrow_mut().insert(n.into(), v.into());
            Ok(())
        }
        fn delete(&self, n: &str) -> Result<(), SecretError> {
            self.0
                .borrow_mut()
                .remove(n)
                .map(|_| ())
                .ok_or_else(|| SecretError::NotFound(n.into()))
        }
    }

    #[test]
    fn oauth_credential_round_trips_under_the_account_entry() {
        let store = Mem(RefCell::default(), false);
        let cred = zerith_providers::OAuthCredential {
            access_token: "A".into(),
            refresh_token: Some("R".into()),
            expires_at: 42,
            account_id: None,
            tool: zerith_providers::OAuthTool::Codex,
        };
        save_oauth(&store, "codex-work", &cred).unwrap();
        let back = load_oauth(&store, "codex-work").unwrap().unwrap();
        assert_eq!(back.access_token, "A");
        assert_eq!(back.tool, zerith_providers::OAuthTool::Codex);
        assert_eq!(oauth_entry("codex-work"), "codex-work.oauth");

        clear_oauth(&store, "codex-work").unwrap();
        assert!(load_oauth(&store, "codex-work").unwrap().is_none());
        // clearing twice is not an error: logout must be idempotent
        clear_oauth(&store, "codex-work").unwrap();
    }

    #[test]
    fn a_corrupted_credential_is_an_error_not_a_panic() {
        let store = Mem(RefCell::default(), false);
        store.set(&oauth_entry("x"), "{not json").unwrap();
        assert!(matches!(
            load_oauth(&store, "x"),
            Err(SecretError::Unavailable(_))
        ));
    }

    /// Windows Credential Manager rejects anything over 2560 chars, and a Codex
    /// JWT pair is longer: the credential must survive in chunks.
    #[test]
    fn a_credential_bigger_than_one_keyring_entry_round_trips_in_chunks() {
        let store = Mem(RefCell::default(), false);
        let long = "x".repeat(6000);
        let cred = zerith_providers::OAuthCredential {
            access_token: long.clone(),
            refresh_token: Some(long.clone()),
            expires_at: 7,
            account_id: None,
            tool: zerith_providers::OAuthTool::Codex,
        };
        save_oauth(&store, "big", &cred).unwrap();

        let header = store.get(&oauth_entry("big")).unwrap();
        let n: usize = header.parse().expect("cabeçalho é o número de partes");
        assert!(n >= 3, "6000 chars não couberam em menos de 3 partes: {n}");
        for i in 0..n {
            let piece = store.get(&format!("{}.{i}", oauth_entry("big"))).unwrap();
            assert!(piece.chars().count() <= OAUTH_CHUNK_CHARS);
        }

        let back = load_oauth(&store, "big").unwrap().unwrap();
        assert_eq!(back.access_token, long);

        clear_oauth(&store, "big").unwrap();
        for i in 0..n {
            assert!(store.get(&format!("{}.{i}", oauth_entry("big"))).is_err());
        }
        assert!(load_oauth(&store, "big").unwrap().is_none());
    }

    /// An entry written by the older single-JSON format keeps being readable.
    #[test]
    fn a_legacy_single_json_entry_is_still_read() {
        let store = Mem(RefCell::default(), false);
        store
            .set(
                &oauth_entry("old"),
                r#"{"access_token":"A","expires_at":1,"tool":"codex"}"#,
            )
            .unwrap();
        assert_eq!(
            load_oauth(&store, "old").unwrap().unwrap().access_token,
            "A"
        );
    }

    #[test]
    fn env_wins_then_keyring_then_none_and_failure_is_an_error() {
        let store = Mem(RefCell::default(), false);
        store.set("openai", "from-keyring").unwrap();
        store.set("work", "from-ref").unwrap();
        let env = |k: &str| (k == API_KEY_ENV).then(|| "from-env".to_string());
        let none = |_: &str| None;

        assert_eq!(
            resolve_api_key_with(&store, env, "openai", None)
                .unwrap()
                .as_deref(),
            Some("from-env")
        );
        assert_eq!(
            resolve_api_key_with(&store, none, "openai", None)
                .unwrap()
                .as_deref(),
            Some("from-keyring")
        );
        assert_eq!(
            resolve_api_key_with(&store, none, "openai", Some("work"))
                .unwrap()
                .as_deref(),
            Some("from-ref")
        );
        assert_eq!(
            resolve_api_key_with(&store, none, "ollama", None).unwrap(),
            None
        );

        let broken = Mem(RefCell::default(), true);
        let err = resolve_api_key_with(&broken, none, "openai", None).unwrap_err();
        assert!(err.to_string().contains("texto puro"));
        // The env var still works when the keyring is down.
        assert!(resolve_api_key_with(&broken, env, "openai", None).is_ok());
    }

    #[test]
    fn openrouter_key_ignores_the_chat_key_and_prefers_env() {
        let store = Mem(RefCell::default(), false);
        let chat = |k: &str| (k == API_KEY_ENV).then(|| "chat-key".to_string());
        let none = |_: &str| None;
        // The chat provider's key must never be taken for OpenRouter's.
        assert_eq!(resolve_openrouter_key_with(&store, chat).unwrap(), None);
        store.set("openrouter", " from-keyring ").unwrap();
        assert_eq!(
            resolve_openrouter_key_with(&store, chat)
                .unwrap()
                .as_deref(),
            Some("from-keyring")
        );
        let env = |k: &str| (k == OPENROUTER_ENV).then(|| "from-env".to_string());
        assert_eq!(
            resolve_openrouter_key_with(&store, env).unwrap().as_deref(),
            Some("from-env")
        );
        let blank = |k: &str| (k == OPENROUTER_ENV).then(|| "  ".to_string());
        assert_eq!(
            resolve_openrouter_key_with(&store, blank)
                .unwrap()
                .as_deref(),
            Some("from-keyring")
        );
        let broken = Mem(RefCell::default(), true);
        assert!(resolve_openrouter_key_with(&broken, none).is_err());
        assert!(resolve_openrouter_key_with(&broken, env).is_ok());
    }

    #[test]
    fn masking_never_shows_the_middle() {
        assert_eq!(mask("short"), "****");
        assert_eq!(mask("sk-abcdefghijklmnop"), "sk-a…mnop");
    }
}
