//! An account with `auth = "oauth"` must reach the provider carrying a Bearer
//! credential read from the cofre — and must not demand an API key.

use std::cell::RefCell;
use std::collections::HashMap;
use zerith_core::secrets::{self, SecretError, SecretStore};
use zerith_core::settings::{AccountEntry, AuthMode, Settings};
use zerith_providers::{OAuthCredential, OAuthTool, ProviderConfig};

struct Mem(RefCell<HashMap<String, String>>);

impl Mem {
    fn new() -> Self {
        Self(RefCell::default())
    }
}

impl SecretStore for Mem {
    fn get(&self, n: &str) -> Result<String, SecretError> {
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

fn oauth_account() -> AccountEntry {
    AccountEntry {
        id: "claude-assinatura".into(),
        provider: "anthropic".into(),
        auth: AuthMode::Oauth,
        oauth_tool: Some("claude-code".into()),
        ..Default::default()
    }
}

fn credential(expires_at: i64) -> OAuthCredential {
    OAuthCredential {
        access_token: "tok".into(),
        refresh_token: Some("ref".into()),
        expires_at,
        account_id: None,
        tool: OAuthTool::ClaudeCode,
    }
}

#[test]
fn oauth_account_carries_the_credential_and_no_key() {
    let store = Mem::new();
    secrets::save_oauth(&store, "claude-assinatura", &credential(i64::MAX / 2)).unwrap();

    let settings = Settings::default();
    let acc = oauth_account();
    let cand = zerith_runtime::candidate_from_account_with(
        &settings,
        &acc,
        "claude-opus-5-5",
        &ProviderConfig::default(),
        &store,
    );

    assert_eq!(cand.config.auth, zerith_providers::AuthMode::Oauth);
    assert_eq!(
        cand.config.oauth.as_ref().map(|c| c.access_token.as_str()),
        Some("tok")
    );
    assert_eq!(cand.config.api_key, None);
    assert_eq!(cand.config.protocol, zerith_providers::Protocol::Anthropic);
}

#[test]
fn api_key_account_keeps_the_old_path() {
    let store = Mem::new();
    store.set("openai", "sk-1").unwrap();
    let settings = Settings::default();
    let acc = AccountEntry {
        id: "work".into(),
        provider: "openai".into(),
        ..Default::default()
    };
    let cand = zerith_runtime::candidate_from_account_with(
        &settings,
        &acc,
        "gpt-6",
        &ProviderConfig::default(),
        &store,
    );
    assert_eq!(cand.config.auth, zerith_providers::AuthMode::ApiKey);
    assert_eq!(cand.config.oauth, None);
    assert_eq!(cand.config.api_key.as_deref(), Some("sk-1"));
}

/// No credential in the cofre: the candidate is still built (the turn reports
/// `NoKey` or the gateway fails over), it must not panic or invent a token.
#[test]
fn a_missing_credential_leaves_the_account_without_any_secret() {
    let store = Mem::new();
    let settings = Settings::default();
    let cand = zerith_runtime::candidate_from_account_with(
        &settings,
        &oauth_account(),
        "claude-opus-5-5",
        &ProviderConfig::default(),
        &store,
    );
    assert_eq!(cand.config.auth, zerith_providers::AuthMode::Oauth);
    assert!(cand.config.oauth.is_none());
    assert_eq!(cand.config.api_key, None);
}

/// Thread-safe store for the parallel test (the `Mem` above is single-thread).
struct Shared(std::sync::Mutex<HashMap<String, String>>);

impl SecretStore for Shared {
    fn get(&self, n: &str) -> Result<String, SecretError> {
        let m = self.0.lock().unwrap();
        m.get(n)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(n.into()))
    }
    fn set(&self, n: &str, v: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(n.into(), v.into());
        Ok(())
    }
    fn delete(&self, n: &str) -> Result<(), SecretError> {
        let mut m = self.0.lock().unwrap();
        m.remove(n)
            .map(|_| ())
            .ok_or_else(|| SecretError::NotFound(n.into()))
    }
}

fn expired(tool: OAuthTool) -> OAuthCredential {
    OAuthCredential {
        access_token: "old".into(),
        refresh_token: Some("rotating".into()),
        expires_at: 0,
        account_id: None,
        tool,
    }
}

#[test]
fn parallel_turns_refresh_a_rotating_token_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let store = Shared(Default::default());
    let mut acc = oauth_account();
    acc.id = "claude-par".into();
    secrets::save_oauth(&store, &acc.id, &expired(OAuthTool::ClaudeCode)).unwrap();
    let calls = AtomicUsize::new(0);
    let refresh = |c: &OAuthCredential| {
        calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut f = c.clone();
        f.access_token = "fresh".into();
        f.expires_at = i64::MAX / 2;
        Ok(f)
    };
    let got: Vec<String> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..6)
            .map(|_| {
                s.spawn(|| {
                    zerith_runtime::hydrate_oauth_with(&acc, &store, &refresh)
                        .unwrap()
                        .access_token
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one refresh for six turns");
    assert!(got.iter().all(|t| t == "fresh"), "{got:?}");
}

#[test]
fn a_codex_token_is_never_refreshed_by_zerith() {
    let store = Shared(Default::default());
    let mut acc = oauth_account();
    acc.id = "codex-x".into();
    secrets::save_oauth(&store, &acc.id, &expired(OAuthTool::Codex)).unwrap();
    let out =
        zerith_runtime::hydrate_oauth_with(&acc, &store, &|_| panic!("codex refreshed")).unwrap();
    assert_eq!(out.access_token, "old");
}
