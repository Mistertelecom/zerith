//! Appearance, privacy, storage, execution profile, about.

use rune::{col, row, text, El, ScrollSpec, Tx};
use rune_kit::{SegmentedControl, Switch};
use zerith_core::{CavemanLevel, PonytailLevel, TokenSaverSettings};
use zerith_sessions::{Store, UsageSummary};

use crate::agentwork;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, section, setting, Load, Tone};
use crate::{wire, Profile, Shell, Theme};

/// What the storage section reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Storage {
    pub db_bytes: u64,
    pub sessions: i64,
    pub messages: i64,
}

pub struct SettingsState {
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
                s.settings.notice = Some(Err(format!("Não consegui salvar o Token Saver: {e}")));
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
                "Nenhuma economia registrada ainda".to_string()
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
                    "RTK: {} tok · Ponytail: {} tok · Caveman: {} tok · Total: {} tok (-{}%)",
                    agentwork::compact_count(u.total_saved_rtk_tokens),
                    agentwork::compact_count(u.total_saved_ponytail_tokens),
                    agentwork::compact_count(u.total_saved_caveman_tokens),
                    agentwork::compact_count(u.total_saved_tokens),
                    pct
                )
            }
        }
    }
}

pub fn view(s: &Shell, u: &Ui, system_reduced: bool) -> El {
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
                "Sistema segue a aparência do sistema operacional.",
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

    let root = s.root();
    let privacy = section(
        "Privacidade",
        vec![
            setting(
                "Pausar memória",
                "Novos turnos deixam de consultar e gravar fatos.",
                Switch::new(s.memory.paused)
                    .label("Pausar memória")
                    .hide_label()
                    .on_toggle(|s: &mut Shell| {
                        crate::screens::memory::toggle_pause(s);
                    })
                    .build(&kit),
                u,
            ),
            setting(
                "Pasta de dados",
                &root.display().to_string(),
                button("open-data", "Abrir pasta", Tone::Plain, true, u, |s, _| {
                    let r = wire::reveal(&s.root());
                    s.settings.notice = Some(r.map(|_| "Pasta aberta".into()));
                }),
                u,
            ),
        ],
        u,
    );

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
            "Cópia consistente do banco em backups/, feita com o banco aberto.".to_string()
        }
        Load::Loading => "Copiando…".to_string(),
        Load::Ready(p) => format!("Backup salvo em {p}"),
        Load::Failed(e) => format!("Backup falhou: {e}"),
    };
    let storage = section(
        "Armazenamento",
        vec![
            setting(
                "Banco de dados",
                &s.db().display().to_string(),
                text(if counts.is_empty() {
                    size_line.clone()
                } else {
                    format!("{size_line} · {counts}")
                })
                .font(ui::LABEL)
                .color(u.muted_text),
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

    let prof_idx = match st.profile {
        Profile::Economy => 0,
        Profile::Balanced => 1,
        Profile::Performance => 2,
    };
    // The real numbers, so the choice says what it changes.
    let lim = wire::profile_limits(st.profile);
    let profile_detail = format!(
        "{} mensagens de histórico por turno, até {} subagentes em planos. Vale para a janela e o CLI.",
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
    let token_saver = section(
        "Token Saver (Economia de Tokens)",
        vec![
            setting(
                "Ponytail (Lazy Senior Dev)",
                "Instrui o agente a escrever o mínimo necessário com YAGNI e código nativo.",
                SegmentedControl::new(&["Off", "Lite", "Full", "Ultra"], Some(pt_idx))
                    .label("Ponytail (Lazy Senior Dev)")
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
                u,
            ),
            setting(
                "Caveman (Compressão de Resposta)",
                "Compacta explicações mantendo código, caminhos, comandos e alertas intactos.",
                SegmentedControl::new(&["Off", "Lite", "Full", "Ultra", "Wenyan"], Some(cv_idx))
                    .label("Caveman (Compressão de Resposta)")
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
                u,
            ),
            setting(
                "RTK (Compressão de Ferramentas)",
                "Comprime saídas grandes de git, build, grep e logs antes de enviar ao modelo.",
                Switch::new(st.token_saver.rtk_enabled)
                    .label("RTK (Compressão de Ferramentas)")
                    .hide_label()
                    .on_toggle(|s: &mut Shell| {
                        let next = !s.settings.token_saver.rtk_enabled;
                        set_rtk_enabled(s, next);
                    })
                    .build(&kit),
                u,
            ),
            setting(
                "Economia registrada (24h)",
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
            ),
        ],
        u,
    );

    let about = section(
        "Sobre",
        vec![
            setting("Versão", env!("CARGO_PKG_VERSION"), col(), u),
            setting("Licença", "GPL-3.0-only", col(), u),
            setting(
                "Créditos",
                "Rune (interface nativa), Agent Skills (formato de skills), Hermes (importação de sessões). Design inspirado no OpenHuman.",
                col(),
                u,
            ),
        ],
        u,
    );

    let mut body = col()
        .w_full()
        .max_w(720.0)
        .gap(28.0)
        .pb(24.0)
        .child(appearance)
        .child(privacy)
        .child(storage)
        .child(profile)
        .child(token_saver)
        .child(about);
    if let Some(Err(e)) = &st.notice {
        body = body.child(widgets::error_line(e.clone(), u));
    }
    widgets::page(
        "Configurações",
        row(),
        body.scroll(ScrollSpec {
            stick_bottom: false,
        }),
        u,
    )
}
