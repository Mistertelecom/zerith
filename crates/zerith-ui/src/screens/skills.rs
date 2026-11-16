//! Installed skills (with their scan), the GitHub catalogue, and MCP plugins.
//! Nothing here runs a skill's code: listing, enabling and removing only.

use rune::{col, el, row, rule, text, El, LocalTime, ScrollSpec, TextEdit, Tx};
use rune_kit::{Badge, BadgeVariant, SegmentedControl, Switch};
use zerith_skills::catalog::{self, Catalogue};
use zerith_skills::{scan, Installed, Library, Severity, Skill, Status};

use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, notice, Load, Tone};
use crate::Shell;

/// One installed skill and what the scanner found in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub installed: Installed,
    /// `None` when the directory no longer scans (moved, corrupted).
    pub scan: Option<Skill>,
}

impl Entry {
    pub fn worst(&self) -> Option<Severity> {
        self.scan
            .as_ref()?
            .findings
            .iter()
            .map(|f| f.severity)
            .max()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Installed,
    Catalog,
    Plugins,
}

#[derive(Default)]
pub struct SkillsState {
    pub tab: Tab,
    pub installed: Load<Vec<Entry>>,
    pub selected: Option<String>,
    pub catalog: Load<Catalogue>,
    pub plugins: Load<Vec<zerith_plugins::Plugin>>,
    /// Local folder to install from.
    pub from: TextEdit,
    /// An install/remove/toggle in flight, by skill name (or the path).
    pub busy: Option<String>,
    pub notice: Option<Result<String, String>>,
}

fn library(s: &Shell) -> std::path::PathBuf {
    s.root().join("skills")
}

pub fn load(s: &mut Shell) {
    s.skills.installed = Load::Loading;
    let root = library(s);
    s.work(
        move || -> Result<Vec<Entry>, String> {
            let lib = Library::open(&root).map_err(|e| e.to_string())?;
            let m = lib.manifest().map_err(|e| e.to_string())?;
            Ok(m.skills
                .into_iter()
                .map(|i| Entry {
                    scan: scan(&root.join(&i.dir)).ok(),
                    installed: i,
                })
                .collect())
        },
        |s, r| {
            if let Ok(v) = &r {
                if s.skills.selected.is_none() {
                    s.skills.selected = v.first().map(|e| e.installed.name.clone());
                }
            }
            s.skills.installed = r.into();
        },
    );
}

/// Reads the catalogue: the cache when fresh, else GitHub (on a worker).
pub fn load_catalog(s: &mut Shell, force: bool) {
    s.skills.catalog = Load::Loading;
    let root = s.root();
    s.work(
        move || {
            let token = std::env::var("GITHUB_TOKEN").ok();
            catalog::load(&root, token.as_deref(), force).map_err(|e| e.to_string())
        },
        |s, r| s.skills.catalog = r.into(),
    );
}

pub fn load_plugins(s: &mut Shell) {
    s.skills.plugins = Load::Loading;
    let root = s.root().join("plugins");
    s.work(
        move || {
            zerith_plugins::Library::open(&root)
                .and_then(|l| l.plugins())
                .map_err(|e| e.to_string())
        },
        |s, r| s.skills.plugins = r.into(),
    );
}

pub fn set_tab(s: &mut Shell, t: Tab) {
    s.skills.tab = t;
    match t {
        Tab::Catalog if s.skills.catalog.is_idle() => load_catalog(s, false),
        Tab::Plugins if s.skills.plugins.is_idle() => load_plugins(s),
        _ => {}
    }
}

/// Runs one library mutation on a worker, then reloads the list.
fn mutate(
    s: &mut Shell,
    label: String,
    ok: String,
    job: impl FnOnce(&Library) -> Result<(), String> + Send + 'static,
) {
    if s.skills.busy.is_some() {
        return;
    }
    s.skills.busy = Some(label);
    s.skills.notice = None;
    let root = library(s);
    s.work(
        move || {
            Library::open(&root)
                .map_err(|e| e.to_string())
                .and_then(|l| job(&l))
        },
        move |s, r| {
            s.skills.busy = None;
            s.skills.notice = Some(r.map(|_| ok));
            load(s);
        },
    );
}

pub fn install_from(s: &mut Shell) {
    let from = s.skills.from.text.trim().to_string();
    if from.is_empty() {
        s.skills.notice = Some(Err("Informe a pasta da skill".into()));
        return;
    }
    let src = from.clone();
    mutate(s, from.clone(), format!("Instalada de {from}"), move |l| {
        l.install(std::path::Path::new(&src), &src)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    s.skills.from = TextEdit::new("");
}

fn trust(e: &Entry) -> (String, BadgeVariant) {
    match (e.scan.as_ref(), e.worst()) {
        (None, _) => ("não escaneada".into(), BadgeVariant::Neutral),
        (_, Some(Severity::High)) => ("risco alto".into(), BadgeVariant::Accent),
        (_, Some(Severity::Warn)) => ("atenção".into(), BadgeVariant::Neutral),
        _ => ("sem alertas".into(), BadgeVariant::Subtle),
    }
}

fn installed_list(s: &Shell, list: &[Entry], u: &Ui) -> El {
    let kit = ui::kit(u);
    let mut c = col()
        .key("skill-list")
        .role("list")
        .label("Skills instaladas")
        .w(260.0)
        .gap(2.0);
    for e in list {
        let name = e.installed.name.clone();
        let on = s.skills.selected.as_deref() == Some(name.as_str());
        // Notes are information, not alerts: only warn/high count.
        let n = e.scan.as_ref().map_or(0, |x| {
            x.findings
                .iter()
                .filter(|f| f.severity >= Severity::Warn)
                .count()
        });
        let enabled = e.installed.status == Status::Enabled;
        let toggle_name = name.clone();
        let busy = s.skills.busy.as_deref() == Some(name.as_str());
        c = c.child(
            row()
                .key(format!("sk-{name}"))
                .w_full()
                .h(44.0)
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(8.0)
                .fill(if on { u.muted } else { u.surface })
                .hover(u.hover)
                .role("button")
                .label(format!("{name}, {} alerta(s)", n))
                .checked(on)
                .focusable(true)
                .on_press(move |s: &mut Shell| s.skills.selected = Some(name.clone()))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(1.0)
                        .child(
                            text(e.installed.name.clone())
                                .font(ui::BODY)
                                .color(u.content)
                                .truncate(),
                        )
                        .child(
                            text(if n == 0 {
                                "sem alertas".to_string()
                            } else {
                                format!("{n} alerta(s)")
                            })
                            .font(ui::LABEL)
                            .color(u.faint),
                        ),
                )
                .child(
                    Switch::new(enabled)
                        .label(format!("Ativar {}", e.installed.name))
                        .hide_label()
                        .disabled(busy)
                        .on_toggle(move |s: &mut Shell| {
                            let name = toggle_name.clone();
                            let to = if enabled {
                                Status::Disabled
                            } else {
                                Status::Enabled
                            };
                            let msg = format!(
                                "{name}: {}",
                                if enabled { "desativada" } else { "ativada" }
                            );
                            mutate(s, name.clone(), msg, move |l| {
                                l.set_status(&name, to).map_err(|e| e.to_string())
                            });
                        })
                        .build(&kit),
                ),
        );
    }
    c
}

fn detail(s: &Shell, e: &Entry, u: &Ui) -> El {
    let kit = ui::kit(u);
    let i = &e.installed;
    let (word, variant) = trust(e);
    let busy = s.skills.busy.is_some();
    let name = i.name.clone();
    let mut c = col()
        .key("skill-detail")
        .grow(1.0)
        .min_w(0.0)
        .gap(14.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    text(i.name.clone())
                        .font(ui::TITLE)
                        .color(u.content)
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(Badge::new(word).variant(variant).build(&kit))
                .child(button(
                    format!("remove-{}", i.name),
                    if s.skills.busy.as_deref() == Some(i.name.as_str()) {
                        "Removendo…"
                    } else {
                        "Remover"
                    },
                    Tone::Danger,
                    !busy,
                    u,
                    move |s, _| {
                        let n = name.clone();
                        mutate(s, n.clone(), format!("{n} removida"), move |l| {
                            l.remove(&n).map_err(|e| e.to_string())
                        });
                        s.skills.selected = None;
                    },
                )),
        )
        .child(
            text(if i.description.is_empty() {
                "Sem descrição.".to_string()
            } else {
                i.description.clone()
            })
            .wrap()
            .font(ui::BODY)
            .color(u.secondary)
            .selectable(true),
        )
        .child(
            text(format!(
                "Origem: {} · Licença: {}",
                i.source,
                if i.license.is_empty() {
                    "—"
                } else {
                    &i.license
                }
            ))
            .wrap()
            .font(ui::LABEL)
            .color(u.faint)
            .selectable(true),
        );
    let Some(sc) = &e.scan else {
        return c.child(widgets::error_line(
            "A pasta desta skill não pôde ser escaneada.",
            u,
        ));
    };
    let mut files = col().w_full().gap(4.0).child(eyebrow("Arquivos", u));
    if sc.inventory.is_empty() && sc.scripts.is_empty() {
        files = files.child(text("Só SKILL.md.").font(ui::SMALL).color(u.muted_text));
    }
    for (ext, n) in &sc.inventory {
        files = files.child(
            text(format!("{n} × .{ext}"))
                .font(ui::SMALL)
                .color(u.secondary),
        );
    }
    for script in &sc.scripts {
        files = files.child(
            text(format!("{script}  (nunca executado)"))
                .font(ui::SMALL)
                .color(u.secondary),
        );
    }
    let mut risks = col().w_full().gap(6.0).child(eyebrow("Riscos", u));
    if sc.findings.is_empty() {
        risks = risks.child(
            text("O scanner não encontrou nada.")
                .font(ui::SMALL)
                .color(u.muted_text),
        );
    }
    for f in &sc.findings {
        let color = match f.severity {
            Severity::High => u.danger,
            Severity::Warn => u.warn,
            Severity::Note => u.faint,
        };
        risks = risks.child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(widgets::dot(color))
                .child(
                    text(f.severity.as_str())
                        .font(ui::LABEL)
                        .color(u.muted_text)
                        .w(36.0),
                )
                .child(
                    text(f.message.clone())
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.secondary)
                        .selectable(true)
                        .grow(1.0)
                        .min_w(0.0),
                ),
        );
    }
    c = c
        .child(rule(1.0, u.line).w_full())
        .child(files)
        .child(risks);
    c
}

fn installed_tab(s: &Shell, u: &Ui) -> El {
    let installer = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            field(
                "skill-from",
                &s.skills.from,
                "Pasta da skill para instalar",
                "Pasta local com SKILL.md",
                Some(ui::icon_folder()),
                u,
                |s: &mut Shell| &mut s.skills.from,
                |i| i.on_submit(|s: &mut Shell, _: &mut Tx| install_from(s)),
            )
            .grow(1.0)
            .min_w(0.0),
        )
        .child(button(
            "skill-install",
            if s.skills.busy.is_some() {
                "Instalando…"
            } else {
                "Instalar"
            },
            Tone::Plain,
            s.skills.busy.is_none(),
            u,
            |s, _| install_from(s),
        ));
    let body = match &s.skills.installed {
        Load::Idle | Load::Loading => widgets::loading("Lendo skills", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler as skills: {e}"), u),
        Load::Ready(list) if list.is_empty() => notice(
            "Nenhuma skill instalada",
            "Instale uma pasta com SKILL.md acima. Ela é escaneada antes de copiar e nada é executado.",
            u,
        ),
        Load::Ready(list) => {
            let sel = s
                .skills
                .selected
                .as_deref()
                .and_then(|n| list.iter().find(|e| e.installed.name == n));
            row()
                .w_full()
                .gap(20.0)
                .child(installed_list(s, list, u))
                .child(match sel {
                    Some(e) => detail(s, e, u),
                    None => notice("Escolha uma skill", "Os detalhes aparecem aqui.", u).grow(1.0),
                })
        }
    };
    col().w_full().gap(16.0).child(installer).child(body)
}

fn catalog_tab(s: &Shell, u: &Ui) -> El {
    match &s.skills.catalog {
        Load::Idle | Load::Loading => widgets::loading("Lendo catálogo", u),
        Load::Failed(e) => col()
            .w_full()
            .gap(12.0)
            .child(widgets::error_line(
                format!("Catálogo indisponível: {e}"),
                u,
            ))
            .child(button(
                "catalog-retry",
                "Tentar de novo",
                Tone::Plain,
                true,
                u,
                |s, _| load_catalog(s, false),
            )),
        Load::Ready(c) if c.entries.is_empty() => notice(
            "Catálogo vazio",
            "O GitHub não respondeu para nenhuma fonte. Tente atualizar mais tarde.",
            u,
        ),
        Load::Ready(c) => {
            let t = LocalTime::at(c.fetched_at);
            let mut list = col().w_full().child(
                row()
                    .w_full()
                    .pb(8.0)
                    .gap(8.0)
                    .center_y()
                    .child(
                        text(format!(
                            "Consultado em {:02}/{:02}/{} {:02}:{:02}. Estrelas medem popularidade, não segurança.",
                            t.day, t.month, t.year, t.hour, t.minute
                        ))
                        .wrap()
                        .font(ui::LABEL)
                        .color(u.faint)
                        .grow(1.0)
                        .min_w(0.0),
                    )
                    .child(button("catalog-refresh", "Atualizar", Tone::Plain, true, u, |s, _| {
                        load_catalog(s, true)
                    })),
            );
            for e in &c.entries {
                let url = e.url.clone();
                list = list.child(rule(1.0, u.line).w_full()).child(
                    row()
                        .key(format!("cat-{}", e.repo))
                        .w_full()
                        .py(10.0)
                        .gap(12.0)
                        .center_y()
                        .child(
                            col()
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(2.0)
                                .child(text(e.repo.clone()).font(ui::BODY).color(u.content))
                                .child(
                                    text(e.description.clone())
                                        .wrap()
                                        .max_lines(2)
                                        .font(ui::SMALL)
                                        .color(u.muted_text),
                                ),
                        )
                        .child(
                            text(format!("★ {}", e.stars))
                                .font(ui::LABEL)
                                .color(u.muted_text),
                        )
                        .child(button(
                            format!("cat-open-{}", e.repo),
                            "Abrir no GitHub",
                            Tone::Plain,
                            !url.is_empty(),
                            u,
                            move |s, tx| {
                                if let Err(e) = tx.open_url(&url) {
                                    s.skills.notice = Some(Err(e.to_string()));
                                }
                            },
                        )),
                );
            }
            list.child(
                text("Para instalar: clone o repositório e use \"Instalar\" com a pasta da skill.")
                    .font(ui::LABEL)
                    .color(u.faint)
                    .mt(10.0),
            )
        }
    }
}

fn plugins_tab(s: &Shell, u: &Ui) -> El {
    match &s.skills.plugins {
        Load::Idle | Load::Loading => widgets::loading("Lendo plugins", u),
        Load::Failed(e) => widgets::error_line(format!("Plugins indisponíveis: {e}"), u),
        Load::Ready(v) if v.is_empty() => notice(
            "Nenhum plugin MCP",
            "Instale com `zerith plugins install <pasta>`. Um plugin só roda depois de revisado.",
            u,
        ),
        Load::Ready(v) => {
            let kit = ui::kit(u);
            let mut c = col().w_full();
            for (i, p) in v.iter().enumerate() {
                if i > 0 {
                    c = c.child(rule(1.0, u.line).w_full());
                }
                c = c.child(
                    row()
                        .w_full()
                        .py(10.0)
                        .gap(10.0)
                        .center_y()
                        .child(
                            col()
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(2.0)
                                .child(
                                    text(format!("{} {}", p.manifest.name, p.manifest.version))
                                        .font(ui::BODY)
                                        .color(u.content),
                                )
                                .child(
                                    text(p.manifest.description.clone())
                                        .wrap()
                                        .font(ui::SMALL)
                                        .color(u.muted_text),
                                ),
                        )
                        .child(
                            Badge::new(if p.trusted {
                                "revisado"
                            } else {
                                "não revisado"
                            })
                            .variant(if p.trusted {
                                BadgeVariant::Subtle
                            } else {
                                BadgeVariant::Neutral
                            })
                            .build(&kit),
                        )
                        .child(Badge::new(p.status.as_str()).build(&kit)),
                );
            }
            c
        }
    }
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let idx = match s.skills.tab {
        Tab::Installed => 0,
        Tab::Catalog => 1,
        Tab::Plugins => 2,
    };
    let mut head = col().w_full().gap(10.0).child(
        row().child(
            SegmentedControl::new(&["Instaladas", "Catálogo", "Plugins MCP"], Some(idx))
                .label("Seção")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    set_tab(s, [Tab::Installed, Tab::Catalog, Tab::Plugins][i])
                })
                .build(&kit),
        ),
    );
    match &s.skills.notice {
        Some(Ok(m)) => head = head.child(text(m.clone()).font(ui::LABEL).color(u.muted_text)),
        Some(Err(m)) => head = head.child(widgets::error_line(m.clone(), u)),
        None => {}
    }
    let body = match s.skills.tab {
        Tab::Installed => installed_tab(s, u),
        Tab::Catalog => catalog_tab(s, u),
        Tab::Plugins => plugins_tab(s, u),
    };
    widgets::page(
        "Skills",
        el(),
        col().w_full().gap(16.0).child(head).child(
            body.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
        ),
        u,
    )
}
