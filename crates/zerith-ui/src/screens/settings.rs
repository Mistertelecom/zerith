//! Appearance, privacy, storage, execution profile, about.

use rune::{col, row, text, El, ScrollSpec, Tx};
use rune_kit::{SegmentedControl, Switch};
use zerith_sessions::Store;

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
    pub storage: Load<Storage>,
    pub backup: Load<String>,
    pub notice: Option<Result<String, String>>,
}

impl SettingsState {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            storage: Load::Idle,
            backup: Load::Idle,
            notice: None,
        }
    }
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
    let profile = section(
        "Perfil de execução",
        vec![setting(
            "Perfil",
            "Quanto histórico, subagentes e cache um turno pode usar. Salvo em settings.toml; o runtime ainda não aplica.",
            SegmentedControl::new(&["Economia", "Balanceado", "Desempenho"], Some(prof_idx))
                .label("Perfil de execução")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    s.settings.profile =
                        [Profile::Economy, Profile::Balanced, Profile::Performance][i];
                    s.save_prefs();
                })
                .build(&kit),
            u,
        )],
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
