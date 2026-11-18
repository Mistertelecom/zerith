//! Agent definitions from `zerith_core::agents`: the user's and the project's,
//! and a small form that writes a new one to the user folder.

use std::path::{Path, PathBuf};

use rune::{col, el, icon, row, text, text_input, El, TextEdit, Tx};
use zerith_core::agents::AgentDef;

use crate::ui::{self, Ui};
use crate::widgets::{self, button, code_line, field, hint, labelled, pill, Load, Tone};
use crate::{wire, Shell};

/// The "Novo agente" form. Every field is plain text; the file is built from
/// them only when the user presses `Criar agente`.
pub struct NewAgent {
    pub name: TextEdit,
    pub description: TextEdit,
    pub instructions: TextEdit,
    /// Comma-separated tool allowlist; empty inherits every tool.
    pub tools: TextEdit,
    pub model: TextEdit,
    pub error: Option<String>,
}

impl NewAgent {
    /// A filled-in form with a name clash error, for the `agent-new` scene.
    pub fn demo() -> Self {
        let mut f = Self::blank();
        f.name = TextEdit::new("revisor-rust");
        f.description = TextEdit::new("Revisa diffs Rust e aponta unwrap em caminhos de erro");
        f.instructions = TextEdit::new(
            "Você revisa diffs em Rust.
Aponte unwrap e erros ignorados, e sugira a correção mínima.",
        );
        f.tools = TextEdit::new("read_file, grep");
        f.error = Some(
            "Já existe um agente chamado “revisor-rust”. Escolha outro nome; nada foi sobrescrito."
                .into(),
        );
        f
    }

    fn blank() -> Self {
        Self {
            name: TextEdit::new(""),
            description: TextEdit::new(""),
            instructions: TextEdit::new(""),
            tools: TextEdit::new(""),
            model: TextEdit::new(""),
            error: None,
        }
    }
}

#[derive(Default)]
pub struct AgentsState {
    pub list: Load<Vec<AgentDef>>,
    /// The open form, if any.
    pub form: Option<NewAgent>,
    /// Outcome of the last creation: the file written, or why not.
    pub notice: Option<Result<String, String>>,
}

pub fn load(s: &mut Shell) {
    s.agents.list = Load::Loading;
    let root = s.root();
    s.work(
        move || -> Result<Vec<AgentDef>, String> {
            let cwd = std::env::current_dir().ok();
            let extra = zerith_plugins::Library::open(&root.join("plugins"))
                .and_then(|l| l.contributions())
                .map(|c| c.agents)
                .unwrap_or_default();
            let paths = zerith_core::Paths::with_root(root);
            let mut all = zerith_core::agents::list_with_extra(&paths, cwd.as_deref(), &extra);
            // The ones switched off (the model's, waiting for review) show up
            // here too, and only here: a turn never sees them.
            for d in zerith_core::agents::list_all(&paths, cwd.as_deref()) {
                if d.disabled && !all.iter().any(|a| a.name == d.name) {
                    all.push(d);
                }
            }
            all.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(all)
        },
        |s, r| s.agents.list = r.into(),
    );
}

// ------------------------------------------------------------- new agent

/// Why `name` cannot be an agent file name, or `None` when it can. The same
/// rule `zerith_core::agents` applies when it reads `name:` from a file, plus
/// `default`, which is reserved.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        Some("Dê um nome ao agente.")
    } else if n.len() > 64 {
        Some("O nome pode ter até 64 caracteres.")
    } else if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Some("Use só letras sem acento, números, “-” e “_” no nome.")
    } else if n == zerith_core::agents::DEFAULT {
        Some("“default” é reservado para o agente padrão.")
    } else {
        None
    }
}

/// The `.md` file for the form: frontmatter only for what was filled in.
pub fn render_file(f: &NewAgent) -> String {
    let mut head = String::new();
    let one_line = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let desc = one_line(&f.description.text);
    if !desc.is_empty() {
        head.push_str(&format!("description: \"{}\"\n", desc.replace('"', "'")));
    }
    let tools: Vec<String> = f
        .tools
        .text
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if !tools.is_empty() {
        head.push_str(&format!("tools: {}\n", tools.join(", ")));
    }
    let model = f.model.text.trim();
    if !model.is_empty() {
        head.push_str(&format!("model: {model}\n"));
    }
    let body = f.instructions.text.trim();
    if head.is_empty() {
        format!("{body}\n")
    } else {
        format!("---\n{head}---\n{body}\n")
    }
}

/// Writes `<dir>/<name>.md`, refusing to overwrite. `create_new` makes the
/// check and the write one step, so two clicks cannot race into a clobber.
pub fn write_agent(dir: &Path, f: &NewAgent) -> Result<PathBuf, String> {
    if let Some(why) = name_problem(&f.name.text) {
        return Err(why.into());
    }
    if f.instructions.text.trim().is_empty() {
        return Err("Escreva as instruções: é o que o agente segue.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Não consegui criar a pasta: {e}"))?;
    let path = dir.join(format!("{}.md", f.name.text.trim()));
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file
            .write_all(render_file(f).as_bytes())
            .map(|_| path)
            .map_err(|e| format!("Não consegui gravar o arquivo: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "Já existe um agente chamado “{}”. Escolha outro nome; nada foi sobrescrito.",
            f.name.text.trim()
        )),
        Err(e) => Err(format!("Não consegui criar o arquivo: {e}")),
    }
}

pub fn open_form(s: &mut Shell, tx: &mut Tx) {
    s.agents.form = Some(NewAgent::blank());
    s.agents.notice = None;
    tx.focus_when_shown("agent-name", true);
}

pub fn close_form(s: &mut Shell) {
    s.agents.form = None;
}

/// `Criar agente`: validates, writes on a worker, reloads the list.
pub fn create(s: &mut Shell) {
    let Some(f) = s.agents.form.take() else {
        return;
    };
    // Validate on the UI thread so a typo answers at once; only the write
    // goes to a worker.
    if let Some(why) = name_problem(&f.name.text) {
        s.agents.form = Some(NewAgent {
            error: Some(why.into()),
            ..f
        });
        return;
    }
    if f.instructions.text.trim().is_empty() {
        s.agents.form = Some(NewAgent {
            error: Some("Escreva as instruções: é o que o agente segue.".into()),
            ..f
        });
        return;
    }
    let dir = zerith_core::Paths::with_root(s.root()).agents();
    s.work(
        move || write_agent(&dir, &f).map_err(|e| Box::new((e, f))),
        |s, r| match r {
            Ok(path) => {
                s.agents.notice = Some(Ok(format!("Agente criado em {}", path.display())));
                s.load_agents();
            }
            Err(boxed) => {
                let (e, f) = *boxed;
                s.agents.form = Some(NewAgent {
                    error: Some(e),
                    ..f
                })
            }
        },
    );
}

pub fn open_folder(s: &mut Shell) {
    let dir = zerith_core::Paths::with_root(s.root()).agents();
    // The folder may not exist yet: opening it must still work.
    let _ = std::fs::create_dir_all(&dir);
    s.agents.notice = Some(wire::reveal(&dir).map(|_| "Pasta aberta".into()));
}

// ------------------------------------------------------------------ views

fn agent_row(a: &AgentDef, u: &Ui) -> El {
    let mut tags = row().gap(6.0).center_y();
    if let Some(m) = &a.model {
        tags = tags.child(pill(m.clone(), u));
    }
    if a.created_by_ai {
        tags = tags.child(pill("IA", u));
    }
    if a.disabled {
        tags = tags.child(pill("desativado", u));
    }
    tags = tags.child(pill(
        match a.layer {
            "project" => "projeto",
            "plugin" => "plugin",
            _ => "usuário",
        },
        u,
    ));
    let badge = el()
        .size(26.0, 26.0)
        .radius(ui::R_SM)
        .fill(u.layer2)
        .border(1.0, u.line)
        .center()
        .a11y_hidden()
        .child(icon(ui::icon_agents(), 14.0).color(if a.disabled {
            u.faint
        } else {
            u.accent_text
        }));
    col()
        .key(format!("agent-{}", a.name))
        .w_full()
        .padding(14.0)
        .gap(6.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(badge)
                .child(
                    text(a.name.clone())
                        .font(ui::BODY)
                        .color(u.content)
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(tags),
        )
        .child(if a.description.is_empty() {
            el()
        } else {
            text(a.description.clone())
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .selectable(true)
        })
        .child(widgets::code_line(a.path.display().to_string(), u))
        .child(agent_actions(a, u))
}

/// Review actions: only for the user's own files (a project or plugin agent is
/// not toggled from here). A disabled agent is meant to be read first, so the
/// button says what it does and the path above lets the person open the file.
fn agent_actions(a: &AgentDef, u: &Ui) -> El {
    if a.layer != "user" || !(a.disabled || a.created_by_ai) {
        return el();
    }
    let name = a.name.clone();
    let to = a.disabled;
    row()
        .gap(8.0)
        .center_y()
        .child(button(
            format!("agent-toggle-{}", a.name),
            if a.disabled { "Ativar" } else { "Desativar" },
            if a.disabled {
                Tone::Primary
            } else {
                Tone::Plain
            },
            true,
            u,
            move |s, _| toggle_agent(s, name.clone(), !to),
        ))
        .child(hint(
            if a.disabled {
                "Criado pela IA e desligado. Leia o arquivo antes de ativar."
            } else {
                "Criado pela IA e ativo."
            },
            u,
        ))
}

/// Writes the new `disabled:` value off the UI thread, then reloads the list.
fn toggle_agent(s: &mut Shell, name: String, disabled: bool) {
    let root = s.root();
    s.work(
        move || {
            zerith_core::agents_write::set_disabled(
                &zerith_core::Paths::with_root(root),
                &name,
                disabled,
            )
        },
        |s, r| {
            s.agents.notice = Some(match r {
                Ok(()) => Ok("Agente atualizado".into()),
                Err(e) => Err(e),
            });
            s.load_agents();
        },
    );
}

/// Where agents live and what a file looks like: the same two lines on the
/// empty screen and above the form.
fn format_help(dir: &Path, u: &Ui) -> El {
    col()
        .w_full()
        .gap(8.0)
        .child(
            text("Um agente é um arquivo .md: o nome do arquivo é o nome do agente e o texto é a instrução que ele segue.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(
            text("Linhas opcionais no começo, entre “---”, ajustam o agente: description, model e tools.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .child(code_line(dir.display().to_string(), u)),
                )
                .child(button(
                    "agents-open-folder",
                    "Abrir pasta",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| open_folder(s),
                )),
        )
        .child(hint(
            "Também vale .zerith/agents/ dentro de um projeto. Escolha o agente no seletor do chat.",
            u,
        ))
}

fn form_view(f: &NewAgent, u: &Ui) -> El {
    let instructions = text_input(&f.instructions, ui::BODY)
        .key("agent-instructions")
        .w_full()
        .min_h(110.0)
        .max_lines(12)
        .color(u.content)
        .label("Instruções do agente")
        .placeholder(
            "Você revisa diffs em Rust. Aponte unwrap em caminhos de erro…",
            u.faint,
        )
        .on_edit(|s: &mut Shell| &mut s.agents.form.as_mut().expect("form open").instructions);
    let mut c = col()
        .key("agent-form")
        .role("form")
        .label("Novo agente")
        .w_full()
        .max_w(820.0)
        .gap(12.0)
        .padding(18.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(text("Novo agente").font(ui::TITLE).color(u.content))
        .child(widgets::split(
            labelled(
                "Nome do arquivo",
                field(
                    "agent-name",
                    &f.name,
                    "Nome do agente",
                    "revisor-rust",
                    None,
                    u,
                    |s: &mut Shell| &mut s.agents.form.as_mut().expect("form open").name,
                    |i| i,
                ),
                u,
            )
            .grow(1.0)
            .min_w(0.0),
            labelled(
                "Modelo (opcional)",
                field(
                    "agent-model",
                    &f.model,
                    "Modelo preferido do agente",
                    "ex.: gpt-4o-mini",
                    None,
                    u,
                    |s: &mut Shell| &mut s.agents.form.as_mut().expect("form open").model,
                    |i| i,
                ),
                u,
            )
            .grow(1.0)
            .min_w(0.0),
            12.0,
            u,
        ))
        .child(labelled(
            "Descrição",
            field(
                "agent-description",
                &f.description,
                "Descrição do agente",
                "Para que serve, em uma frase",
                None,
                u,
                |s: &mut Shell| &mut s.agents.form.as_mut().expect("form open").description,
                |i| i,
            ),
            u,
        ))
        .child(labelled(
            "Instruções",
            col()
                .w_full()
                .px(12.0)
                .py(8.0)
                .radius(10.0)
                .fill(u.layer2)
                .border(
                    1.0,
                    if f.instructions.focused {
                        u.accent.alpha(0.75)
                    } else {
                        u.line_strong
                    },
                )
                .child(instructions),
            u,
        ))
        .child(labelled(
            "Ferramentas permitidas (opcional)",
            field(
                "agent-tools",
                &f.tools,
                "Ferramentas permitidas, separadas por vírgula",
                "read_file, grep (vazio: todas)",
                None,
                u,
                |s: &mut Shell| &mut s.agents.form.as_mut().expect("form open").tools,
                |i| i,
            ),
            u,
        ));
    if let Some(e) = &f.error {
        c = c.child(widgets::error_line(e.clone(), u));
    }
    c.child(
        row()
            .w_full()
            .gap(8.0)
            .child(button(
                "agent-create",
                "Criar agente",
                Tone::Primary,
                true,
                u,
                |s, _| create(s),
            ))
            .child(button(
                "agent-cancel",
                "Cancelar",
                Tone::Plain,
                true,
                u,
                |s, _| close_form(s),
            )),
    )
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let dir = zerith_core::Paths::with_root(s.root()).agents();
    let mut body = col().w_full().max_w(820.0).gap(16.0).pb(20.0);
    if let Some(f) = &s.agents.form {
        body = body.child(form_view(f, u));
    }
    match &s.agents.notice {
        Some(Ok(m)) => {
            body = body.child(text(m.clone()).font(ui::LABEL).color(u.muted_text));
        }
        Some(Err(m)) => body = body.child(widgets::error_line(m.clone(), u)),
        None => {}
    }
    body = body.child(match &s.agents.list {
        Load::Idle | Load::Loading => widgets::loading("Lendo agentes", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler os agentes: {e}"), u),
        Load::Ready(v) if v.is_empty() => col()
            .key("agents-empty")
            .w_full()
            .max_w(640.0)
            .padding(18.0)
            .radius(ui::R_MD)
            .fill(u.muted)
            .border(1.0, u.line)
            .gap(10.0)
            .child(
                text("Nenhum agente definido")
                    .font(ui::TITLE)
                    .color(u.content),
            )
            .child(format_help(&dir, u)),
        Load::Ready(v) => {
            let mut c = col()
                .w_full()
                .role("list")
                .label("Agentes")
                .gap(10.0)
                .child(format_help_compact(&dir, u));
            for a in v {
                c = c.child(agent_row(a, u));
            }
            c
        }
    });
    let actions = row()
        .gap(8.0)
        .center_y()
        .child(button(
            "agents-new",
            "Novo agente",
            Tone::Primary,
            s.agents.form.is_none(),
            u,
            open_form,
        ))
        .child(widgets::icon_button(
            "agents-reload",
            "Recarregar",
            ui::icon_refresh(),
            u,
            |s| s.load_agents(),
        ));
    widgets::page(
        "Agentes",
        actions,
        widgets::scroll_body("agents-scroll", body, u),
        u,
    )
}

/// One line above a non-empty list: where the files are.
fn format_help_compact(dir: &Path, u: &Ui) -> El {
    row()
        .w_full()
        .pb(10.0)
        .gap(8.0)
        .center_y()
        .child(
            col()
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .child(code_line(dir.display().to_string(), u)),
        )
        .child(button(
            "agents-open-folder",
            "Abrir pasta",
            Tone::Plain,
            true,
            u,
            |s, _| open_folder(s),
        ))
}
