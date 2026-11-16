//! Agent definitions from `zerith_core::agents`: the user's and the project's.

use rune::{col, el, row, rule, text, El, ScrollSpec};
use zerith_core::agents::AgentDef;

use crate::ui::{self, Ui};
use crate::widgets::{self, notice, pill, Load};
use crate::Shell;

#[derive(Default)]
pub struct AgentsState {
    pub list: Load<Vec<AgentDef>>,
}

pub fn load(s: &mut Shell) {
    s.agents.list = Load::Loading;
    let root = s.root();
    s.work(
        move || -> Result<Vec<AgentDef>, String> {
            let cwd = std::env::current_dir().ok();
            Ok(zerith_core::agents::list(
                &zerith_core::Paths::with_root(root),
                cwd.as_deref(),
            ))
        },
        |s, r| s.agents.list = r.into(),
    );
}

fn agent_row(a: &AgentDef, u: &Ui) -> El {
    let mut tags = row().gap(6.0).center_y();
    if let Some(m) = &a.model {
        tags = tags.child(pill(m.clone(), u));
    }
    tags = tags.child(pill(
        if a.layer == "project" {
            "projeto"
        } else {
            "usuário"
        },
        u,
    ));
    col()
        .key(format!("agent-{}", a.name))
        .w_full()
        .py(10.0)
        .gap(4.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
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
        .child(
            text(a.path.display().to_string())
                .font(ui::LABEL)
                .color(u.faint)
                .truncate()
                .selectable(true),
        )
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let user_dir = s.root().join("agents");
    let body = match &s.agents.list {
        Load::Idle | Load::Loading => widgets::loading("Lendo agentes", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler os agentes: {e}"), u),
        Load::Ready(v) if v.is_empty() => notice(
            "Nenhum agente definido",
            format!(
                "Crie <nome>.md em {} ou em .zerith/agents/ no projeto. O nome do arquivo vira o agente e o texto, as instruções dele. Use no chat pelo seletor ou num plano com @nome.",
                user_dir.display()
            ),
            u,
        ),
        Load::Ready(v) => {
            let mut c = col().w_full().role("list").label("Agentes");
            for (i, a) in v.iter().enumerate() {
                if i > 0 {
                    c = c.child(rule(1.0, u.line).w_full());
                }
                c = c.child(agent_row(a, u));
            }
            c.scroll(ScrollSpec {
                stick_bottom: false,
            })
        }
    };
    widgets::page(
        "Agentes",
        widgets::icon_button("agents-reload", "Recarregar", ui::icon_refresh(), u, |s| {
            s.load_agents()
        }),
        body,
        u,
    )
}
