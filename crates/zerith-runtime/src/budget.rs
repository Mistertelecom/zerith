//! Orçamento visível ao modelo: ao cruzar 50%, 75% e 90% dos passos ou da
//! janela de contexto, uma linha curta no contexto do passo, uma vez por
//! limiar. Vai como nota sintética depois dos resultados, nunca no prompt de
//! sistema (que mudaria a cada passo e perderia o cache).
//!
//! Ideia do `<turn-budget>` do Goose (`agents/moim.rs`, Apache-2.0) e do
//! contexto restante que o Codex mostra ao modelo; código próprio.

const MARKS: [usize; 3] = [50, 75, 90];

#[derive(Default)]
pub(crate) struct Budget {
    /// Limiares já avisados (quantos de [`MARKS`]) de passos e de contexto.
    steps: usize,
    ctx: usize,
}

impl Budget {
    /// `step` passos feitos de `max_steps`; `used` tokens estimados de `window`.
    pub fn note(
        &mut self,
        step: usize,
        max_steps: usize,
        used: usize,
        window: usize,
    ) -> Option<String> {
        let sp = step * 100 / max_steps.max(1);
        let cp = used * 100 / window.max(1);
        let crossed = |pct: usize| MARKS.iter().filter(|&&m| pct >= m).count();
        let (s, c) = (crossed(sp), crossed(cp));
        if s <= self.steps && c <= self.ctx {
            return None;
        }
        self.steps = self.steps.max(s);
        self.ctx = self.ctx.max(c);
        Some(format!(
            "[orçamento: passo {step} de {max_steps} · contexto ~{cp}% usado]"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_per_threshold_of_steps_or_context() {
        let mut b = Budget::default();
        let lines: Vec<(usize, String)> = (1..=100)
            .filter_map(|s| b.note(s, 100, 1_000, 100_000).map(|l| (s, l)))
            .collect();
        let at: Vec<usize> = lines.iter().map(|(s, _)| *s).collect();
        assert_eq!(at, [50, 75, 90]);
        assert_eq!(
            lines[0].1,
            "[orçamento: passo 50 de 100 · contexto ~1% usado]"
        );

        // Contexto cruza sozinho; um salto de vários limiares dá uma linha só.
        let mut b = Budget::default();
        assert!(b.note(1, 100, 40_000, 100_000).is_none());
        let l = b.note(2, 100, 95_000, 100_000).unwrap();
        assert!(l.contains("~95%"), "{l}");
        assert!(b.note(3, 100, 96_000, 100_000).is_none());
        // Depois de compactar e voltar a subir, o limiar já avisado não repete.
        assert!(b.note(4, 100, 30_000, 100_000).is_none());
        assert!(b.note(5, 100, 80_000, 100_000).is_none());
    }
}
