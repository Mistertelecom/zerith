//! Provedores: four tabs for four jobs. `Provedor` is the endpoint the chat
//! talks to, `Contas` the failover gateway, `Delegação` the model split between
//! the main agent and its subagents, `Uso` the quota and the savings.
//!
//! Split by what it holds: `state` the forms and validation, `actions` what
//! the buttons do, one view module per tab.

mod accounts;
mod actions;
mod provider;
mod routing;
mod state;
mod usage;

use rune::{col, el, text, El};

pub use actions::*;
pub use state::{AccountForm, Form, ProvidersState, RoutingForm, Tab};

use crate::ui::{self, Ui};
use crate::widgets;
use crate::Shell;

pub fn view(s: &Shell, u: &Ui) -> El {
    let idx = Tab::ALL
        .iter()
        .position(|t| *t == s.providers.tab)
        .unwrap_or(0);
    let labels: Vec<&str> = Tab::ALL.iter().map(|t| t.label()).collect();
    let body = match s.providers.tab {
        Tab::Provider => provider::view(s, u),
        Tab::Accounts => accounts::view(s, u),
        Tab::Routing => routing::view(s, u),
        Tab::Usage => usage::view(s, u),
    };
    let tabs = widgets::tabs(&labels, idx, "Seção de provedores", u, |s, i| {
        set_tab(s, Tab::ALL[i])
    });
    widgets::page(
        "Provedores",
        el(),
        col()
            .w_full()
            .gap(16.0)
            .child(tabs)
            .child(widgets::scroll_body("providers-scroll", body, u)),
        u,
    )
}

/// A status line under a form: muted for success, the alert row for errors.
pub(super) fn notice_line(n: &Option<Result<String, String>>, u: &Ui) -> El {
    match n {
        Some(Ok(m)) => text(m.clone()).wrap().font(ui::LABEL).color(u.muted_text),
        Some(Err(m)) => widgets::error_line(m.clone(), u),
        None => el(),
    }
}
