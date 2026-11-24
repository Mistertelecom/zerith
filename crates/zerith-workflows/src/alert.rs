//! Aviso de falha de workflow agendado: quando as falhas seguidas chegam ao
//! limite do workflow (`alert_after_failures`, padrão 1) avisa uma vez nos
//! canais configurados; quando volta a funcionar avisa de novo, uma vez. O
//! estado ("avisado") fica em `workflow_meta`, então reiniciar o daemon não
//! repete aviso.
//!
//! Ideia (só conceito): incidentes do cron do Hermes (`cron/incidents.py`,
//! MIT), que alertam uma vez por incidente e fecham ao recuperar.

use crate::model::{ChannelConfig, Workflow};
use crate::scheduler::consecutive_failures;
use crate::store::Store;
use crate::{channels, Result};

/// Setting (em `workflow_meta`) com o id do canal usado quando o workflow não
/// lista `alert_channels`.
pub const DEFAULT_ALERT_CHANNEL_KEY: &str = "workflows.default_alert_channel";

/// Para onde vão os avisos. O padrão usa [`channels::dispatch`] e `stderr`;
/// os testes trocam por um canal falso.
pub trait AlertSink: Send + Sync {
    fn send(&self, channel: &ChannelConfig, target: Option<&str>, text: &str) -> Result<String> {
        channels::dispatch(channel, target, text)
    }
    fn log(&self, msg: &str) {
        eprintln!("zerith: {msg}");
    }
}

/// Envio real pelos canais do motor.
pub struct ChannelAlerts;
impl AlertSink for ChannelAlerts {}

/// Mudança de estado que gerou aviso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertChange {
    Failing(u32),
    Recovered,
}

fn state_key(workflow_id: &str) -> String {
    format!("alert_state:{workflow_id}")
}

/// Confere o histórico depois de uma execução e avisa só se o estado mudou.
pub fn check_after_run(
    store: &Store,
    wf: &Workflow,
    last_summary: &str,
    sink: &dyn AlertSink,
) -> Result<Option<AlertChange>> {
    let fails = consecutive_failures(store, &wf.id)?;
    let threshold = wf.alert.alert_after_failures.unwrap_or(1).max(1);
    let key = state_key(&wf.id);
    let alerted = store.setting(&key)?.is_some();
    let host = crate::hostname();
    let (change, text) = if !alerted && fails >= threshold {
        store.set_setting(&key, &fails.to_string())?;
        let mut err: String = last_summary.trim().chars().take(500).collect();
        if err.is_empty() {
            err = "(sem detalhes)".into();
        }
        (
            AlertChange::Failing(fails),
            format!(
                "[ALERTA] Workflow '{}' falhou {fails} vez(es) seguida(s) em {host}.\nÚltimo erro: {err}",
                wf.name
            ),
        )
    } else if alerted && fails == 0 {
        store.delete_setting(&key)?;
        (
            AlertChange::Recovered,
            format!("[OK] Workflow '{}' voltou a funcionar em {host}.", wf.name),
        )
    } else {
        return Ok(None);
    };
    deliver(store, wf, &text, sink)?;
    Ok(Some(change))
}

/// Envia para os canais do workflow, senão para o padrão do servidor, senão
/// só registra no log. Falha de envio vira log e não desfaz a mudança.
fn deliver(store: &Store, wf: &Workflow, text: &str, sink: &dyn AlertSink) -> Result<()> {
    let ids = if wf.alert.alert_channels.is_empty() {
        store
            .setting(DEFAULT_ALERT_CHANNEL_KEY)?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect()
    } else {
        wf.alert.alert_channels.clone()
    };
    if ids.is_empty() {
        sink.log(&format!("{text} (nenhum canal de aviso configurado)"));
        return Ok(());
    }
    for id in ids {
        let Some(ch) = store.get_channel(&id)? else {
            sink.log(&format!(
                "canal de aviso '{id}' não encontrado; aviso: {text}"
            ));
            continue;
        };
        let target = wf.alert.alert_target.get(&id).map(String::as_str);
        if let Err(e) = sink.send(&ch, target, text) {
            sink.log(&format!("falha ao avisar pelo canal '{}': {e}", ch.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChannelKind;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSink {
        sent: Mutex<Vec<(String, Option<String>, String)>>,
        logs: Mutex<Vec<String>>,
    }

    impl AlertSink for FakeSink {
        fn send(&self, ch: &ChannelConfig, target: Option<&str>, text: &str) -> Result<String> {
            self.sent.lock().unwrap().push((
                ch.id.clone(),
                target.map(str::to_string),
                text.to_string(),
            ));
            Ok("ok".into())
        }
        fn log(&self, msg: &str) {
            self.logs.lock().unwrap().push(msg.to_string());
        }
    }

    fn channel(id: &str, kind: ChannelKind) -> ChannelConfig {
        ChannelConfig {
            id: id.into(),
            name: id.into(),
            kind,
            enabled: true,
            config: serde_json::json!({}),
            updated_at_ms: 0,
        }
    }

    fn run(store: &Store, wf: &str, n: u32, status: &str) {
        let id = format!("{wf}-{n}");
        store.record_run_start(&id, wf, "cron").unwrap();
        store
            .record_run_finish(&id, wf, status, "deu ruim", &[])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }

    fn wf(id: &str) -> Workflow {
        Workflow {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            nodes: vec![],
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
            alert: Default::default(),
        }
    }

    #[test]
    fn alerts_once_per_state_change_on_every_listed_channel() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        store
            .save_channel(&channel("tg", ChannelKind::Telegram))
            .unwrap();
        store
            .save_channel(&channel("zap", ChannelKind::Waha))
            .unwrap();
        let mut w = wf("w");
        w.alert.alert_after_failures = Some(2);
        w.alert.alert_channels = vec!["tg".into(), "zap".into()];
        w.alert.alert_target.insert("zap".into(), "5511999".into());
        let sink = FakeSink::default();
        let check = |sink: &FakeSink| check_after_run(&store, &w, "deu ruim", sink).unwrap();

        run(&store, "w", 1, "error");
        assert_eq!(check(&sink), None, "abaixo do limite");
        run(&store, "w", 2, "error");
        assert_eq!(check(&sink), Some(AlertChange::Failing(2)));
        run(&store, "w", 3, "error");
        assert_eq!(check(&sink), None, "sem spam enquanto segue falhando");
        {
            let sent = sink.sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(sent[0].0, "tg");
            assert_eq!(sent[1].1.as_deref(), Some("5511999"));
            assert!(sent[0].2.contains("falhou 2 vez(es)") && sent[0].2.contains("deu ruim"));
        }

        run(&store, "w", 4, "ok");
        assert_eq!(check(&sink), Some(AlertChange::Recovered));
        run(&store, "w", 5, "ok");
        assert_eq!(check(&sink), None);
        let sent = sink.sent.lock().unwrap();
        assert_eq!(sent.len(), 4);
        assert!(sent[3].2.contains("voltou a funcionar"));
    }

    #[test]
    fn falls_back_to_default_channel_then_to_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let w = wf("w");
        let sink = FakeSink::default();

        run(&store, "w", 1, "error");
        assert_eq!(
            check_after_run(&store, &w, "x", &sink).unwrap(),
            Some(AlertChange::Failing(1))
        );
        assert!(sink.sent.lock().unwrap().is_empty());
        assert!(sink.logs.lock().unwrap()[0].contains("nenhum canal"));

        store
            .save_channel(&channel("padrao", ChannelKind::Telegram))
            .unwrap();
        store
            .set_setting(DEFAULT_ALERT_CHANNEL_KEY, "padrao")
            .unwrap();
        run(&store, "w", 2, "ok");
        assert_eq!(
            check_after_run(&store, &w, "", &sink).unwrap(),
            Some(AlertChange::Recovered)
        );
        assert_eq!(sink.sent.lock().unwrap()[0].0, "padrao");
    }
}
