//! Model endpoints: what is configured, editing, and a real connection test.

use rune::{col, el, row, text, El, ScrollSpec, TextEdit};
use zerith_providers::{Client, Probe, ProviderConfig, ProviderKind};

use crate::ui::{self, Ui};
use crate::widgets::{self, button, field, labelled, Load, Tone};
use crate::{wire, Shell};

pub struct Form {
    pub name: TextEdit,
    pub url: TextEdit,
    pub model: TextEdit,
    pub timeout: TextEdit,
    /// Masked, wiped on drop by Rune's secure editing. Never written to disk.
    pub key: TextEdit,
}

impl Form {
    fn of(c: &ProviderConfig) -> Self {
        Self {
            name: TextEdit::new(c.name.clone()),
            url: TextEdit::new(c.base_url.clone()),
            model: TextEdit::new(c.default_model.clone().unwrap_or_default()),
            timeout: TextEdit::new(c.timeout_ms.to_string()),
            key: TextEdit::new(c.api_key.clone().unwrap_or_default()).secure(),
        }
    }
}

pub struct ProvidersState {
    pub list: Vec<ProviderConfig>,
    pub selected: usize,
    pub form: Form,
    pub probe: Load<Probe>,
    pub notice: Option<Result<String, String>>,
}

impl ProvidersState {
    pub fn new(list: Vec<ProviderConfig>) -> Self {
        let list = if list.is_empty() {
            vec![ProviderConfig::default()]
        } else {
            list
        };
        let form = Form::of(&list[0]);
        Self {
            list,
            selected: 0,
            form,
            probe: Load::Idle,
            notice: None,
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

fn preset_9router(s: &mut Shell) {
    let f = &mut s.providers.form;
    f.name = TextEdit::new("9router");
    f.url = TextEdit::new(ProviderKind::NineRouter.default_base_url());
    s.providers.probe = Load::Idle;
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

pub fn view(s: &Shell, u: &Ui) -> El {
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
        text("O primeiro vem do ambiente (ZERITH_*); os outros, de settings.toml.")
            .wrap()
            .font(ui::LABEL)
            .color(u.faint)
            .mt(10.0),
    );

    let f = &p.form;
    let mut form = col()
        .key("provider-form")
        .grow(1.0)
        .min_w(0.0)
        .max_w(520.0)
        .gap(14.0)
        .child(labelled(
            "Nome",
            field(
                "prov-name",
                &f.name,
                "Nome",
                "9router",
                None,
                u,
                |s: &mut Shell| &mut s.providers.form.name,
                |i| i,
            ),
            u,
        ))
        .child(labelled(
            "URL",
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    field(
                        "prov-url",
                        &f.url,
                        "URL do endpoint",
                        "http://localhost:20128/v1",
                        None,
                        u,
                        |s: &mut Shell| &mut s.providers.form.url,
                        |i| i,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(button(
                    "preset-9router",
                    "9Router",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| preset_9router(s),
                )),
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
                            "ex.: gpt-4o-mini",
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
        .child(probe_line(&p.probe, u));
    match &p.notice {
        Some(Ok(m)) => {
            form = form.child(text(m.clone()).wrap().font(ui::LABEL).color(u.muted_text))
        }
        Some(Err(m)) => form = form.child(widgets::error_line(m.clone(), u)),
        None => {}
    }
    widgets::page(
        "Provedores",
        el(),
        row()
            .w_full()
            .gap(28.0)
            .child(list)
            .child(form)
            .scroll(ScrollSpec {
                stick_bottom: false,
            }),
        u,
    )
}

/// Used by tests: the 9Router preset fills the URL.
pub fn apply_preset(s: &mut Shell) {
    preset_9router(s);
}
