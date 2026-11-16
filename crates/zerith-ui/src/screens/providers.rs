//! Model endpoints, native provider presets, multi-account gateway, routing
//! delegation, and rolling-window quota/usage telemetry.

use std::collections::HashSet;

use rune::{col, el, row, rule, text, El, ScrollSpec, TextEdit, Tx};
use rune_kit::{Badge, BadgeVariant, SegmentedControl, Switch};
use zerith_core::{AccountEntry, RoutingSettings};
use zerith_providers::{Client, Probe, Protocol, ProviderConfig, ProviderKind};
use zerith_sessions::{AccountWindowStats, UsageSummary};

use crate::agentwork;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, labelled, Load, Tone};
use crate::{wire, Shell};

struct Preset {
    kind: ProviderKind,
    key: &'static str,
    label: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset {
        kind: ProviderKind::Anthropic,
        key: "preset-anthropic",
        label: "Anthropic",
    },
    Preset {
        kind: ProviderKind::DeepSeek,
        key: "preset-deepseek",
        label: "DeepSeek",
    },
    Preset {
        kind: ProviderKind::OpenAi,
        key: "preset-openai",
        label: "OpenAI",
    },
    Preset {
        kind: ProviderKind::Gemini,
        key: "preset-gemini",
        label: "Gemini",
    },
    Preset {
        kind: ProviderKind::OpenRouter,
        key: "preset-openrouter",
        label: "OpenRouter",
    },
    Preset {
        kind: ProviderKind::Ollama,
        key: "preset-ollama",
        label: "Ollama",
    },
];

fn model_placeholder(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "ex.: claude-sonnet-4",
        ProviderKind::DeepSeek => "ex.: deepseek-chat",
        ProviderKind::OpenAi => "ex.: gpt-4o",
        ProviderKind::Gemini => "ex.: gemini-2.5-flash",
        ProviderKind::OpenRouter => "ex.: anthropic/claude-sonnet-4",
        ProviderKind::Ollama => "ex.: qwen2.5-coder",
        ProviderKind::LmStudio => "ex.: local-model",
        ProviderKind::Custom => "ex.: modelo-padrão",
    }
}

pub struct Form {
    pub name: TextEdit,
    pub url: TextEdit,
    pub model: TextEdit,
    pub timeout: TextEdit,
    /// Masked, wiped on drop by Rune's secure editing. Never written to disk.
    pub key: TextEdit,
    pub protocol: Protocol,
    pub model_placeholder: &'static str,
}

impl Form {
    fn of(c: &ProviderConfig) -> Self {
        Self {
            name: TextEdit::new(c.name.clone()),
            url: TextEdit::new(c.base_url.clone()),
            model: TextEdit::new(c.default_model.clone().unwrap_or_default()),
            timeout: TextEdit::new(c.timeout_ms.to_string()),
            key: TextEdit::new(c.api_key.clone().unwrap_or_default()).secure(),
            protocol: c.protocol,
            model_placeholder: model_placeholder(c.kind),
        }
    }
}

pub struct AccountForm {
    pub id: TextEdit,
    pub provider: TextEdit,
    pub priority: TextEdit,
    pub quota_tokens: TextEdit,
    pub window_hours: TextEdit,
    pub default_model: TextEdit,
    /// Masked, wiped on drop by Rune's secure editing. Never written to disk.
    pub key: TextEdit,
    pub enabled: bool,
}

impl AccountForm {
    pub fn empty() -> Self {
        Self {
            id: TextEdit::new(""),
            provider: TextEdit::new(""),
            priority: TextEdit::new("0"),
            quota_tokens: TextEdit::new(""),
            window_hours: TextEdit::new("5"),
            default_model: TextEdit::new(""),
            key: TextEdit::new("").secure(),
            enabled: true,
        }
    }

    pub fn of(a: &AccountEntry) -> Self {
        Self {
            id: TextEdit::new(a.id.clone()),
            provider: TextEdit::new(a.provider.clone()),
            priority: TextEdit::new(a.priority.to_string()),
            quota_tokens: TextEdit::new(a.quota_tokens.map(|q| q.to_string()).unwrap_or_default()),
            window_hours: TextEdit::new(a.window_hours.to_string()),
            default_model: TextEdit::new(a.default_model.clone().unwrap_or_default()),
            key: TextEdit::new("").secure(),
            enabled: a.enabled,
        }
    }
}

pub struct RoutingForm {
    pub main_model: TextEdit,
    pub subagent_model: TextEdit,
}

impl RoutingForm {
    fn of(r: &RoutingSettings) -> Self {
        Self {
            main_model: TextEdit::new(r.main_model.clone().unwrap_or_default()),
            subagent_model: TextEdit::new(r.subagent_model.clone().unwrap_or_default()),
        }
    }
}

pub struct ProvidersState {
    pub list: Vec<ProviderConfig>,
    pub selected: usize,
    pub form: Form,
    pub probe: Load<Probe>,
    pub notice: Option<Result<String, String>>,
    pub accounts: Vec<AccountEntry>,
    pub selected_account: Option<usize>,
    pub account_form: AccountForm,
    pub account_notice: Option<Result<String, String>>,
    pub routing: RoutingSettings,
    pub routing_form: RoutingForm,
    pub routing_notice: Option<Result<String, String>>,
    pub usage: Load<UsageSummary>,
}

impl ProvidersState {
    pub fn new(list: Vec<ProviderConfig>) -> Self {
        Self::with_gateway(list, Vec::new(), RoutingSettings::default())
    }

    pub fn with_gateway(
        list: Vec<ProviderConfig>,
        accounts: Vec<AccountEntry>,
        routing: RoutingSettings,
    ) -> Self {
        let list = if list.is_empty() {
            vec![ProviderConfig::default()]
        } else {
            list
        };
        let form = Form::of(&list[0]);
        let (selected_account, account_form) = match accounts.first() {
            Some(a) => (Some(0), AccountForm::of(a)),
            None => (None, AccountForm::empty()),
        };
        let routing_form = RoutingForm::of(&routing);
        Self {
            list,
            selected: 0,
            form,
            probe: Load::Idle,
            notice: None,
            accounts,
            selected_account,
            account_form,
            account_notice: None,
            routing,
            routing_form,
            routing_notice: None,
            usage: Load::Idle,
        }
    }

    /// The provider the chat talks to: the selected one as last applied.
    pub fn active(&self) -> ProviderConfig {
        self.list.get(self.selected).cloned().unwrap_or_default()
    }

    /// The form read back into a config. Errors name the field.
    pub fn from_form(&self) -> Result<ProviderConfig, String> {
        let mut c = self.active();
        let f = &self.form;
        let url = f.url.text.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL precisa começar com http:// ou https://".into());
        }
        c.name = f.name.text.trim().to_string();
        if c.name.is_empty() {
            return Err("Dê um nome ao provedor".into());
        }
        c.kind = ProviderKind::parse(&c.name);
        c.protocol = f.protocol;
        c.base_url = url.to_string();
        c.default_model = Some(f.model.text.trim().to_string()).filter(|m| !m.is_empty());
        c.timeout_ms = f
            .timeout
            .text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|t| (1_000..=600_000).contains(t))
            .ok_or("Timeout entre 1000 e 600000 ms")?;
        c.api_key = Some(f.key.text.clone()).filter(|k| !k.is_empty());
        Ok(c)
    }

    /// Reads and validates the gateway account form.
    pub fn account_from_form(&self) -> Result<(AccountEntry, Option<String>), String> {
        let f = &self.account_form;
        let id = f.id.text.trim().to_string();
        if id.is_empty() {
            return Err("Informe o ID da conta".into());
        }
        let provider = f.provider.text.trim().to_string();
        if provider.is_empty() {
            return Err("Informe o provedor da conta".into());
        }
        let prio_str = f.priority.text.trim();
        let priority = if prio_str.is_empty() {
            0
        } else {
            prio_str
                .parse::<i32>()
                .map_err(|_| "Prioridade precisa ser um número inteiro".to_string())?
        };
        let quota_str = f.quota_tokens.text.trim();
        let quota_tokens = if quota_str.is_empty() {
            None
        } else {
            let q = quota_str
                .parse::<u64>()
                .ok()
                .filter(|&v| v > 0)
                .ok_or("Cota de tokens precisa ser um inteiro positivo")?;
            Some(q)
        };
        let win_str = f.window_hours.text.trim();
        let window_hours = if win_str.is_empty() {
            5
        } else {
            win_str
                .parse::<u32>()
                .ok()
                .filter(|&v| v >= 1)
                .ok_or("Janela precisa ter pelo menos 1 hora")?
        };
        let default_model = Some(f.default_model.text.trim().to_string()).filter(|m| !m.is_empty());
        let api_key = Some(f.key.text.clone()).filter(|k| !k.is_empty());

        let mut entry = self
            .selected_account
            .and_then(|i| self.accounts.get(i))
            .filter(|a| a.id == id)
            .cloned()
            .unwrap_or_default();
        entry.id = id.clone();
        entry.provider = provider;
        entry.priority = priority;
        entry.quota_tokens = quota_tokens;
        entry.window_hours = window_hours;
        entry.default_model = default_model;
        entry.enabled = f.enabled;
        if entry.api_key_ref.is_none() && api_key.is_some() {
            entry.api_key_ref = Some(id);
        }
        Ok((entry, api_key))
    }
}

fn select(s: &mut Shell, i: usize) {
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn probe_line(p: &Load<Probe>, u: &Ui) -> El {
    match p {
        Load::Idle => el(),
        Load::Loading => row()
            .gap(8.0)
            .center_y()
            .role("status")
            .label("Testando conexão")
            .child(widgets::dot(u.warn))
            .child(text("Testando…").font(ui::SMALL).color(u.muted_text)),
        Load::Failed(e) => widgets::error_line(e.clone(), u),
        Load::Ready(p) => {
            let (color, head) = if p.ok {
                (u.ok, format!("Conectado em {} ms", p.latency_ms))
            } else if p.status == 0 {
                (u.danger, format!("Sem resposta ({} ms)", p.latency_ms))
            } else {
                (u.warn, format!("HTTP {} em {} ms", p.status, p.latency_ms))
            };
            let detail: String = p.detail.chars().take(240).collect();
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .role("status")
                .label(format!("{head}. {detail}"))
                .child(widgets::dot(color))
                .child(text(head).font(ui::SMALL).color(u.content))
                .child(
                    text(detail)
                        .font(ui::SMALL)
                        .color(u.muted_text)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0)
                        .selectable(true),
                )
        }
    }
}

fn notice_line(n: &Option<Result<String, String>>, u: &Ui) -> El {
    match n {
        Some(Ok(m)) => text(m.clone()).wrap().font(ui::LABEL).color(u.muted_text),
        Some(Err(m)) => widgets::error_line(m.clone(), u),
        None => el(),
    }
}

fn providers_section(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let mut list = col()
        .key("provider-list")
        .role("list")
        .label("Provedores")
        .w(200.0)
        .gap(2.0);
    for (i, c) in p.list.iter().enumerate() {
        let on = i == p.selected;
        list = list.child(
            col()
                .key(format!("prov-{i}"))
                .w_full()
                .py(8.0)
                .px(10.0)
                .gap(2.0)
                .radius(8.0)
                .fill(if on { u.muted } else { u.surface })
                .hover(u.hover)
                .role("button")
                .label(c.name.clone())
                .checked(on)
                .focusable(true)
                .on_press(move |s: &mut Shell| select(s, i))
                .child(text(c.name.clone()).font(ui::BODY).color(u.content))
                .child(
                    text(c.base_url.clone())
                        .font(ui::LABEL)
                        .color(u.faint)
                        .truncate(),
                ),
        );
    }
    list = list.child(
        text("O primeiro pode vir do ambiente (ZERITH_*); provedores, contas e delegação vêm de settings.toml.")
            .wrap()
            .font(ui::LABEL)
            .color(u.faint)
            .mt(10.0),
    );

    let f = &p.form;
    let mut preset_row = row().w_full().gap(6.0).flex_wrap();
    for preset in PRESETS {
        let kind = preset.kind;
        preset_row = preset_row.child(button(
            preset.key,
            preset.label,
            Tone::Plain,
            true,
            u,
            move |s, _| apply_kind_preset(s, kind),
        ));
    }
    let proto_idx = match f.protocol {
        Protocol::OpenAi => 0,
        Protocol::Anthropic => 1,
    };

    let form = col()
        .key("provider-form")
        .grow(1.0)
        .min_w(0.0)
        .max_w(520.0)
        .gap(14.0)
        .child(labelled("Presets nativos", preset_row, u))
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(
                    labelled(
                        "Nome",
                        field(
                            "prov-name",
                            &f.name,
                            "Nome",
                            "anthropic",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.form.name,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    col()
                        .gap(6.0)
                        .child(eyebrow("Protocolo", u))
                        .child(
                            SegmentedControl::new(&["OpenAI", "Anthropic"], Some(proto_idx))
                                .label("Protocolo")
                                .fit()
                                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                                    s.providers.form.protocol =
                                        [Protocol::OpenAi, Protocol::Anthropic][i];
                                })
                                .build(&kit),
                        ),
                ),
        )
        .child(labelled(
            "URL",
            field(
                "prov-url",
                &f.url,
                "URL do endpoint",
                "https://api.anthropic.com/v1",
                None,
                u,
                |s: &mut Shell| &mut s.providers.form.url,
                |i| i,
            ),
            u,
        ))
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(
                    labelled(
                        "Modelo padrão",
                        field(
                            "prov-model",
                            &f.model,
                            "Modelo padrão",
                            f.model_placeholder,
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.form.model,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Timeout (ms)",
                        field(
                            "prov-timeout",
                            &f.timeout,
                            "Timeout em milissegundos",
                            "60000",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.form.timeout,
                            |i| i,
                        ),
                        u,
                    )
                    .w(140.0),
                ),
        )
        .child(labelled(
            "Chave de API",
            field(
                "prov-key",
                &f.key,
                "Chave de API",
                "não definida",
                None,
                u,
                |s: &mut Shell| &mut s.providers.form.key,
                |i| i,
            ),
            u,
        ))
        .child(
            text("A chave vai para o cofre do sistema (Credential Manager / Keychain), nunca para settings.toml.")
                .wrap()
                .font(ui::LABEL)
                .color(u.faint),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(button(
                    "prov-test",
                    if p.probe.is_loading() {
                        "Testando…"
                    } else {
                        "Testar conexão"
                    },
                    Tone::Plain,
                    !p.probe.is_loading(),
                    u,
                    |s, _| test(s),
                ))
                .child(button(
                    "prov-apply",
                    "Aplicar",
                    Tone::Primary,
                    true,
                    u,
                    |s, _| apply(s),
                )),
        )
        .child(probe_line(&p.probe, u))
        .child(notice_line(&p.notice, u));

    row().w_full().gap(28.0).child(list).child(form)
}

fn accounts_section(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let now = now_ms();
    let usage_accounts = p
        .usage
        .ready()
        .map(|sum| sum.accounts.as_slice())
        .unwrap_or(&[]);

    let mut list = col()
        .key("account-list")
        .role("list")
        .label("Contas do Gateway")
        .w(240.0)
        .gap(4.0);

    if p.accounts.is_empty() {
        list = list.child(
            text("Nenhuma conta cadastrada. Adicione múltiplas contas do mesmo provedor para failover por prioridade e cota.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        );
    } else {
        for (i, acc) in p.accounts.iter().enumerate() {
            let on = p.selected_account == Some(i);
            let id_toggle = acc.id.clone();
            let id_cooldown = acc.id.clone();
            let in_cooldown = usage_accounts
                .iter()
                .find(|st| st.account_id == acc.id)
                .is_some_and(|st| st.cooldown_until > now);
            let mut card = col()
                .key(format!("acc-{i}"))
                .w_full()
                .py(8.0)
                .px(10.0)
                .gap(6.0)
                .radius(8.0)
                .fill(if on { u.muted } else { u.surface })
                .border(1.0, if on { u.line_strong } else { u.line })
                .hover(u.hover)
                .role("button")
                .label(format!("Conta {} ({})", acc.id, acc.provider))
                .checked(on)
                .focusable(true)
                .on_press(move |s: &mut Shell| select_account(s, i))
                .child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(
                            col()
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(2.0)
                                .child(
                                    text(acc.id.clone())
                                        .font(ui::BODY)
                                        .color(u.content)
                                        .truncate(),
                                )
                                .child(
                                    text(format!("{} · prioridade {}", acc.provider, acc.priority))
                                        .font(ui::LABEL)
                                        .color(u.faint)
                                        .truncate(),
                                ),
                        )
                        .child(
                            Switch::new(acc.enabled)
                                .label(format!("Ativar conta {}", acc.id))
                                .hide_label()
                                .on_toggle(move |s: &mut Shell| {
                                    toggle_account_enabled(s, id_toggle.clone());
                                })
                                .build(&kit),
                        ),
                );
            if in_cooldown {
                card = card.child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(widgets::dot(u.warn))
                        .child(text("Em cooldown").font(ui::LABEL).color(u.warn).grow(1.0))
                        .child(button(
                            format!("acc-clear-cd-{i}"),
                            "Limpar cooldown",
                            Tone::Plain,
                            true,
                            u,
                            move |s, _| clear_cooldown(s, id_cooldown.clone()),
                        )),
                );
            }
            list = list.child(card);
        }
    }

    let af = &p.account_form;
    let selected_id = p
        .selected_account
        .and_then(|i| p.accounts.get(i))
        .map(|a| a.id.clone());

    let mut actions = row().w_full().gap(8.0).center_y().child(button(
        "acc-save",
        "Salvar conta",
        Tone::Primary,
        true,
        u,
        |s, _| save_account(s),
    ));
    if let Some(sel_id) = selected_id {
        let cd_id = sel_id.clone();
        let del_id = sel_id;
        actions = actions
            .child(button(
                "acc-clear-cooldown",
                "Limpar cooldown",
                Tone::Plain,
                true,
                u,
                move |s, _| clear_cooldown(s, cd_id.clone()),
            ))
            .child(el().grow(1.0))
            .child(button(
                "acc-delete",
                "Remover",
                Tone::Danger,
                true,
                u,
                move |s, _| delete_account(s, del_id.clone()),
            ));
    }

    let form = col()
        .key("account-form")
        .grow(1.0)
        .min_w(0.0)
        .max_w(520.0)
        .gap(12.0)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(
                    labelled(
                        "ID da conta",
                        field(
                            "acc-id",
                            &af.id,
                            "ID da conta",
                            "ex.: anthropic-trabalho",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.id,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Provedor",
                        field(
                            "acc-provider",
                            &af.provider,
                            "Provedor",
                            "ex.: anthropic",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.provider,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(
                    labelled(
                        "Prioridade",
                        field(
                            "acc-priority",
                            &af.priority,
                            "Prioridade (menor número roda primeiro)",
                            "0",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.priority,
                            |i| i,
                        ),
                        u,
                    )
                    .w(110.0),
                )
                .child(
                    labelled(
                        "Cota de tokens (janela)",
                        field(
                            "acc-quota",
                            &af.quota_tokens,
                            "Cota de tokens na janela",
                            "opcional, ex.: 500000",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.quota_tokens,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Janela (h)",
                        field(
                            "acc-window",
                            &af.window_hours,
                            "Janela em horas",
                            "5",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.window_hours,
                            |i| i,
                        ),
                        u,
                    )
                    .w(100.0),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(
                    labelled(
                        "Modelo padrão",
                        field(
                            "acc-model",
                            &af.default_model,
                            "Modelo padrão da conta",
                            "opcional, ex.: claude-sonnet-4",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.default_model,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Estado",
                        Switch::new(af.enabled)
                            .label(if af.enabled { "Ativa" } else { "Pausada" })
                            .on_toggle(|s: &mut Shell| {
                                s.providers.account_form.enabled =
                                    !s.providers.account_form.enabled;
                            })
                            .build(&kit),
                        u,
                    )
                    .w(130.0),
                ),
        )
        .child(labelled(
            "Chave de API",
            field(
                "acc-key",
                &af.key,
                "Chave de API da conta",
                "deixe vazio para manter a chave atual no cofre",
                None,
                u,
                |s: &mut Shell| &mut s.providers.account_form.key,
                |i| i,
            ),
            u,
        ))
        .child(actions)
        .child(notice_line(&p.account_notice, u));

    col()
        .w_full()
        .gap(14.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(eyebrow("Contas do Gateway ([[accounts]])", u).grow(1.0))
                .child(button(
                    "acc-new",
                    "Nova conta",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| new_account(s),
                )),
        )
        .child(row().w_full().gap(28.0).child(list).child(form))
}

fn routing_section(s: &Shell, u: &Ui) -> El {
    let p = &s.providers;
    let rf = &p.routing_form;
    col()
        .key("routing-section")
        .w_full()
        .max_w(560.0)
        .gap(12.0)
        .child(eyebrow("Orquestração & Delegação de Subagentes ([routing])", u))
        .child(
            text("Use provedor/modelo (ex.: anthropic/claude-opus-5-5) ou conta:modelo para definir o modelo principal e o dos subagentes.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(labelled(
            "Modelo principal",
            field(
                "route-main",
                &rf.main_model,
                "Modelo principal",
                "ex.: anthropic/claude-opus-5-5",
                None,
                u,
                |s: &mut Shell| &mut s.providers.routing_form.main_model,
                |i| i,
            ),
            u,
        ))
        .child(labelled(
            "Modelo dos subagentes",
            field(
                "route-subagent",
                &rf.subagent_model,
                "Modelo dos subagentes",
                "ex.: deepseek/deepseek-4.1-flash",
                None,
                u,
                |s: &mut Shell| &mut s.providers.routing_form.subagent_model,
                |i| i,
            ),
            u,
        ))
        .child(row().w_full().gap(8.0).child(button(
            "route-save",
            "Salvar delegação",
            Tone::Primary,
            true,
            u,
            |s, _| save_routing(s),
        )))
        .child(notice_line(&p.routing_notice, u))
}

struct UsageRow<'a> {
    idx: usize,
    account_id: &'a str,
    provider: &'a str,
    enabled: bool,
    quota_tokens: Option<u64>,
    stats: Option<&'a AccountWindowStats>,
    now: i64,
}

fn account_usage_row(r: UsageRow<'_>, u: &Ui) -> El {
    let UsageRow {
        idx,
        account_id,
        provider,
        enabled,
        quota_tokens,
        stats,
        now,
    } = r;
    let consumed = stats.map_or(0, |st| st.total_tokens);
    let requests = stats.map_or(0, |st| st.requests);
    let cooldown_until = stats.map_or(0, |st| st.cooldown_until);
    let in_cooldown = cooldown_until > now;

    let usage_label = match quota_tokens {
        Some(q) => {
            let pct = ((consumed as f64 / q as f64) * 100.0).round() as u64;
            format!(
                "{} / {} tok ({pct}%) · {} req",
                agentwork::compact_count(consumed),
                agentwork::compact_count(q),
                requests
            )
        }
        None => format!(
            "{} tok · {} req",
            agentwork::compact_count(consumed),
            requests
        ),
    };

    let mut right = row().gap(8.0).center_y();
    if let Some(q) = quota_tokens.filter(|&q| q > 0) {
        let frac = (consumed as f32 / q as f32).min(1.0);
        let bar_ink = if frac > 0.95 {
            u.danger
        } else if frac > 0.80 {
            u.warn
        } else {
            u.accent
        };
        right = right.child(
            row()
                .w(68.0)
                .h(4.0)
                .radius(2.0)
                .fill(u.layer3)
                .child(el().w(68.0 * frac).h(4.0).radius(2.0).fill(bar_ink)),
        );
    }
    right = right.child(text(usage_label).font(ui::LABEL).color(u.secondary));

    if in_cooldown {
        let rem_s = ((cooldown_until - now) / 1000).max(1);
        let cd_id = account_id.to_string();
        right = right
            .child(widgets::dot(u.warn))
            .child(
                text(format!("Cooldown ({rem_s}s)"))
                    .font(ui::LABEL)
                    .color(u.warn),
            )
            .child(button(
                format!("usage-clear-cd-{idx}"),
                "Limpar cooldown",
                Tone::Plain,
                true,
                u,
                move |s, _| clear_cooldown(s, cd_id.clone()),
            ));
    } else if !enabled {
        right = right
            .child(widgets::dot(u.idle))
            .child(text("Pausada").font(ui::LABEL).color(u.faint));
    } else {
        right = right
            .child(widgets::dot(u.ok))
            .child(text("Disponível").font(ui::LABEL).color(u.muted_text));
    }

    row()
        .key(format!("usage-acc-{idx}"))
        .w_full()
        .py(8.0)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(1.0)
                .child(
                    text(account_id.to_string())
                        .font(ui::BODY)
                        .color(u.content)
                        .truncate(),
                )
                .child(
                    text(provider.to_string())
                        .font(ui::LABEL)
                        .color(u.faint)
                        .truncate(),
                ),
        )
        .child(right)
}

fn usage_section(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let header = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(eyebrow("Quota & Uso (24h)", u).grow(1.0))
        .child(button(
            "usage-refresh",
            if p.usage.is_loading() {
                "Atualizando…"
            } else {
                "Atualizar"
            },
            Tone::Plain,
            !p.usage.is_loading(),
            u,
            |s, _| load_usage(s),
        ));

    let body = match &p.usage {
        Load::Idle | Load::Loading => widgets::loading("Carregando cotas e consumo", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler o uso: {e}"), u),
        Load::Ready(sum) => {
            let now = now_ms();
            let active_pair = s
                .chat
                .active_route
                .clone()
                .or_else(|| sum.active_account.clone().zip(sum.active_model.clone()));
            let active_row = match active_pair {
                Some((acc, model)) => row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(widgets::dot(u.ok))
                    .child(
                        text(format!("Ativo agora: conta {acc}"))
                            .font(ui::SMALL)
                            .color(u.content),
                    )
                    .child(Badge::new(model).variant(BadgeVariant::Subtle).build(&kit)),
                None => row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(widgets::dot(u.idle))
                    .child(
                        text("Ativo agora: nenhuma rota registrada nas últimas 24h")
                            .font(ui::SMALL)
                            .color(u.muted_text),
                    ),
            };

            let mut seen = HashSet::new();
            let mut rows = col().w_full().gap(6.0);
            let mut idx = 0usize;
            for acc in &p.accounts {
                seen.insert(acc.id.clone());
                let st = sum.accounts.iter().find(|x| x.account_id == acc.id);
                rows = rows.child(account_usage_row(
                    UsageRow {
                        idx,
                        account_id: &acc.id,
                        provider: &acc.provider,
                        enabled: acc.enabled,
                        quota_tokens: acc.quota_tokens,
                        stats: st,
                        now,
                    },
                    u,
                ));
                idx += 1;
            }
            for st in &sum.accounts {
                if !seen.insert(st.account_id.clone()) {
                    continue;
                }
                let prov = if st.provider.is_empty() {
                    "—"
                } else {
                    st.provider.as_str()
                };
                rows = rows.child(account_usage_row(
                    UsageRow {
                        idx,
                        account_id: &st.account_id,
                        provider: prov,
                        enabled: true,
                        quota_tokens: None,
                        stats: Some(st),
                        now,
                    },
                    u,
                ));
                idx += 1;
            }
            if idx == 0 {
                rows = rows.child(
                    text("Nenhum consumo ou conta registrada na janela de 24h.")
                        .font(ui::SMALL)
                        .color(u.muted_text),
                );
            }

            let consumed_total = sum
                .total_prompt_tokens
                .saturating_add(sum.total_completion_tokens);
            let saver_line = format!(
                "Consumo (24h): {} tok ({} req) · Token Saver economizou {} tok (RTK: {} · Ponytail: {} · Caveman: {})",
                agentwork::compact_count(consumed_total),
                sum.total_requests,
                agentwork::compact_count(sum.total_saved_tokens),
                agentwork::compact_count(sum.total_saved_rtk_tokens),
                agentwork::compact_count(sum.total_saved_ponytail_tokens),
                agentwork::compact_count(sum.total_saved_caveman_tokens),
            );

            col()
                .w_full()
                .gap(10.0)
                .child(active_row)
                .child(rows)
                .child(
                    text(saver_line)
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.muted_text)
                        .selectable(true),
                )
        }
    };

    col()
        .key("usage-section")
        .w_full()
        .gap(12.0)
        .child(header)
        .child(body)
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let body = col()
        .w_full()
        .gap(24.0)
        .pb(24.0)
        .child(providers_section(s, u))
        .child(rule(1.0, u.line).w_full())
        .child(accounts_section(s, u))
        .child(rule(1.0, u.line).w_full())
        .child(routing_section(s, u))
        .child(rule(1.0, u.line).w_full())
        .child(usage_section(s, u));
    widgets::page(
        "Provedores & Gateway Local",
        el(),
        body.scroll(ScrollSpec {
            stick_bottom: false,
        }),
        u,
    )
}
