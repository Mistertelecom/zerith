//! Settings in three tabs: `Geral` (appearance, privacy, profile, about),
//! `Economia de tokens` and `Armazenamento`. Titles say what a setting does;
//! the stored names appear only as small muted hints.

use rune::{col, el, text, El, Tx};
use rune_kit::{SegmentedControl, Switch};
use zerith_core::{CavemanLevel, PonytailLevel, TokenSaverSettings};
use zerith_sessions::{Store, UsageSummary};

use crate::agentwork;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, section, setting, setting_full, Load, Tone};
use crate::{wire, Profile, Shell, Theme};

/// What the storage section reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Storage {
    pub db_bytes: u64,
    pub sessions: i64,
    pub messages: i64,
}

/// The three tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Tokens,
    Storage,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::General, Tab::Tokens, Tab::Storage];

    pub fn label(self) -> &'static str {
        match self {
            Tab::General => "Geral",
            Tab::Tokens => "Economia de tokens",
            Tab::Storage => "Armazenamento",
        }
    }
}

pub fn set_tab(s: &mut Shell, t: Tab) {
    s.settings.tab = t;
}

pub struct SettingsState {
    pub tab: Tab,
    pub profile: Profile,
    pub token_saver: TokenSaverSettings,
    pub savings: Load<UsageSummary>,
    pub storage: Load<Storage>,
    pub backup: Load<String>,
    pub notice: Option<Result<String, String>>,
}

impl SettingsState {
    pub fn new(profile: Profile, token_saver: TokenSaverSettings) -> Self {
        Self {
            tab: Tab::default(),
            profile,
            token_saver,
            savings: Load::Idle,
            storage: Load::Idle,
            backup: Load::Idle,
            notice: None,
        }
    }
}

pub fn load_usage(s: &mut Shell) {
    s.settings.savings = Load::Loading;
    let db = s.db().to_path_buf();
    s.work(
        move || wire::usage_summary(&db, 24),
        |s, r| s.settings.savings = r.into(),
    );
}

fn save_token_saver(s: &mut Shell) {
    let root = s.root();
    let ts = s.settings.token_saver.clone();
    s.work(
        move || wire::save_token_saver(&root, &ts),
        |s, r| {
            if let Err(e) = r {
                s.settings.notice = Some(Err(format!(
                    "Não consegui salvar a economia de tokens: {e}"
                )));
            }
        },
    );
}

pub fn set_ponytail(s: &mut Shell, level: PonytailLevel) {
    s.settings.token_saver.ponytail = level;
    save_token_saver(s);
}

pub fn set_caveman(s: &mut Shell, level: CavemanLevel) {
    s.settings.token_saver.caveman = level;
    save_token_saver(s);
}

pub fn set_rtk_enabled(s: &mut Shell, enabled: bool) {
    s.settings.token_saver.rtk_enabled = enabled;
    save_token_saver(s);
}

pub fn load_storage(s: &mut Shell) {
    s.settings.storage = Load::Loading;
    let db = s.db().to_path_buf();
    s.work(
        move || -> Result<Storage, String> {
            let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
            let st = Store::open(&db).map_err(|e| e.to_string())?;
            let count = |sql: &str| -> Result<i64, String> {
                st.conn()
                    .query_row(sql, [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            };
            Ok(Storage {
                db_bytes,
                sessions: count("SELECT COUNT(*) FROM sessions")?,
                messages: count("SELECT COUNT(*) FROM messages")?,
            })
        },
        |s, r| s.settings.storage = r.into(),
    );
}

/// Copies the live database with SQLite's online backup, on a worker.
pub fn backup(s: &mut Shell) {
    if s.settings.backup.is_loading() {
        return;
    }
    s.settings.backup = Load::Loading;
    let db = s.db().to_path_buf();
    let dir = s.root().join("backups");
    s.work(
        move || -> Result<String, String> {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let to = dir.join(format!("zerith-{stamp}.db"));
            Store::open(&db)
                .and_then(|st| st.backup_to(&to))
                .map_err(|e| e.to_string())?;
            Ok(to.display().to_string())
        },
        |s, r| s.settings.backup = r.into(),
    );
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn savings_detail(savings: &Load<UsageSummary>) -> String {
    match savings {
        Load::Idle | Load::Loading => "calculando…".to_string(),
        Load::Failed(e) => format!("indisponível: {e}"),
        Load::Ready(u) => {
            if u.total_saved_tokens == 0 {
                "Nada poupado ainda: ligue uma das opções acima e converse.".to_string()
            } else {
                let consumed = u
                    .total_prompt_tokens
                    .saturating_add(u.total_completion_tokens);
                let baseline = consumed.saturating_add(u.total_saved_tokens);
                let pct = if baseline > 0 {
                    ((u.total_saved_tokens as f64 / baseline as f64) * 100.0).round() as u64
                } else {
                    0
                };
                format!(
                    "Poupou {} tokens (-{}%): {} em saídas de ferramentas, {} em código enxuto, {} em respostas curtas.",
                    agentwork::compact_count(u.total_saved_tokens),
                    pct,
                    agentwork::compact_count(u.total_saved_rtk_tokens),
                    agentwork::compact_count(u.total_saved_ponytail_tokens),
                    agentwork::compact_count(u.total_saved_caveman_tokens),
                )
            }
        }
    }
}

fn card(body: El, u: &Ui) -> El {
    col()
        .w_full()
        .padding(16.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(body)
}

fn general(s: &Shell, u: &Ui, system_reduced: bool) -> El {
    let kit = ui::kit(u);
    let st = &s.settings;
    let theme_idx = match s.theme {
        Theme::Dark => 0,
        Theme::Light => 1,
        Theme::System => 2,
    };
    let appearance = section(
        "Aparência",
        vec![
            setting(
                "Tema",
                "“Sistema” acompanha o modo claro ou escuro do computador.",
                SegmentedControl::new(&["Escuro", "Claro", "Sistema"], Some(theme_idx))
                    .label("Tema")
                    .fit()
                    .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                        s.set_theme([Theme::Dark, Theme::Light, Theme::System][i])
                    })
                    .build(&kit),
                u,
            ),
            setting(
                "Movimento reduzido",
                if system_reduced {
                    "O sistema já pede menos movimento; a janela respeita."
                } else {
                    "Troca animações por cortes e fades curtos."
                },
                Switch::new(s.reduce_motion || system_reduced)
                    .label("Movimento reduzido")
                    .hide_label()
                    .disabled(system_reduced)
                    .on_toggle(|s: &mut Shell| s.set_reduce_motion(!s.reduce_motion))
                    .build(&kit),
                u,
            ),
        ],
        u,
    );

    let privacy = section(
        "Privacidade",
        vec![setting(
            "Pausar memória",
            "Enquanto estiver ligado, as próximas conversas não leem nem gravam lembranças.",
            Switch::new(s.memory.paused)
                .label("Pausar memória")
                .hide_label()
                .on_toggle(|s: &mut Shell| {
                    crate::screens::memory::toggle_pause(s);
                })
                .build(&kit),
            u,
        )],
        u,
    );

    let prof_idx = match st.profile {
        Profile::Economy => 0,
        Profile::Balanced => 1,
        Profile::Performance => 2,
    };
    // The real numbers, so the choice says what it changes.
    let lim = wire::profile_limits(st.profile);
    let profile_detail = format!(
        "Quanto o agente lê e delega por turno: {} mensagens de histórico e até {} subagentes em planos. Vale para a janela e para o terminal.",
        lim.history_window, lim.max_subagents
    );
    let profile = section(
        "Perfil de execução",
        vec![setting(
            "Perfil",
            &profile_detail,
            SegmentedControl::new(&["Economia", "Balanceado", "Desempenho"], Some(prof_idx))
                .label("Perfil de execução")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    s.set_profile([Profile::Economy, Profile::Balanced, Profile::Performance][i]);
                })
                .build(&kit),
            u,
        )],
        u,
    );

    let about = section(
        "Sobre",
        vec![
            setting("Versão", env!("CARGO_PKG_VERSION"), el(), u),
            setting("Licença", "GPL-3.0-only", el(), u),
            setting(
                "Créditos",
                "Rune (interface nativa), Agent Skills (formato de skills), Hermes (importação de sessões). Design inspirado no OpenHuman.",
                el(),
                u,
            ),
        ],
        u,
    );
    col()
        .w_full()
        .max_w(640.0)
        .gap(16.0)
        .pb(24.0)
        .child(card(appearance, u))
        .child(card(privacy, u))
        .child(card(profile, u))
        .child(card(about, u))
}

fn tokens(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let st = &s.settings;
    let pt_idx = match st.token_saver.ponytail {
        PonytailLevel::Off => 0,
        PonytailLevel::Lite => 1,
        PonytailLevel::Full => 2,
        PonytailLevel::Ultra => 3,
    };
    let cv_idx = match st.token_saver.caveman {
        CavemanLevel::Off => 0,
        CavemanLevel::Lite => 1,
        CavemanLevel::Full => 2,
        CavemanLevel::Ultra => 3,
        CavemanLevel::Wenyan => 4,
    };
    let savings_line = savings_detail(&st.savings);
    let intro = text(
        "Três jeitos de gastar menos tokens, e portanto menos dinheiro ou cota. Todos são opcionais e valem para a próxima mensagem.",
    )
    .wrap()
    .font(ui::SMALL)
    .color(u.muted_text);
    let rows = section(
        "Como economizar",
        vec![
            setting_full(
                "Código enxuto",
                "Pede ao agente o mínimo que resolve: reaproveitar o que já existe, sem camadas a mais. Nível mais alto, diff menor.",
                "Guardado como token_saver.ponytail",
                SegmentedControl::new(&["Desligado", "Leve", "Médio", "Máximo"], Some(pt_idx))
                    .label("Código enxuto")
                    .fit()
                    .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                        let level = [
                            PonytailLevel::Off,
                            PonytailLevel::Lite,
                            PonytailLevel::Full,
                            PonytailLevel::Ultra,
                        ][i];
                        set_ponytail(s, level);
                    })
                    .build(&kit),
                true,
                u,
            ),
            setting_full(
                "Respostas curtas",
                "O agente explica em poucas palavras. Código, caminhos, comandos e avisos de segurança continuam completos. “Wenyan” escreve em chinês clássico, ainda mais curto e difícil de ler.",
                "Guardado como token_saver.caveman",
                SegmentedControl::new(
                    &["Desligado", "Leve", "Médio", "Máximo", "Wenyan"],
                    Some(cv_idx),
                )
                .label("Respostas curtas")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    let level = [
                        CavemanLevel::Off,
                        CavemanLevel::Lite,
                        CavemanLevel::Full,
                        CavemanLevel::Ultra,
                        CavemanLevel::Wenyan,
                    ][i];
                    set_caveman(s, level);
                })
                .build(&kit),
                true,
                u,
            ),
            setting_full(
                "Resumir saídas longas",
                "Comandos como git, build e grep podem imprimir milhares de linhas. Só o essencial vai ao modelo; o texto completo continua no terminal.",
                "Guardado como token_saver.rtk_enabled (RTK)",
                Switch::new(st.token_saver.rtk_enabled)
                    .label("Resumir saídas longas")
                    .hide_label()
                    .on_toggle(|s: &mut Shell| {
                        let next = !s.settings.token_saver.rtk_enabled;
                        set_rtk_enabled(s, next);
                    })
                    .build(&kit),
                false,
                u,
            ),
        ],
        u,
    );
    let result = section(
        "O que já foi poupado",
        vec![setting(
            "Últimas 24 horas",
            &savings_line,
            button(
                "settings-usage-refresh",
                if st.savings.is_loading() {
                    "Atualizando…"
                } else {
                    "Atualizar"
                },
                Tone::Plain,
                !st.savings.is_loading(),
                u,
                |s, _| load_usage(s),
            ),
            u,
        )],
        u,
    );
    col()
        .w_full()
        .max_w(640.0)
        .gap(16.0)
        .pb(24.0)
        .child(intro)
        .child(card(rows, u))
        .child(card(result, u))
}

fn storage(s: &Shell, u: &Ui) -> El {
    let st = &s.settings;
    let root = s.root();
    let (size_line, counts) = match &st.storage {
        Load::Idle | Load::Loading => ("calculando…".to_string(), String::new()),
        Load::Failed(e) => (format!("indisponível: {e}"), String::new()),
        Load::Ready(x) => (
            human(x.db_bytes),
            format!("{} conversas · {} mensagens", x.sessions, x.messages),
        ),
    };
    let backup_detail = match &st.backup {
        Load::Idle => {
            "Cópia consistente do banco na pasta backups/, feita com o app aberto.".to_string()
        }
        Load::Loading => "Copiando…".to_string(),
        Load::Ready(p) => format!("Backup salvo em {p}"),
        Load::Failed(e) => format!("Backup falhou: {e}"),
    };
    let rows = section(
        "Seus dados",
        vec![
            setting_full(
                "Pasta de dados",
                "Conversas, memória, skills e configurações ficam aqui, só no seu computador.",
                &root.display().to_string(),
                button("open-data", "Abrir pasta", Tone::Plain, true, u, |s, _| {
                    let r = wire::reveal(&s.root());
                    s.settings.notice = Some(r.map(|_| "Pasta aberta".into()));
                }),
                false,
                u,
            ),
            setting_full(
                "Banco de conversas",
                &if counts.is_empty() {
                    size_line.clone()
                } else {
                    format!("{size_line} · {counts}")
                },
                &s.db().display().to_string(),
                el(),
                false,
                u,
            ),
            setting(
                "Backup",
                &backup_detail,
                button(
                    "backup",
                    if st.backup.is_loading() {
                        "Copiando…"
                    } else {
                        "Fazer backup"
                    },
                    Tone::Plain,
                    !st.backup.is_loading(),
                    u,
                    |s, _| backup(s),
                ),
                u,
            ),
        ],
        u,
    );
    col()
        .w_full()
        .max_w(640.0)
        .gap(16.0)
        .pb(24.0)
        .child(card(rows, u))
}

pub fn view(s: &Shell, u: &Ui, system_reduced: bool) -> El {
    let idx = Tab::ALL
        .iter()
        .position(|t| *t == s.settings.tab)
        .unwrap_or(0);
    let labels: Vec<&str> = Tab::ALL.iter().map(|t| t.label()).collect();
    let mut body = match s.settings.tab {
        Tab::General => general(s, u, system_reduced),
        Tab::Tokens => tokens(s, u),
        Tab::Storage => storage(s, u),
    };
    if let Some(Err(e)) = &s.settings.notice {
        body = body.child(widgets::error_line(e.clone(), u));
    }
    widgets::page(
        "Configurações",
        el(),
        col()
            .w_full()
            .gap(16.0)
            .child(widgets::tabs(
                &labels,
                idx,
                "Seção das configurações",
                u,
                |s, i| set_tab(s, Tab::ALL[i]),
            ))
            .child(widgets::scroll_body("settings-scroll", body, u)),
        u,
    )
}
