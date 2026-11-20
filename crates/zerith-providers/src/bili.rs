//! Routing through a local billion-context proxy
//! (<https://github.com/ranxianglei/billion-context>, MIT + attribution).
//!
//! The host starts the proxy (`zerith_runtime::bili`) and sets its origin
//! here. Every chat request of a routable account then goes to
//! `{origin}/bili/{base_url}/{path}`, keyed to the conversation by
//! `x-acp-session`, and the proxy folds old history on the wire. No origin, or
//! a proxy that stopped answering, means direct, exactly as before.

use std::cell::RefCell;
use std::net::{SocketAddr, TcpStream};
use std::sync::RwLock;
use std::time::Duration;

use crate::{AuthMode, OAuthTool, Protocol, ProviderConfig};

static ORIGIN: RwLock<Option<String>> = RwLock::new(None);

thread_local! {
    static CONVERSATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn set_origin(origin: Option<String>) {
    *ORIGIN.write().unwrap_or_else(|e| e.into_inner()) = origin;
}

pub fn origin() -> Option<String> {
    ORIGIN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether the proxy can carry this account's traffic at all.
fn routable(cfg: &ProviderConfig) -> bool {
    let wire = matches!(
        cfg.protocol,
        Protocol::OpenAi | Protocol::Anthropic | Protocol::Responses
    );
    // The Claude subscription answers only when the FIRST system block is the
    // Claude Code identity, and the proxy rewrites the system prompt.
    // ponytail: kept direct until it is proven against a live subscription.
    let claude_sub = cfg.auth == AuthMode::Oauth
        && cfg
            .oauth
            .as_ref()
            .is_some_and(|c| c.tool == OAuthTool::ClaudeCode);
    let http = cfg.base_url.starts_with("https://") || cfg.base_url.starts_with("http://");
    wire && !claude_sub && http
}

/// Whether requests of `cfg` go through the proxy right now. The host uses it
/// to step its own summary aside when the proxy does the folding, so a proxy
/// that died is checked (and forgotten) here, not only at send time.
pub fn routes(cfg: &ProviderConfig) -> bool {
    routable(cfg) && alive()
}

fn alive() -> bool {
    let Some(o) = origin() else {
        return false;
    };
    if listening(&o) {
        return true;
    }
    eprintln!("zerith: billion-context não respondeu em {o}; seguindo direto");
    set_origin(None);
    false
}

/// The URL a chat request of `cfg` is sent to. A proxy that does not accept
/// the connection is forgotten on the spot: the request goes direct and the
/// host starts a new proxy on the next turn.
pub(crate) fn url(cfg: &ProviderConfig, direct: String) -> String {
    if !routes(cfg) {
        return direct;
    }
    match origin() {
        Some(o) => format!("{}/bili/{direct}", o.trim_end_matches('/')),
        None => direct,
    }
}

/// The conversation id the proxy keys its compression state on, when this
/// request goes through it.
pub(crate) fn session_header(cfg: &ProviderConfig) -> Option<String> {
    if !routable(cfg) || origin().is_none() {
        return None;
    }
    CONVERSATION.with(|c| c.borrow().clone())
}

/// A TCP connect to the loopback port: well under a millisecond.
pub fn listening(origin: &str) -> bool {
    let hostport = origin
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .replace("localhost", "127.0.0.1");
    hostport
        .parse::<SocketAddr>()
        .is_ok_and(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
}

/// Requests made on this thread belong to conversation `id` until the guard
/// drops (a turn, a subagent's turn). The previous id comes back after.
pub fn conversation(id: &str) -> Conversation {
    Conversation(CONVERSATION.with(|c| c.replace(Some(id.to_string()))))
}

pub struct Conversation(Option<String>);

impl Drop for Conversation {
    fn drop(&mut self) {
        let prev = self.0.take();
        CONVERSATION.with(|c| *c.borrow_mut() = prev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OAuthCredential, ProviderKind};

    // The origin is process-global: tests here never set it, or a parallel
    // provider test would be sent to a proxy that is not there.

    #[test]
    fn which_accounts_can_go_through_the_proxy() {
        assert!(routable(&ProviderConfig::new(ProviderKind::OpenRouter)));
        assert!(routable(&ProviderConfig::new(ProviderKind::Anthropic)));
        let mut sub = ProviderConfig::new(ProviderKind::Anthropic);
        sub.auth = AuthMode::Oauth;
        sub.oauth = Some(OAuthCredential {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: 0,
            account_id: None,
            tool: OAuthTool::ClaudeCode,
        });
        assert!(!routable(&sub), "Claude subscription stays direct");
        let mut gem = ProviderConfig::new(ProviderKind::Gemini);
        gem.protocol = Protocol::Gemini;
        assert!(!routable(&gem), "Gemini wire is not proxied");
    }

    #[test]
    fn a_closed_port_is_not_a_proxy_and_the_conversation_guard_restores() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live = format!("http://{}", open.local_addr().unwrap());
        assert!(listening(&live));
        drop(open);
        assert!(!listening(&live));
        assert!(!listening("http://nao-e-um-endereco"));

        let now = || CONVERSATION.with(|c| c.borrow().clone());
        {
            let _a = conversation("a");
            {
                let _b = conversation("b");
                assert_eq!(now().as_deref(), Some("b"));
            }
            assert_eq!(now().as_deref(), Some("a"));
        }
        assert_eq!(now(), None);
    }
}
