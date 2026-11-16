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
    fn masking_never_shows_the_middle() {
        assert_eq!(mask("short"), "****");
        assert_eq!(mask("sk-abcdefghijklmnop"), "sk-a…mnop");
    }
}
