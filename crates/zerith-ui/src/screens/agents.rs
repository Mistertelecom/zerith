//! Agent definitions. Read-only for now.

use rune::{col, rule, text, El, ScrollSpec};

use crate::ui::{self, Ui};
use crate::widgets::{self, notice, Load};
use crate::wire::{self, Agent};
use crate::Shell;

#[derive(Default)]
pub struct AgentsState {
    pub list: Load<Vec<Agent>>,
}

pub fn load(s: &mut Shell) {
    s.agents.list = Load::Loading;
    s.work(wire::agents, |s, r| s.agents.list = r.into());
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let body = match &s.agents.list {
        Load::Idle | Load::Loading => widgets::loading("Lendo agentes", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler os agentes: {e}"), u),
        Load::Ready(v) if v.is_empty() => notice(
            "Nenhum agente definido",
            "Crie arquivos .zerith/agents/<nome>.md no projeto: o nome do arquivo vira o agente e o texto, as instruções dele.",
            u,
        ),
        Load::Ready(v) => {
            let mut c = col().w_full().role("list").label("Agentes");
            for (i, a) in v.iter().enumerate() {
                if i > 0 {
                    c = c.child(rule(1.0, u.line).w_full());
                }
                c = c.child(
                    col()
                        .key(format!("agent-{}", a.name))
                        .w_full()
                        .py(10.0)
                        .gap(2.0)
                        .child(text(a.name.clone()).font(ui::BODY).color(u.content))
                        .child(
                            text(if a.summary.is_empty() {
                                a.path.display().to_string()
                            } else {
                                a.summary.clone()
                            })
                            .wrap()
                            .font(ui::SMALL)
                            .color(u.muted_text)
                            .selectable(true),
                        ),
                );
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
