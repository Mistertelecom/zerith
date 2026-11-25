use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::model::{
    ChannelConfig, ChannelKind, DaemonStatus, NodeKind, NodeRunLog, Workflow, WorkflowAlert,
    WorkflowEdge, WorkflowNode, WorkflowRun,
};
use crate::{now_ms, Result, WorkflowError};

const DAEMON_STATUS_KEY: &str = "daemon_status";

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let st = Self {
            conn: Mutex::new(conn),
        };
        st.init()?;
        Ok(st)
    }

    fn init(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                icon TEXT NOT NULL DEFAULT 'workflow',
                enabled INTEGER NOT NULL DEFAULT 0,
                nodes_json TEXT NOT NULL,
                edges_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_run_at_ms INTEGER,
                last_status TEXT,
                bot_name TEXT,
                alert_json TEXT,
                run_on TEXT
            );

            CREATE TABLE IF NOT EXISTS channels (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                config_json TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workflow_runs (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                trigger_source TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                summary TEXT NOT NULL DEFAULT '',
                node_logs_json TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS workflow_seen (
                scope TEXT NOT NULL,
                item_key TEXT NOT NULL,
                seen_at_ms INTEGER NOT NULL,
                PRIMARY KEY (scope, item_key)
            );

            CREATE TABLE IF NOT EXISTS workflow_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_runs_wf_started
                ON workflow_runs(workflow_id, started_at_ms DESC);
            "#,
        )?;
        // Idempotent migration for existing databases created prior to v0.3.3.
        let _ = conn.execute("ALTER TABLE workflows ADD COLUMN bot_name TEXT", []);
        let _ = conn.execute("ALTER TABLE workflows ADD COLUMN alert_json TEXT", []);
        let _ = conn.execute("ALTER TABLE workflows ADD COLUMN run_on TEXT", []);
        Ok(())
    }

    /// Seeds starter workflows once on a clean database so new users
    /// immediately see the OpenMausBot-style visual studio populated,
    /// without resurrecting templates after the user deletes them.
    pub fn seed_defaults(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let seeded: Option<String> = conn
            .query_row(
                "SELECT value FROM workflow_meta WHERE key = 'seeded_defaults'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if seeded.is_some() {
            return Ok(());
        }
        let existing: i64 = conn.query_row("SELECT COUNT(*) FROM workflows", [], |r| r.get(0))?;
        if existing == 0 {
            for wf in starter_templates() {
                upsert_workflow(&conn, &wf)?;
            }
        }
        conn.execute(
            "INSERT INTO workflow_meta (key, value) VALUES ('seeded_defaults', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        Ok(())
    }

    pub fn list_workflows(&self) -> Result<Vec<Workflow>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, icon, enabled, nodes_json, edges_json,
                    created_at_ms, updated_at_ms, last_run_at_ms, last_status, bot_name, alert_json, run_on
             FROM workflows
             ORDER BY updated_at_ms DESC, name ASC",
        )?;
        let rows = stmt.query_map([], row_to_workflow)?;
        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok(w) => out.push(w),
                // Corrupt graph JSON: hide the row (it cannot be saved over)
                // instead of failing every list.
                Err(rusqlite::Error::FromSqlConversionFailure(..)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<Workflow>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.query_row(
            "SELECT id, name, description, icon, enabled, nodes_json, edges_json,
                    created_at_ms, updated_at_ms, last_run_at_ms, last_status, bot_name, alert_json, run_on
             FROM workflows WHERE id = ?1",
            params![id],
            row_to_workflow,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn save_workflow(&self, wf: &Workflow) -> Result<Workflow> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        upsert_workflow(&conn, wf)?;
        drop(conn);
        Ok(self.get_workflow(&wf.id)?.unwrap_or_else(|| wf.clone()))
    }

    pub fn delete_workflow(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute(
            "DELETE FROM workflow_runs WHERE workflow_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM workflows WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_workflow_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute(
            "UPDATE workflows SET enabled = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![i32::from(enabled), now_ms(), id],
        )?;
        Ok(())
    }

    pub fn list_channels(&self) -> Result<Vec<ChannelConfig>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, enabled, config_json, updated_at_ms
             FROM channels
             ORDER BY updated_at_ms DESC, name ASC",
        )?;
        let rows = stmt.query_map([], row_to_channel)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_channel(&self, id: &str) -> Result<Option<ChannelConfig>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.query_row(
            "SELECT id, name, kind, enabled, config_json, updated_at_ms
             FROM channels WHERE id = ?1",
            params![id],
            row_to_channel,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn save_channel(&self, ch: &ChannelConfig) -> Result<ChannelConfig> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let kind_str = serde_json::to_value(ch.kind)?
            .as_str()
            .unwrap_or("generic_webhook")
            .to_string();
        let config_json = serde_json::to_string(&ch.config)?;
        let updated = if ch.updated_at_ms > 0 {
            ch.updated_at_ms
        } else {
            now_ms()
        };
        conn.execute(
            "INSERT INTO channels (id, name, kind, enabled, config_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                kind = excluded.kind,
                enabled = excluded.enabled,
                config_json = excluded.config_json,
                updated_at_ms = excluded.updated_at_ms",
            params![
                ch.id,
                ch.name,
                kind_str,
                i32::from(ch.enabled),
                config_json,
                updated
            ],
        )?;
        drop(conn);
        Ok(self.get_channel(&ch.id)?.unwrap_or_else(|| ch.clone()))
    }

    pub fn delete_channel(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute("DELETE FROM channels WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn record_run_start(
        &self,
        run_id: &str,
        workflow_id: &str,
        trigger_source: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let now = now_ms();
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_id, trigger_source, status, started_at_ms, summary, node_logs_json)
             VALUES (?1, ?2, ?3, 'running', ?4, '', '[]')",
            params![run_id, workflow_id, trigger_source, now],
        )?;
        conn.execute(
            "UPDATE workflows SET last_run_at_ms = ?1, last_status = 'running' WHERE id = ?2",
            params![now, workflow_id],
        )?;
        Ok(())
    }

    pub fn record_run_finish(
        &self,
        run_id: &str,
        workflow_id: &str,
        status: &str,
        summary: &str,
        node_logs: &[NodeRunLog],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let now = now_ms();
        let logs_json = serde_json::to_string(node_logs)?;
        conn.execute(
            "UPDATE workflow_runs
             SET status = ?1, finished_at_ms = ?2, summary = ?3, node_logs_json = ?4
             WHERE id = ?5",
            params![status, now, summary, logs_json, run_id],
        )?;
        conn.execute(
            "UPDATE workflows SET last_run_at_ms = ?1, last_status = ?2 WHERE id = ?3",
            params![now, status, workflow_id],
        )?;
        Ok(())
    }

    pub fn list_runs(&self, workflow_id: &str, limit: usize) -> Result<Vec<WorkflowRun>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let mut stmt = conn.prepare(
            "SELECT id, workflow_id, trigger_source, status, started_at_ms, finished_at_ms, summary, node_logs_json
             FROM workflow_runs
             WHERE workflow_id = ?1
             ORDER BY started_at_ms DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![workflow_id, limit as i64], row_to_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Valor de configuração do servidor de workflows (tabela `workflow_meta`),
    /// ex. `workflows.default_alert_channel`, `workflows.tz_offset_minutes`.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.query_row(
            "SELECT value FROM workflow_meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute(
            "INSERT INTO workflow_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        conn.execute("DELETE FROM workflow_meta WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Último sinal de vida gravado pelo `zerith daemon` (`None` se o daemon
    /// nunca rodou com este banco). API estável para a API HTTP do servidor.
    pub fn daemon_status(&self) -> Result<Option<DaemonStatus>> {
        Ok(self
            .setting(DAEMON_STATUS_KEY)?
            .and_then(|r| serde_json::from_str(&r).ok()))
    }

    /// Grava o sinal de vida do daemon (substitui o anterior).
    pub fn set_daemon_status(&self, status: &DaemonStatus) -> Result<()> {
        self.set_setting(DAEMON_STATUS_KEY, &serde_json::to_string(status)?)
    }

    /// Persistent deduplication (`Seen Store`), read half: items not yet seen
    /// under `scope` (also deduplicated within `items`). Records nothing; the
    /// runner calls [`Store::mark_items_seen`] once the run succeeds.
    pub fn unseen_items(&self, scope: &str, items: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let mut stmt =
            conn.prepare("SELECT 1 FROM workflow_seen WHERE scope = ?1 AND item_key = ?2")?;
        let mut local = std::collections::HashSet::new();
        let mut unseen = Vec::new();
        for raw in items {
            let trimmed = raw.trim();
            if trimmed.is_empty() || !local.insert(trimmed) {
                continue;
            }
            if !stmt.exists(params![scope, trimmed])? {
                unseen.push(trimmed.to_string());
            }
        }
        Ok(unseen)
    }

    /// Commit half of the Seen Store: records `items` under `scope`.
    pub fn mark_items_seen(&self, scope: &str, items: &[String]) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| WorkflowError::Lock)?;
        let now = now_ms();
        let mut inserted_any = false;
        for raw in items {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            inserted_any |= conn.execute(
                "INSERT OR IGNORE INTO workflow_seen (scope, item_key, seen_at_ms) VALUES (?1, ?2, ?3)",
                params![scope, trimmed, now],
            )? > 0;
        }
        if inserted_any {
            let _ = conn.execute(
                "DELETE FROM workflow_seen
                 WHERE scope = ?1 AND item_key NOT IN (
                     SELECT item_key FROM workflow_seen WHERE scope = ?1 ORDER BY seen_at_ms DESC LIMIT 2000
                 )",
                params![scope],
            );
        }
        Ok(())
    }
}

/// 128-bit hex token from std's OS-seeded SipHash keys (no extra dependency).
pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(SEQ.fetch_add(1, Ordering::Relaxed));
        h.write_i64(now_ms());
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

/// Webhook triggers always carry a secret: an empty one is replaced by a
/// random token at save time, so no webhook is reachable unauthenticated.
fn ensure_webhook_secrets(nodes: &mut [WorkflowNode]) {
    for n in nodes
        .iter_mut()
        .filter(|n| n.kind == NodeKind::TriggerWebhook)
    {
        let empty = n
            .config
            .get("secret")
            .and_then(|v| v.as_str())
            .is_none_or(|s| s.trim().is_empty());
        if !empty {
            continue;
        }
        if !n.config.is_object() {
            n.config = json!({});
        }
        if let Some(obj) = n.config.as_object_mut() {
            obj.insert("secret".into(), json!(random_token()));
        }
    }
}

fn upsert_workflow(conn: &Connection, wf: &Workflow) -> Result<()> {
    let mut nodes = wf.nodes.clone();
    ensure_webhook_secrets(&mut nodes);
    let nodes_json = serde_json::to_string(&nodes)?;
    let edges_json = serde_json::to_string(&wf.edges)?;
    let now = now_ms();
    let created = if wf.created_at_ms > 0 {
        wf.created_at_ms
    } else {
        now
    };
    let updated = if wf.updated_at_ms > 0 {
        wf.updated_at_ms
    } else {
        now
    };
    let alert_json = (wf.alert != WorkflowAlert::default())
        .then(|| serde_json::to_string(&wf.alert))
        .transpose()?;
    let bot_name = wf
        .bot_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    conn.execute(
        "INSERT INTO workflows (
            id, name, description, icon, enabled, nodes_json, edges_json,
            created_at_ms, updated_at_ms, last_run_at_ms, last_status, bot_name, alert_json, run_on
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            icon = excluded.icon,
            enabled = excluded.enabled,
            nodes_json = excluded.nodes_json,
            edges_json = excluded.edges_json,
            updated_at_ms = excluded.updated_at_ms,
            bot_name = excluded.bot_name,
            alert_json = excluded.alert_json,
            run_on = excluded.run_on",
        params![
            wf.id,
            wf.name,
            wf.description,
            wf.icon,
            i32::from(wf.enabled),
            nodes_json,
            edges_json,
            created,
            updated,
            wf.last_run_at_ms,
            wf.last_status,
            bot_name,
            alert_json,
            wf.run_on.as_deref().filter(|s| !s.is_empty())
        ],
    )?;
    Ok(())
}

fn row_to_workflow(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workflow> {
    let nodes_raw: String = row.get(5)?;
    let edges_raw: String = row.get(6)?;
    // A corrupt row errors instead of loading as empty: an empty graph would be
    // written back over the real data on the next save.
    let bad = |col: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e))
    };
    let nodes: Vec<WorkflowNode> = serde_json::from_str(&nodes_raw).map_err(|e| bad(5, e))?;
    let edges: Vec<WorkflowEdge> = serde_json::from_str(&edges_raw).map_err(|e| bad(6, e))?;
    let enabled_i: i32 = row.get(4)?;
    Ok(Workflow {
        // Config de aviso ilegível não esconde o workflow: volta ao padrão.
        alert: row
            .get::<_, Option<String>>(12)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        icon: row.get(3)?,
        enabled: enabled_i != 0,
        nodes,
        edges,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
        last_run_at_ms: row.get(9)?,
        last_status: row.get(10)?,
        bot_name: row.get(11)?,
        run_on: row.get(13)?,
    })
}

fn row_to_channel(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelConfig> {
    let kind_raw: String = row.get(2)?;
    let kind: ChannelKind = serde_json::from_value(serde_json::Value::String(kind_raw))
        .unwrap_or(ChannelKind::GenericWebhook);
    let enabled_i: i32 = row.get(3)?;
    let cfg_raw: String = row.get(4)?;
    let config = serde_json::from_str(&cfg_raw).unwrap_or_else(|_| json!({}));
    Ok(ChannelConfig {
        id: row.get(0)?,
        name: row.get(1)?,
        kind,
        enabled: enabled_i != 0,
        config,
        updated_at_ms: row.get(5)?,
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRun> {
    let logs_raw: String = row.get(7)?;
    let node_logs: Vec<NodeRunLog> = serde_json::from_str(&logs_raw).unwrap_or_default();
    Ok(WorkflowRun {
        id: row.get(0)?,
        workflow_id: row.get(1)?,
        trigger_source: row.get(2)?,
        status: row.get(3)?,
        started_at_ms: row.get(4)?,
        finished_at_ms: row.get(5)?,
        summary: row.get(6)?,
        node_logs,
    })
}

pub fn starter_templates() -> Vec<Workflow> {
    let now = now_ms();
    let mut base = default_templates(now);
    base.push(Workflow {
        alert: Default::default(),
        run_on: None,
        id: "linux-proxmox-health".into(),
        name: "Server / Proxmox SSH Health".into(),
        description: "Runs SSH health checks (uptime, disk, RAM) on a Linux or Proxmox host and notifies you if usage is critical.".into(),
        icon: "terminal".into(),
        enabled: false,
        nodes: vec![
            WorkflowNode {
                id: "p1".into(),
                kind: NodeKind::TriggerCron,
                label: "Every hour".into(),
                x: 40.0,
                y: 120.0,
                config: json!({ "cron": "0 * * * *", "tz_offset_minutes": -180 }),
            },
            WorkflowNode {
                id: "p2".into(),
                kind: NodeKind::SshCommand,
                label: "SSH Health Probe".into(),
                x: 310.0,
                y: 120.0,
                config: json!({
                    "host": "192.168.1.10",
                    "user": "root",
                    "port": 22,
                    "command": "hostname && df -h / | tail -1 && free -m | head -2",
                    "timeout_secs": 20
                }),
            },
            WorkflowNode {
                id: "p3".into(),
                kind: NodeKind::AiAgent,
                label: "Audit Metrics".into(),
                x: 580.0,
                y: 120.0,
                config: json!({
                    "model": "",
                    "system": "Check disk >= 85% or memory critical. Reply concisely starting with ALERT: if critical, else OK.",
                    "prompt": "{{prev}}"
                }),
            },
            WorkflowNode {
                id: "p4".into(),
                kind: NodeKind::Condition,
                label: "Critical alert?".into(),
                x: 850.0,
                y: 120.0,
                config: json!({ "mode": "contains", "pattern": "ALERT:" }),
            },
            WorkflowNode {
                id: "p5".into(),
                kind: NodeKind::NotifyChannel,
                label: "Alert Ops Channel".into(),
                x: 1120.0,
                y: 120.0,
                config: json!({
                    "channel_id": "",
                    "target": "",
                    "message": "🚨 *Server Alert ({{date}} {{time}})*\n{{prev}}"
                }),
            },
        ],
        edges: vec![
            WorkflowEdge { id: "pe1".into(), from: "p1".into(), to: "p2".into(), branch: None },
            WorkflowEdge { id: "pe2".into(), from: "p2".into(), to: "p3".into(), branch: None },
            WorkflowEdge { id: "pe3".into(), from: "p3".into(), to: "p4".into(), branch: None },
            WorkflowEdge { id: "pe4".into(), from: "p4".into(), to: "p5".into(), branch: Some("true".into()) },
        ],
        created_at_ms: now,
        updated_at_ms: now - 3000,
        last_run_at_ms: None,
        last_status: None,
        bot_name: Some("noc-engineer".into()),
    });
    base.push(Workflow {
        alert: Default::default(),
        run_on: None,
        id: "api-ai-executive-brief".into(),
        name: "API Data → Transform → AI Brief".into(),
        description: "Fetches JSON from any external API or ERP, extracts the key field without spending AI tokens, summarizes via AI, and notifies your channel.".into(),
        icon: "bot".into(),
        enabled: false,
        nodes: vec![
            WorkflowNode {
                id: "a1".into(),
                kind: NodeKind::TriggerCron,
                label: "Weekdays 08:30".into(),
                x: 40.0,
                y: 120.0,
                config: json!({ "cron": "30 8 * * 1-5", "tz_offset_minutes": -180 }),
            },
            WorkflowNode {
                id: "a2".into(),
                kind: NodeKind::HttpRequest,
                label: "Fetch JSON API".into(),
                x: 310.0,
                y: 120.0,
                config: json!({
                    "method": "GET",
                    "url": "https://api.github.com/zen",
                    "headers": {},
                    "body": "",
                    "allow_error_status": false
                }),
            },
            WorkflowNode {
                id: "a3".into(),
                kind: NodeKind::Transform,
                label: "Format Payload".into(),
                x: 580.0,
                y: 120.0,
                config: json!({
                    "mode": "template",
                    "pattern": "",
                    "template": "Fetched on {{date}} at {{time}}:\n{{prev}}"
                }),
            },
            WorkflowNode {
                id: "a4".into(),
                kind: NodeKind::AiAgent,
                label: "Executive Summary".into(),
                x: 850.0,
                y: 120.0,
                config: json!({
                    "model": "",
                    "system": "Turn raw data into 3 clear executive bullets.",
                    "prompt": "{{prev}}"
                }),
            },
            WorkflowNode {
                id: "a5".into(),
                kind: NodeKind::NotifyChannel,
                label: "Deliver Brief".into(),
                x: 1120.0,
                y: 120.0,
                config: json!({
                    "channel_id": "",
                    "target": "",
                    "message": "{{prev}}"
                }),
            },
        ],
        edges: vec![
            WorkflowEdge { id: "ae1".into(), from: "a1".into(), to: "a2".into(), branch: None },
            WorkflowEdge { id: "ae2".into(), from: "a2".into(), to: "a3".into(), branch: None },
            WorkflowEdge { id: "ae3".into(), from: "a3".into(), to: "a4".into(), branch: None },
            WorkflowEdge { id: "ae4".into(), from: "a4".into(), to: "a5".into(), branch: None },
        ],
        created_at_ms: now,
        updated_at_ms: now - 4000,
        last_run_at_ms: None,
        last_status: None,
        bot_name: Some("workflow-architect".into()),
    });
    base.push(offline_template(
        now,
        "dude-equipamentos-offline",
        "Equipamentos offline (The Dude)",
        "Lista os equipamentos fora do ar no The Dude (RouterOS com o pacote dude) e avisa um grupo. \
         Preencha: no nó SSH, `device` (cliente/nome do inventário) ou host/usuário/porta e \
         `password_secret` (nome do segredo com a senha; grave no servidor) — marque algoritmos \
         antigos se o RouterOS for velho; no nó de aviso, o canal e o grupo (…@g.us). Horário no \
         gatilho (padrão 07:50). Se o seu RouterOS não mostrar `status` no print detail, troque \
         `status_field`/`down_values` no nó Transformar. A condição \"Há offline?\" pode ser \
         removida para receber também o \"Nenhum equipamento offline\".",
        WorkflowNode {
            id: "o2".into(),
            kind: NodeKind::SshCommand,
            label: "Dude: listar dispositivos".into(),
            x: 310.0,
            y: 120.0,
            config: json!({
                "device": "",
                "host": "",
                "user": "",
                "port": 22,
                "password_secret": "",
                "legacy_ssh": false,
                "command": "/dude device print detail without-paging",
                "timeout_secs": 60
            }),
        },
    ));
    base
}

/// Molde dos modelos "Equipamentos offline": cron → consulta (`query`, id
/// `o2`) → transform `offline_report` → "há offline?" → aviso no canal, com
/// alerta de falha ligado (canal padrão do servidor).
fn offline_template(
    now: i64,
    id: &str,
    name: &str,
    description: &str,
    query: WorkflowNode,
) -> Workflow {
    let node = |id: &str, kind, label: &str, x: f64, config| WorkflowNode {
        id: id.into(),
        kind,
        label: label.into(),
        x,
        y: 120.0,
        config,
    };
    let edge = |id: &str, from: &str, to: &str, branch: Option<&str>| WorkflowEdge {
        id: id.into(),
        from: from.into(),
        to: to.into(),
        branch: branch.map(str::to_string),
    };
    Workflow {
        alert: WorkflowAlert {
            alert_after_failures: Some(1),
            ..Default::default()
        },
        run_on: None,
        id: id.into(),
        name: name.into(),
        description: description.into(),
        icon: "server".into(),
        enabled: false,
        nodes: vec![
            node(
                "o1",
                NodeKind::TriggerCron,
                "Todo dia 07:50",
                40.0,
                json!({ "cron": "50 7 * * *" }),
            ),
            query,
            node(
                "o3",
                NodeKind::Transform,
                "Montar lista de offline",
                580.0,
                json!({
                    "mode": "offline_report",
                    "status_field": "status",
                    "down_values": "down",
                    "title": "Equipamentos offline",
                    "empty_text": "Nenhum equipamento offline"
                }),
            ),
            node(
                "o4",
                NodeKind::Condition,
                "Há offline?",
                850.0,
                json!({ "mode": "not_contains", "pattern": "Nenhum equipamento offline" }),
            ),
            node(
                "o5",
                NodeKind::NotifyChannel,
                "Avisar o grupo",
                1120.0,
                json!({ "channel_id": "", "target": "", "message": "{{prev}}" }),
            ),
        ],
        edges: vec![
            edge("oe1", "o1", "o2", None),
            edge("oe2", "o2", "o3", None),
            edge("oe3", "o3", "o4", None),
            edge("oe4", "o4", "o5", Some("true")),
        ],
        created_at_ms: now,
        updated_at_ms: now - 5000,
        last_run_at_ms: None,
        last_status: None,
        bot_name: None,
    }
}

fn default_templates(now: i64) -> Vec<Workflow> {
    vec![
        Workflow {
            alert: Default::default(),
            run_on: None,
            id: "noc-daily-report".into(),
            name: "Daily NOC Offline Report".into(),
            description: "Checks hosts/equipment every day at 08:00, summarizes incidents with AI, and alerts your WhatsApp if anything is offline.".into(),
            icon: "activity".into(),
            enabled: false,
            nodes: vec![
                WorkflowNode {
                    id: "n1".into(),
                    kind: NodeKind::TriggerCron,
                    label: "Every day at 08:00".into(),
                    x: 40.0,
                    y: 120.0,
                    config: json!({ "cron": "0 8 * * *", "tz_offset_minutes": -180 }),
                },
                WorkflowNode {
                    id: "n2".into(),
                    kind: NodeKind::Shell,
                    label: "Collect host status".into(),
                    x: 310.0,
                    y: 120.0,
                    config: json!({
                        "command": "echo \"Checked at {{date}} {{time}} — replace with ping/snmp/zabbix/mikrotik script\"",
                        "timeout_secs": 30
                    }),
                },
                WorkflowNode {
                    id: "n3".into(),
                    kind: NodeKind::AiAgent,
                    label: "Analyst Bot".into(),
                    x: 580.0,
                    y: 120.0,
                    config: json!({
                        "model": "",
                        "system": "You are a concise NOC operations assistant. Format an executive morning WhatsApp report with bullet points.",
                        "prompt": "Analyze this infrastructure telemetry and list any offline equipment or confirm all systems are healthy:\n\n{{prev}}"
                    }),
                },
                WorkflowNode {
                    id: "n4".into(),
                    kind: NodeKind::Condition,
                    label: "Non-empty report?".into(),
                    x: 850.0,
                    y: 120.0,
                    config: json!({ "mode": "not_empty", "pattern": "" }),
                },
                WorkflowNode {
                    id: "n5".into(),
                    kind: NodeKind::NotifyChannel,
                    label: "Send to WhatsApp / Channel".into(),
                    x: 1120.0,
                    y: 80.0,
                    config: json!({
                        "channel_id": "",
                        "target": "",
                        "message": "*NOC Morning Report ({{date}})*\n\n{{prev}}"
                    }),
                },
            ],
            edges: vec![
                WorkflowEdge { id: "e1".into(), from: "n1".into(), to: "n2".into(), branch: None },
                WorkflowEdge { id: "e2".into(), from: "n2".into(), to: "n3".into(), branch: None },
                WorkflowEdge { id: "e3".into(), from: "n3".into(), to: "n4".into(), branch: None },
                WorkflowEdge { id: "e4".into(), from: "n4".into(), to: "n5".into(), branch: Some("true".into()) },
            ],
            created_at_ms: now,
            updated_at_ms: now,
            last_run_at_ms: None,
            last_status: None,
            bot_name: Some("noc-engineer".into()),
        },
        Workflow {
            alert: Default::default(),
            run_on: None,
            id: "api-health-monitor".into(),
            name: "Global HTTP Healthcheck".into(),
            description: "Polls an HTTP endpoint every 15 minutes and triggers an alert if it stops responding.".into(),
            icon: "globe".into(),
            enabled: false,
            nodes: vec![
                WorkflowNode {
                    id: "h1".into(),
                    kind: NodeKind::TriggerCron,
                    label: "Every 15 minutes".into(),
                    x: 60.0,
                    y: 140.0,
                    config: json!({ "cron": "*/15 * * * *", "tz_offset_minutes": -180 }),
                },
                WorkflowNode {
                    id: "h2".into(),
                    kind: NodeKind::HttpRequest,
                    label: "Probe endpoint".into(),
                    x: 340.0,
                    y: 140.0,
                    config: json!({
                        "method": "GET",
                        "url": "https://httpbin.org/status/200",
                        "headers": {},
                        "body": "",
                        "allow_error_status": true
                    }),
                },
                WorkflowNode {
                    id: "h3".into(),
                    kind: NodeKind::Condition,
                    label: "Status != 200?".into(),
                    x: 620.0,
                    y: 140.0,
                    config: json!({ "mode": "not_contains", "pattern": "HTTP 200" }),
                },
                WorkflowNode {
                    id: "h4".into(),
                    kind: NodeKind::NotifyChannel,
                    label: "Alert On-Call".into(),
                    x: 900.0,
                    y: 140.0,
                    config: json!({
                        "channel_id": "",
                        "target": "",
                        "message": "⚠️ Endpoint alert ({{date}} {{time}}):\n{{prev}}"
                    }),
                },
            ],
            edges: vec![
                WorkflowEdge { id: "he1".into(), from: "h1".into(), to: "h2".into(), branch: None },
                WorkflowEdge { id: "he2".into(), from: "h2".into(), to: "h3".into(), branch: None },
                WorkflowEdge { id: "he3".into(), from: "h3".into(), to: "h4".into(), branch: Some("true".into()) },
            ],
            created_at_ms: now,
            updated_at_ms: now - 1000,
            last_run_at_ms: None,
            last_status: None,
            bot_name: Some("noc-engineer".into()),
        },
        Workflow {
            alert: Default::default(),
            run_on: None,
            id: "daily-workspace-digest".into(),
            name: "Daily Git & System Digest".into(),
            description: "Collects local system info, summarizes it with your AI model, and saves a daily Markdown log.".into(),
            icon: "file-text".into(),
            enabled: false,
            nodes: vec![
                WorkflowNode {
                    id: "d1".into(),
                    kind: NodeKind::TriggerManual,
                    label: "Manual / Scheduled".into(),
                    x: 60.0,
                    y: 140.0,
                    config: json!({}),
                },
                WorkflowNode {
                    id: "d2".into(),
                    kind: NodeKind::Shell,
                    label: "System Snapshot".into(),
                    x: 340.0,
                    y: 140.0,
                    config: json!({
                        "command": "uname -a 2>/dev/null || hostname",
                        "timeout_secs": 15
                    }),
                },
                WorkflowNode {
                    id: "d3".into(),
                    kind: NodeKind::AiAgent,
                    label: "Summarize Digest".into(),
                    x: 620.0,
                    y: 140.0,
                    config: json!({
                        "model": "",
                        "system": "Write a crisp markdown daily digest.",
                        "prompt": "Summarize this snapshot for {{date}}:\n{{prev}}"
                    }),
                },
                WorkflowNode {
                    id: "d4".into(),
                    kind: NodeKind::SaveFile,
                    label: "Save Markdown Report".into(),
                    x: 900.0,
                    y: 140.0,
                    config: json!({
                        "path": "reports/digest-{{date}}.md",
                        "append": false
                    }),
                },
            ],
            edges: vec![
                WorkflowEdge { id: "de1".into(), from: "d1".into(), to: "d2".into(), branch: None },
                WorkflowEdge { id: "de2".into(), from: "d2".into(), to: "d3".into(), branch: None },
                WorkflowEdge { id: "de3".into(), from: "d3".into(), to: "d4".into(), branch: None },
            ],
            created_at_ms: now,
            updated_at_ms: now - 2000,
            last_run_at_ms: None,
            last_status: None,
            bot_name: Some("workflow-architect".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roda um modelo "Equipamentos offline" com a consulta fixada em `sample`
    /// e o aviso fixado (sem canal); devolve (texto do transform, aviso rodou).
    pub(crate) fn run_offline_template(id: &str, sample: &str) -> (String, bool) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let mut wf = starter_templates()
            .into_iter()
            .find(|w| w.id == id)
            .unwrap();
        for n in &mut wf.nodes {
            match n.id.as_str() {
                "o2" => n.config = json!({"pin_enabled": true, "pinned_output": sample}),
                "o5" => n.config = json!({"pin_enabled": true, "pinned_output": "enviado"}),
                _ => {}
            }
        }
        let mut opts = crate::RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &crate::NoopAiExecutor,
            trigger_source: "manual",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let run = crate::execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.node_logs);
        let log = |id: &str| run.node_logs.iter().find(|l| l.node_id == id).unwrap();
        (log("o3").output.clone(), log("o5").status == "ok")
    }

    #[test]
    fn modelo_dude_offline_monta_texto_e_so_avisa_com_offline() {
        let wf = starter_templates()
            .into_iter()
            .find(|w| w.id == "dude-equipamentos-offline")
            .unwrap();
        assert_eq!(wf.nodes[0].config["cron"], "50 7 * * *");
        assert_eq!(wf.alert.alert_after_failures, Some(1));
        assert!(wf.nodes[1].config["command"]
            .as_str()
            .unwrap()
            .starts_with("/dude device print detail"));

        let sample = include_str!("fixtures/exemplo-dude-device-print-detail.txt");
        let (text, sent) = run_offline_template("dude-equipamentos-offline", sample);
        assert!(text.starts_with("Equipamentos offline às "), "{text}");
        assert!(text.contains("(2):\n- Switch Bloco B (198.18.1.20)\n- radio-torre-3"));
        assert!(sent);
        let (text, sent) = run_offline_template(
            "dude-equipamentos-offline",
            &sample.replace("status=down", "status=up"),
        );
        assert!(text.starts_with("Nenhum equipamento offline às "), "{text}");
        assert!(!sent, "condição barra o aviso sem offline");
    }

    #[test]
    fn corrupt_graph_json_errors_instead_of_loading_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("c.db")).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO workflows (id, name, nodes_json, edges_json, created_at_ms, updated_at_ms)
                 VALUES ('bad', 'bad', '{not json', '[]', 1, 1)",
                [],
            )
            .unwrap();
        assert!(store.get_workflow("bad").is_err());
        assert!(store.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn alert_config_round_trips_and_old_json_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("a.db")).unwrap();
        let mut wf: Workflow = serde_json::from_value(
            json!({"id": "w", "name": "w", "created_at_ms": 1, "updated_at_ms": 1}),
        )
        .unwrap();
        assert_eq!(wf.alert, WorkflowAlert::default());
        wf.alert.alert_after_failures = Some(3);
        wf.alert.alert_channels = vec!["zap".into()];
        wf.alert.alert_target.insert("zap".into(), "5511".into());
        store.save_workflow(&wf).unwrap();
        let back = store.get_workflow("w").unwrap().unwrap();
        assert_eq!(back.alert, wf.alert);
        let v = serde_json::to_value(&back).unwrap();
        assert_eq!(v["alert_channels"], json!(["zap"]));
        assert_eq!(v["alert_after_failures"], json!(3));
    }

    #[test]
    fn seed_defaults_does_not_resurrect_after_user_deletes_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("workflows.db")).unwrap();

        store.seed_defaults().unwrap();
        let seeded = store.list_workflows().unwrap();
        assert!(!seeded.is_empty());

        for wf in seeded {
            store.delete_workflow(&wf.id).unwrap();
        }
        assert!(store.list_workflows().unwrap().is_empty());

        // Calling seed_defaults again on subsequent startups must leave 0 workflows.
        store.seed_defaults().unwrap();
        assert!(store.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn seed_defaults_marks_existing_nonempty_db_without_inserting_templates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("legacy.db")).unwrap();
        let custom = Workflow {
            alert: Default::default(),
            run_on: None,
            id: "custom-1".into(),
            name: "Custom".into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: false,
            nodes: Vec::new(),
            edges: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        };
        store.save_workflow(&custom).unwrap();

        store.seed_defaults().unwrap();
        let list = store.list_workflows().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "custom-1");

        store.delete_workflow("custom-1").unwrap();
        store.seed_defaults().unwrap();
        assert!(store.list_workflows().unwrap().is_empty());
    }
}
