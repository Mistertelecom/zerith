//! What the Providers screen holds: the presets, the three forms and the
//! state that ties them to the settings layers. Pure data and validation, no
//! drawing and no I/O.

use rune::TextEdit;
use zerith_core::{AccountEntry, RoutingSettings};
use zerith_providers::{Probe, Protocol, ProviderConfig, ProviderKind};
use zerith_sessions::UsageSummary;

use crate::widgets::Load;

/// The four jobs of the screen, one tab each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Provider,
    Accounts,
    Routing,
    Usage,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Provider, Tab::Accounts, Tab::Routing, Tab::Usage];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Provider => "Provedor",
            Tab::Accounts => "Contas",
            Tab::Routing => "Delegação",
            Tab::Usage => "Uso",
        }
    }
}

pub(super) struct Preset {
    pub kind: ProviderKind,
    pub key: &'static str,
    pub label: &'static str,
}

pub(super) const PRESETS: &[Preset] = &[
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

pub(super) fn model_placeholder(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "ex.: claude-sonnet-4",
        ProviderKind::DeepSeek => "ex.: deepseek-chat",
        ProviderKind::OpenAi => "ex.: gpt-4o",
        ProviderKind::Gemini => "ex.: gemini-2.5-flash",
        ProviderKind::OpenRouter => "ex.: anthropic/claude-sonnet-4",
        ProviderKind::Ollama => "ex.: qwen2.5-coder",
        ProviderKind::LmStudio => "ex.: local-model",
        ProviderKind::Codex => "ex.: gpt-5-codex",
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
    pub(super) fn of(c: &ProviderConfig) -> Self {
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
    pub(super) fn of(r: &RoutingSettings) -> Self {
        Self {
            main_model: TextEdit::new(r.main_model.clone().unwrap_or_default()),
            subagent_model: TextEdit::new(r.subagent_model.clone().unwrap_or_default()),
        }
    }
}

pub struct ProvidersState {
    pub tab: Tab,
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
            tab: Tab::default(),
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
