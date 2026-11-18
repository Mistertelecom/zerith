//! The `Delegação` tab: which model leads and which one the subagents use.

use rune::{col, row, text, El};

use super::actions::save_routing;
use super::notice_line;
use crate::ui::{self, Ui};
use crate::widgets::{button, eyebrow, field, hint, labelled, Tone};
use crate::Shell;

pub fn view(s: &Shell, u: &Ui) -> El {
    let p = &s.providers;
    let rf = &p.routing_form;
    let card = col()
        .key("routing-section")
        .w_full()
        .max_w(600.0)
        .padding(18.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .gap(12.0)
        .child(eyebrow("Quem faz o quê", u))
        .child(
            text("O modelo principal conversa com você e planeja. Os subagentes recebem tarefas menores; um modelo mais barato neles reduz o custo sem mudar a conversa.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(
            hint(
                "Escreva provedor/modelo (anthropic/claude-opus-5-5) ou conta:modelo. Guardado em [routing].",
                u,
            ),
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
        .child(notice_line(&p.routing_notice, u));
    col().w_full().pb(16.0).child(card)
}
