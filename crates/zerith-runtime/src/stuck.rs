//! Detector de travamento do laço do agente. Avisa o modelo; não trava o
//! agente. A única recusa é a repetição literal de uma chamada que já falhou
//! [`FAIL_BLOCK`] vezes idêntica, e mesmo ela devolve um erro explicando o
//! porquê (nenhuma ferramenta é bloqueada em geral).
//!
//! O que ele vê, por turno:
//! - mesma ação bem-sucedida repetida (`shell`, `agent`, edições);
//! - mesma chamada falhando de novo (aviso a partir de [`FAIL_WARN`]);
//! - ciclos de período 2 a 4 (A,B,A,B,A,B) sem edição de arquivo no meio;
//! - a mesma resposta de texto (ou quase) em passos seguidos.
//!
//! Editar arquivo com sucesso zera as falhas, os ciclos e as repetições de
//! `shell`: editar e rodar o teste de novo é o laço de correção, não travamento.
//!
//! Ideias (só a lógica; código próprio): `tool_guardrails` e
//! `repetition_guard` do Hermes Agent (MIT) e o `StuckDetector` do OpenHands
//! software-agent-sdk (MIT).

use std::collections::{HashMap, HashSet};

/// Falhas idênticas que disparam o aviso.
pub(crate) const FAIL_WARN: u8 = 2;
/// Falhas idênticas depois das quais a próxima chamada igual não executa.
pub(crate) const FAIL_BLOCK: u8 = 5;
/// Execuções idênticas bem-sucedidas que disparam o aviso.
const EXEC_WARN: u8 = 3;
/// Voltas completas de um ciclo que disparam o aviso.
const CYCLE_LAPS: usize = 3;
const MAX_PERIOD: usize = 4;
/// Chamadas guardadas para achar ciclos: folga sobre 3 voltas de período 4.
const HISTORY: usize = 64;
/// Texto mais curto que isso ("Vou ler o arquivo.") não conta como repetição.
const TEXT_MIN: usize = 30;
/// Parecença (Jaccard das palavras) a partir da qual dois textos são "o mesmo".
const TEXT_SIMILAR: f64 = 0.9;

/// Ferramenta + argumentos canônicos (JSON reserializado: espaço e ordem das
/// chaves não mudam a assinatura).
type Sig = (String, String);

fn sig(name: &str, args: &str) -> Sig {
    let canon = serde_json::from_str::<serde_json::Value>(args)
        .map_or_else(|_| args.trim().to_string(), |v| v.to_string());
    (name.to_string(), canon)
}

pub(crate) fn is_edit(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "multi_edit" | "apply_patch"
    )
}

/// Esperar e consultar em volta é legítimo: um ciclo só dessas não avisa.
fn is_poll(name: &str) -> bool {
    matches!(name, "sleep" | "shell_output" | "last_status")
}

#[derive(Default)]
pub(crate) struct Stuck {
    fails: HashMap<Sig, u8>,
    execs: HashMap<Sig, u8>,
    calls: Vec<Sig>,
    /// Chamadas vistas desde a última edição, e onde o último aviso de ciclo saiu.
    seen: usize,
    cycle_warned: usize,
    last_text: String,
    text_repeats: u8,
}

impl Stuck {
    /// Erro sintético no lugar da chamada, quando ela já falhou idêntica
    /// [`FAIL_BLOCK`] vezes sem edição entre elas.
    pub fn blocked(&self, name: &str, args: &str) -> Option<String> {
        let n = *self.fails.get(&sig(name, args))?;
        (n >= FAIL_BLOCK).then(|| {
            format!(
                "erro: [Loop Detectado] chamada não executada: `{name}` com estes mesmos argumentos já falhou {n}x neste turno sem nenhuma edição entre as tentativas. Mude de abordagem (outra ferramenta, outros argumentos, corrija a causa do erro) ou pergunte ao usuário com ask_user."
            )
        })
    }

    /// Registra o resultado de uma chamada e devolve os avisos para o modelo.
    pub fn observe(&mut self, name: &str, args: &str, ok: bool) -> Vec<String> {
        let s = sig(name, args);
        let mut out = Vec::new();
        if ok && is_edit(name) {
            self.fails.clear();
            self.execs.retain(|(n, _), _| n != "shell");
            self.calls.clear();
            self.seen = 0;
            self.cycle_warned = 0;
        }
        if ok {
            self.fails.remove(&s);
            if name == "shell" || name == "agent" || is_edit(name) {
                let n = self.execs.entry(s.clone()).or_insert(0);
                *n = n.saturating_add(1);
                if *n >= EXEC_WARN {
                    out.push(format!(
                        "[Loop Detectado] A ferramenta `{name}` foi executada {n}x com os mesmos argumentos neste turno sem nenhuma edição entre elas. NÃO repita a mesma ação: use o resultado anterior ou responda ao usuário."
                    ));
                }
            }
        } else {
            let n = self.fails.entry(s.clone()).or_insert(0);
            *n = n.saturating_add(1);
            // Passado do bloqueio, a recusa sintética já diz tudo.
            if (FAIL_WARN..=FAIL_BLOCK).contains(n) {
                let next = if *n == FAIL_BLOCK {
                    " Uma próxima chamada idêntica não será executada."
                } else {
                    ""
                };
                out.push(format!(
                    "[Loop Detectado] A chamada `{name}` com os mesmos argumentos falhou {n}x neste turno sem nenhuma edição entre elas. Não repita sem mudar algo: leia o erro, corrija a causa ou explique o bloqueio ao usuário. Se travou por ser app GUI ou servidor contínuo, use `run_in_background: true` uma única vez.{next}"
                ));
            }
        }
        if !(ok && is_edit(name)) {
            if self.calls.len() == HISTORY {
                self.calls.remove(0);
            }
            self.calls.push(s);
            self.seen += 1;
            if let Some((period, laps)) = cycle(&self.calls) {
                // Um aviso por volta, não um por chamada.
                if self.seen >= self.cycle_warned + period {
                    self.cycle_warned = self.seen;
                    out.push(format!(
                        "[Loop Detectado] As últimas {} chamadas repetem o mesmo ciclo de {period} ferramentas ({laps} voltas) sem nenhuma edição de arquivo no meio. Repetir o ciclo não é progresso: use os resultados que já tem, mude de abordagem ou pergunte ao usuário.",
                        period * laps
                    ));
                }
            }
        }
        out
    }

    /// Registra o texto de um passo; avisa quando repete o do passo anterior.
    pub fn observe_text(&mut self, text: &str) -> Option<String> {
        let norm = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if norm.chars().count() < TEXT_MIN {
            self.last_text.clear();
            self.text_repeats = 0;
            return None;
        }
        self.text_repeats = if similar(&norm, &self.last_text) {
            self.text_repeats.saturating_add(1)
        } else {
            0
        };
        self.last_text = norm;
        (self.text_repeats > 0).then(|| {
            format!(
                "[Loop Detectado] Sua resposta repetiu a do passo anterior ({}x seguidas). Não repita: avance a tarefa com outra ação ou, se está travado, explique ao usuário o que falta.",
                self.text_repeats + 1
            )
        })
    }
}

/// `(período, voltas)` do menor ciclo 2..=4 que fecha na última chamada.
/// Período 1 é a repetição simples, coberta pelos contadores.
fn cycle(calls: &[Sig]) -> Option<(usize, usize)> {
    let n = calls.len();
    for p in 2..=MAX_PERIOD {
        if n < p * CYCLE_LAPS {
            continue;
        }
        let tail = &calls[n - p..];
        if tail.iter().all(|s| *s == tail[0]) || tail.iter().all(|s| is_poll(&s.0)) {
            continue;
        }
        let laps = 1
            + (1..)
                .take_while(|&k| n >= p * (k + 1) && calls[n - p * (k + 1)..n - p * k] == *tail)
                .count();
        if laps >= CYCLE_LAPS {
            return Some((p, laps));
        }
    }
    None
}

fn similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if b.is_empty() {
        return false;
    }
    let wa: HashSet<&str> = a.split(' ').collect();
    let wb: HashSet<&str> = b.split(' ').collect();
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    inter / union >= TEXT_SIMILAR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_failure_warns_at_two_and_blocks_after_five() {
        let mut s = Stuck::default();
        let args = r#"{"command":"cargo test"}"#;
        assert!(s.observe("shell", args, false).is_empty());
        assert!(s.observe("shell", args, false)[0].contains("falhou 2x"));
        for _ in 3..=4 {
            assert!(s.blocked("shell", args).is_none());
            s.observe("shell", args, false);
        }
        let w = s.observe("shell", args, false);
        assert!(w[0].contains("não será executada"), "{w:?}");
        // Mesmos argumentos em outra ordem/espaçamento: a mesma chamada.
        let msg = s
            .blocked("shell", r#"{ "command" : "cargo test" }"#)
            .unwrap();
        assert!(
            msg.starts_with("erro:") && msg.contains("ask_user"),
            "{msg}"
        );
        // Outra chamada segue livre: só a repetição literal é recusada.
        assert!(s
            .blocked("shell", r#"{"command":"cargo test -p x"}"#)
            .is_none());
        // A recusa conta como falha, mas não repete o aviso.
        assert!(s.observe("shell", args, false).is_empty());
    }

    #[test]
    fn an_edit_resets_failures_and_shell_repeats() {
        let mut s = Stuck::default();
        let t = r#"{"command":"cargo test"}"#;
        for _ in 0..5 {
            s.observe("shell", t, false);
        }
        assert!(s.blocked("shell", t).is_some());
        assert!(s
            .observe("edit_file", r#"{"path":"a.rs"}"#, true)
            .is_empty());
        assert!(s.blocked("shell", t).is_none());
        assert!(s.observe("shell", t, false).is_empty());
        // Sucesso repetido: aviso na terceira, zerado por edição.
        let ok = r#"{"command":"ls"}"#;
        s.observe("shell", ok, true);
        assert!(s.observe("shell", ok, true).is_empty());
        assert!(s.observe("shell", ok, true)[0].contains("executada 3x"));
        s.observe("write_file", r#"{"path":"b.rs"}"#, true);
        assert!(s.observe("shell", ok, true).is_empty());
    }

    #[test]
    fn a_success_clears_that_call_failures() {
        let mut s = Stuck::default();
        let a = r#"{"path":"x"}"#;
        for _ in 0..5 {
            s.observe("read_file", a, false);
        }
        s.observe("read_file", a, true);
        assert!(s.blocked("read_file", a).is_none());
    }

    #[test]
    fn cycles_of_two_three_and_four_warn_once_per_lap() {
        for period in 2..=4usize {
            let mut s = Stuck::default();
            let names: Vec<String> = (0..period).map(|i| format!("t{i}")).collect();
            let mut warns = Vec::new();
            for lap in 0..4 {
                for n in &names {
                    for w in s.observe(n, "{}", true) {
                        warns.push((lap, w));
                    }
                }
            }
            // Avisa ao fechar a 3ª volta e de novo na 4ª.
            assert_eq!(warns.len(), 2, "período {period}: {warns:?}");
            assert_eq!(warns[0].0, 2);
            assert!(
                warns[0].1.contains(&format!("ciclo de {period}")),
                "{warns:?}"
            );
        }
    }

    #[test]
    fn a_cycle_broken_by_an_edit_or_made_of_polls_does_not_warn() {
        let mut s = Stuck::default();
        for _ in 0..2 {
            s.observe("read_file", r#"{"path":"a"}"#, true);
            s.observe("grep", r#"{"q":"x"}"#, true);
        }
        s.observe("edit_file", r#"{"path":"a"}"#, true);
        s.observe("read_file", r#"{"path":"a"}"#, true);
        assert!(s.observe("grep", r#"{"q":"x"}"#, true).is_empty());

        let mut p = Stuck::default();
        for _ in 0..5 {
            assert!(p.observe("sleep", r#"{"seconds":5}"#, true).is_empty());
            assert!(p.observe("shell_output", r#"{"id":"1"}"#, true).is_empty());
        }
    }

    #[test]
    fn repeated_text_warns_and_short_text_does_not() {
        let mut s = Stuck::default();
        let a = "Vou verificar o arquivo de configuração e rodar os testes de novo.";
        assert!(s.observe_text(a).is_none());
        let w = s.observe_text(&format!("  {a}\n")).unwrap();
        assert!(w.contains("2x"), "{w}");
        // Quase igual (uma palavra a mais em texto longo) também conta.
        let long = "agora eu vou abrir o arquivo principal do projeto ler a função de entrada conferir os testes e depois rodar a suíte inteira para ver o que quebrou";
        let mut t = Stuck::default();
        t.observe_text(long);
        assert!(t.observe_text(&format!("{long} hoje")).is_some());
        // Texto diferente ou curto zera.
        assert!(t
            .observe_text("Outra coisa totalmente diferente agora, nada a ver.")
            .is_none());
        let mut u = Stuck::default();
        u.observe_text("Vou ler.");
        assert!(u.observe_text("Vou ler.").is_none());
    }
}
