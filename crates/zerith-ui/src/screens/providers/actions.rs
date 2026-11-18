//! What the Providers buttons do. Plain state plus the worker jobs
//! `Shell::work` runs, so tests drive them directly.

use rune::TextEdit;
use zerith_providers::{Client, ProviderConfig, ProviderKind};

use super::state::{model_placeholder, AccountForm, Form, Tab};
use crate::widgets::Load;
use crate::{wire, Shell};

/// Opens one tab. The usage tab reads fresh numbers the first time.
pub fn set_tab(s: &mut Shell, tab: Tab) {
    s.providers.tab = tab;
    if tab == Tab::Usage && s.providers.usage.is_idle() {
        load_usage(s);
    }
}

pub(super) fn select(s: &mut Shell, i: usize) {
    if let Some(c) = s.providers.list.get(i) {
        s.providers.form = Form::of(c);
        s.providers.selected = i;
        s.providers.probe = Load::Idle;
        s.providers.notice = None;
    }
}

pub fn apply(s: &mut Shell) {
    let c = match s.providers.from_form() {
        Ok(c) => c,
        Err(e) => {
            s.providers.notice = Some(Err(e));
            return;
        }
    };
    let i = s.providers.selected;
    if s.chat.model.is_empty() {
        s.chat.model = c.default_model.clone().unwrap_or_default();
    }
    // The cached model list belongs to an endpoint: keep it unless that moved.
    let old = &s.providers.list[i];
    if old.base_url != c.base_url || old.api_key != c.api_key {
        s.chat.models = Load::Idle;
        s.chat.link = crate::screens::chat::Link::Unknown;
    }
    s.providers.list[i] = c.clone();
    s.providers.notice = Some(Ok("Salvando…".into()));
    let root = s.root();
    // Settings file and keyring both touch the disk: off the UI thread.
    s.work(
        move || {
            let saved = wire::save_provider(&root, &c);
            let key = match &c.api_key {
                Some(k) => wire::store_key(&c.name, k),
                None => Ok(()),
            };
            (saved, key)
        },
        |s, r| {
            s.providers.notice = Some(match r {
                (Ok(()), Ok(())) => Ok("Salvo em settings.toml; chave no cofre do sistema".into()),
                (Err(e), _) => Err(format!("Não consegui salvar: {e}")),
                (Ok(()), Err(e)) => Err(format!("Salvo, mas a chave não foi para o cofre: {e}")),
            });
        },
    );
}

pub fn test(s: &mut Shell) {
    let cfg = match s.providers.from_form() {
        Ok(c) => c,
        Err(e) => {
            s.providers.notice = Some(Err(e));
            return;
        }
    };
    s.providers.probe = Load::Loading;
    s.work(
        move || {
            Client::new(cfg)
                .map(|c| c.probe())
                .map_err(|e| e.to_string())
        },
        |s, r| s.providers.probe = r.into(),
    );
}

pub fn apply_kind_preset(s: &mut Shell, kind: ProviderKind) {
    let f = &mut s.providers.form;
    f.name = TextEdit::new(kind.as_str());
    f.url = TextEdit::new(kind.default_base_url());
    f.protocol = kind.default_protocol();
    f.model_placeholder = model_placeholder(kind);
    s.providers.probe = Load::Idle;
    s.providers.notice = None;
}

/// Used by tests: applies the native Anthropic preset.
pub fn apply_preset(s: &mut Shell) {
    apply_kind_preset(s, ProviderKind::Anthropic);
}

pub fn new_account(s: &mut Shell) {
    s.providers.selected_account = None;
    s.providers.account_form = AccountForm::empty();
    s.providers.account_notice = None;
}

pub fn select_account(s: &mut Shell, i: usize) {
    if let Some(acc) = s.providers.accounts.get(i) {
        s.providers.account_form = AccountForm::of(acc);
        s.providers.selected_account = Some(i);
        s.providers.account_notice = None;
    }
}

fn sync_provider_list(s: &mut Shell, updated: Vec<ProviderConfig>) {
    if updated.is_empty() {
        return;
    }
    let active_name = s.providers.active().name;
    let pos = updated
        .iter()
        .position(|p| p.name == active_name)
        .unwrap_or(0);
    s.providers.list = updated;
    s.providers.selected = pos;
}

pub fn save_account(s: &mut Shell) {
    let (acc, api_key) = match s.providers.account_from_form() {
        Ok(v) => v,
        Err(e) => {
            s.providers.account_notice = Some(Err(e));
            return;
        }
    };
    let idx = match s.providers.accounts.iter().position(|a| a.id == acc.id) {
        Some(i) => {
            s.providers.accounts[i] = acc.clone();
            i
        }
        None => {
            s.providers.accounts.push(acc.clone());
            s.providers.accounts.len() - 1
        }
    };
    s.providers.selected_account = Some(idx);
    s.providers.account_notice = Some(Ok("Salvando conta…".into()));
    let root = s.root();
    let has_key = api_key.is_some();
    s.work(
        move || {
            let res = wire::save_account(&root, &acc, api_key.as_deref());
            let providers = if res.is_ok() {
                Some(wire::configured_providers(&root))
            } else {
                None
            };
            (res, providers)
        },
        move |s, (r, providers)| {
            if let Some(list) = providers {
                sync_provider_list(s, list);
            }
            s.providers.account_notice = Some(match r {
                Ok(()) => {
                    if has_key {
                        s.providers.account_form.key = TextEdit::new("").secure();
                        Ok("Conta salva em settings.toml; chave no cofre do sistema".into())
                    } else {
                        Ok("Conta salva em settings.toml".into())
                    }
                }
                Err(e) => Err(format!("Não consegui salvar a conta: {e}")),
            });
        },
    );
}

/// Browser login for one CLI. The wait happens on the worker thread; the
/// notice says so first, because nothing moves until the human authorizes.
pub fn oauth_login(s: &mut Shell, tool: zerith_providers::OAuthTool) {
    let typed = s.providers.account_form.id.text.trim().to_string();
    let account = if typed.is_empty() {
        tool.as_str().to_string()
    } else {
        typed
    };
    s.providers.account_form.id = TextEdit::new(account.clone());
    s.providers.account_notice = Some(Ok(format!(
        "Abrindo o navegador para {}… autorize e volte para esta janela",
        tool.as_str()
    )));
    let root = s.root();
    let who = account.clone();
    s.work(
        move || wire::oauth_login(&root, tool, &account),
        move |s, r| {
            s.providers.accounts = wire::configured_accounts(&s.root());
            if let Some(i) = s.providers.accounts.iter().position(|a| a.id == who) {
                s.providers.selected_account = Some(i);
                s.providers.account_form = AccountForm::of(&s.providers.accounts[i]);
            }
            s.providers.account_notice = Some(match r {
                Ok(msg) => Ok(msg),
                Err(e) => Err(format!("Não consegui entrar: {e}")),
            });
        },
    );
}

pub fn toggle_account_enabled(s: &mut Shell, id: String) {
    let Some(idx) = s.providers.accounts.iter().position(|a| a.id == id) else {
        return;
    };
    s.providers.accounts[idx].enabled = !s.providers.accounts[idx].enabled;
    let acc = s.providers.accounts[idx].clone();
    if s.providers.selected_account == Some(idx) {
        s.providers.account_form.enabled = acc.enabled;
    }
    let root = s.root();
    s.work(
        move || wire::save_account(&root, &acc, None),
        |s, r| {
            if let Err(e) = r {
                s.providers.account_notice =
                    Some(Err(format!("Não consegui atualizar a conta: {e}")));
            }
        },
    );
}

pub fn delete_account(s: &mut Shell, id: String) {
    s.providers.accounts.retain(|a| a.id != id);
    if s.providers.accounts.is_empty() {
        s.providers.selected_account = None;
        s.providers.account_form = AccountForm::empty();
    } else {
        s.providers.selected_account = Some(0);
        s.providers.account_form = AccountForm::of(&s.providers.accounts[0]);
    }
    let root = s.root();
    s.work(
        move || {
            let res = wire::delete_account(&root, &id);
            let providers = if res.is_ok() {
                Some(wire::configured_providers(&root))
            } else {
                None
            };
            (res, providers)
        },
        |s, (r, providers)| {
            if let Some(list) = providers {
                sync_provider_list(s, list);
            }
            s.providers.account_notice = Some(match r {
                Ok(()) => Ok("Conta removida".into()),
                Err(e) => Err(format!("Não consegui remover a conta: {e}")),
            });
        },
    );
}

pub fn clear_cooldown(s: &mut Shell, account_id: String) {
    let db = s.db().to_path_buf();
    let label = account_id.clone();
    s.work(
        move || wire::clear_account_cooldown(&db, &account_id),
        move |s, r| match r {
            Ok(()) => {
                s.providers.account_notice = Some(Ok(format!("Cooldown limpo para {label}")));
                load_usage(s);
            }
            Err(e) => {
                s.providers.account_notice =
                    Some(Err(format!("Não consegui limpar o cooldown: {e}")));
            }
        },
    );
}

pub fn save_routing(s: &mut Shell) {
    let clean = |t: &str| {
        let trimmed = t.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    s.providers.routing.main_model = clean(&s.providers.routing_form.main_model.text);
    s.providers.routing.subagent_model = clean(&s.providers.routing_form.subagent_model.text);
    if !s.chat.busy {
        if let Some(m) = &s.providers.routing.main_model {
            s.chat.model = m.clone();
        }
    }
    s.providers.routing_notice = Some(Ok("Salvando delegação…".into()));
    let root = s.root();
    let routing = s.providers.routing.clone();
    s.work(
        move || {
            let res = wire::save_routing(&root, &routing);
            let providers = if res.is_ok() {
                Some(wire::configured_providers(&root))
            } else {
                None
            };
            (res, providers)
        },
        |s, (r, providers)| {
            if let Some(list) = providers {
                sync_provider_list(s, list);
            }
            s.providers.routing_notice = Some(match r {
                Ok(()) => Ok("Delegação salva em settings.toml".into()),
                Err(e) => Err(format!("Não consegui salvar a delegação: {e}")),
            });
        },
    );
}

pub fn load_usage(s: &mut Shell) {
    s.providers.usage = Load::Loading;
    let db = s.db().to_path_buf();
    s.work(
        move || wire::usage_summary(&db, 24),
        |s, r| s.providers.usage = r.into(),
    );
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
