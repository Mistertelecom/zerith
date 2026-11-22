//! Scheduled jobs & zero-dependency 5-field cron evaluator (`UTC`).

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::migrations::now_ms;
use crate::store::{new_id, Result, StoreError};
use crate::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    min: [bool; 60],
    hour: [bool; 24],
    dom: [bool; 32],
    month: [bool; 13],
    dow: [bool; 7],
    /// Standard cron: when both day-of-month and day-of-week are restricted,
    /// a day matching either one fires.
    dom_star: bool,
    dow_star: bool,
}

impl CronExpr {
    /// Parses a standard 5-field cron expression (`min hour dom month dow`, UTC).
    pub fn parse(expr: &str) -> std::result::Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expressão cron inválida `{expr}`: espere 5 campos (min hora dia mês semana)"
            ));
        }
        let min = parse_field::<60>(fields[0], 0, 59)?;
        let hour = parse_field::<24>(fields[1], 0, 23)?;
        let dom = parse_field::<32>(fields[2], 1, 31)?;
        let month = parse_field::<13>(fields[3], 1, 12)?;
        let dow = parse_dow(fields[4])?;
        Ok(Self {
            min,
            hour,
            dom,
            month,
            dow,
            dom_star: fields[2].starts_with('*'),
            dow_star: fields[4].starts_with('*'),
        })
    }

    /// True if `epoch_secs` (UTC) matches this schedule's minute.
    pub fn matches_unix_secs(&self, epoch_secs: i64) -> bool {
        let (min, hour, dom, month, dow) = utc_components(epoch_secs);
        let dom_ok = self.dom.get(dom as usize).copied().unwrap_or(false);
        let dow_ok = self.dow.get(dow as usize).copied().unwrap_or(false);
        let day_ok = if self.dom_star || self.dow_star {
            dom_ok && dow_ok
        } else {
            dom_ok || dow_ok
        };
        self.min.get(min as usize).copied().unwrap_or(false)
            && self.hour.get(hour as usize).copied().unwrap_or(false)
            && self.month.get(month as usize).copied().unwrap_or(false)
            && day_ok
    }
}

fn parse_dow(spec: &str) -> std::result::Result<[bool; 7], String> {
    let raw = parse_field::<8>(spec, 0, 7)?;
    let mut out = [false; 7];
    out.copy_from_slice(&raw[..7]);
    if raw[7] {
        out[0] = true; // 7 is Sunday alias in standard cron
    }
    Ok(out)
}

fn parse_field<const N: usize>(
    spec: &str,
    min: usize,
    max: usize,
) -> std::result::Result<[bool; N], String> {
    let mut out = [false; N];
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("campo cron vazio em `{spec}`"));
        }
        let (range_part, step) = match part.split_once('/') {
            Some((lhs, rhs)) => {
                let s: usize = rhs
                    .parse()
                    .map_err(|_| format!("passo cron inválido: {part}"))?;
                if s == 0 {
                    return Err(format!("passo cron precisa ser > 0: {part}"));
                }
                (lhs, s)
            }
            None => (part, 1usize),
        };
        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let s: usize = a.parse().map_err(|_| format!("cron inválido: {part}"))?;
            let e: usize = b.parse().map_err(|_| format!("cron inválido: {part}"))?;
            (s, e)
        } else {
            let v: usize = range_part
                .parse()
                .map_err(|_| format!("cron inválido: {part}"))?;
            // `5/15` starts at 5 up to max in cron implementations
            let e = if part.contains('/') { max } else { v };
            (v, e)
        };
        if start < min || end > max || start > end {
            return Err(format!(
                "intervalo fora dos limites ({min}..={max}): {part}"
            ));
        }
        let mut cur = start;
        while cur <= end {
            out[cur] = true;
            match cur.checked_add(step) {
                Some(next) if next <= max => cur = next,
                _ => break,
            }
        }
    }
    Ok(out)
}

/// Converts Unix epoch seconds (UTC) into `(min, hour, day_of_month, month, day_of_week 0=Sun)`.
/// Uses Howard Hinnant's civil-from-days algorithm cleanly in pure Rust.
pub fn utc_components(epoch_secs: i64) -> (u32, u32, u32, u32, u32) {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    // 1970-01-01 was Thursday (4)
    let dow = (days + 4).rem_euclid(7) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dom = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (min, hour, dom, month, dow)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub cron_expr: String,
    pub workspace: String,
    pub prompt: Option<String>,
    pub command: Option<String>,
    pub no_agent: bool,
    pub model: Option<String>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
    pub last_output: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub name: String,
    pub cron_expr: String,
    pub workspace: String,
    pub prompt: Option<String>,
    pub command: Option<String>,
    pub no_agent: bool,
    pub model: Option<String>,
    pub enabled: bool,
}

const COLS: &str = "id, name, cron_expr, workspace, prompt, command, no_agent, model, enabled, last_run_at, last_status, last_output, created_at";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: r.get(0)?,
        name: r.get(1)?,
        cron_expr: r.get(2)?,
        workspace: r.get(3)?,
        prompt: r.get(4)?,
        command: r.get(5)?,
        no_agent: r.get::<_, i64>(6)? != 0,
        model: r.get(7)?,
        enabled: r.get::<_, i64>(8)? != 0,
        last_run_at: r.get(9)?,
        last_status: r.get(10)?,
        last_output: r.get(11)?,
        created_at: r.get(12)?,
    })
}

impl Store {
    pub fn schedule_add(&self, n: &NewSchedule) -> Result<Schedule> {
        if let Err(e) = CronExpr::parse(&n.cron_expr) {
            return Err(StoreError::Invalid(e));
        }
        if n.name.trim().is_empty() {
            return Err(StoreError::Invalid(
                "nome da tarefa não pode ficar vazio".into(),
            ));
        }
        if n.prompt.as_deref().unwrap_or("").trim().is_empty()
            && n.command.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(StoreError::Invalid("informe --prompt ou --command".into()));
        }
        let id = new_id();
        let ts = now_ms();
        self.conn().execute(
            "INSERT INTO schedules(id, name, cron_expr, workspace, prompt, command, no_agent, model, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                n.name.trim(),
                n.cron_expr.trim(),
                n.workspace.trim(),
                n.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                n.command.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                i64::from(n.no_agent),
                n.model.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                i64::from(n.enabled),
                ts
            ],
        )?;
        self.schedule_get(&id)?
            .ok_or_else(|| StoreError::NotFound(id))
    }

    pub fn schedule_list(&self) -> Result<Vec<Schedule>> {
        let mut st = self.conn().prepare(&format!(
            "SELECT {COLS} FROM schedules ORDER BY created_at ASC"
        ))?;
        let rows = st.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn schedule_get(&self, id_or_name: &str) -> Result<Option<Schedule>> {
        let s = self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM schedules WHERE id = ?1 OR name = ?1 LIMIT 1"),
                params![id_or_name],
                from_row,
            )
            .optional()?;
        Ok(s)
    }

    pub fn schedule_set_enabled(&self, id_or_name: &str, enabled: bool) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE schedules SET enabled = ?1 WHERE id = ?2 OR name = ?2",
            params![i64::from(enabled), id_or_name],
        )?;
        Ok(n > 0)
    }

    pub fn schedule_remove(&self, id_or_name: &str) -> Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM schedules WHERE id = ?1 OR name = ?1",
            params![id_or_name],
        )?;
        Ok(n > 0)
    }

    pub fn schedule_record_run(
        &self,
        id: &str,
        run_at_ms: i64,
        status: &str,
        output: &str,
    ) -> Result<()> {
        let clipped: String = output.chars().take(4000).collect();
        self.conn().execute(
            "UPDATE schedules SET last_run_at = ?1, last_status = ?2, last_output = ?3 WHERE id = ?4",
            params![run_at_ms, status, clipped, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_parser_handles_wildcards_steps_ranges_lists_and_utc() {
        let c = CronExpr::parse("*/15 9-17 * 1,11 1-5").unwrap();
        // 2026-11-20 12:30 UTC -> Friday (dow=5), month=11, hour=12, min=30
        let ts = 1_795_177_800i64; // verify exact UTC components first
        let (min, hour, _dom, month, dow) = utc_components(ts);
        assert_eq!((month, dow, hour, min), (11, 5, 12, 30));
        assert!(c.matches_unix_secs(ts));
        assert!(!c.matches_unix_secs(ts + 60)); // minute 31 does not match */15
        assert!(CronExpr::parse("0 0 * * 7")
            .unwrap()
            .matches_unix_secs(259_200)); // 1970-01-04 Sunday
        assert!(CronExpr::parse("bad").is_err());
        // dom=1 OR Monday: 1970-01-05 was a Monday (not the 1st), 1970-01-01 a Thursday.
        let either = CronExpr::parse("0 0 1 * 1").unwrap();
        assert!(either.matches_unix_secs(4 * 86_400));
        assert!(either.matches_unix_secs(0));
        assert!(!either.matches_unix_secs(86_400));
        assert!(CronExpr::parse("61 * * * *").is_err());
    }

    #[test]
    fn cron_parser_handles_large_step_without_overflow() {
        let c = CronExpr::parse("1/18446744073709551615 * * * *").unwrap();
        assert!(c.matches_unix_secs(60));
        assert!(!c.matches_unix_secs(0));
        assert!(!c.matches_unix_secs(120));
    }

    #[test]
    fn schedule_crud_and_run_recording_work() {
        let store = Store::open_in_memory().unwrap();
        let added = store
            .schedule_add(&NewSchedule {
                name: "bgp-check".into(),
                cron_expr: "*/5 * * * *".into(),
                workspace: "/tmp/ws".into(),
                prompt: None,
                command: Some("echo ok".into()),
                no_agent: true,
                model: None,
                enabled: true,
            })
            .unwrap();
        assert_eq!(added.name, "bgp-check");
        assert!(added.no_agent);
        assert_eq!(store.schedule_list().unwrap().len(), 1);
        assert!(store.schedule_set_enabled("bgp-check", false).unwrap());
        assert!(!store.schedule_get("bgp-check").unwrap().unwrap().enabled);
        store
            .schedule_record_run(&added.id, 1_700_000_000_000, "ok", "bgp up")
            .unwrap();
        let got = store.schedule_get(&added.id).unwrap().unwrap();
        assert_eq!(got.last_status.as_deref(), Some("ok"));
        assert!(store.schedule_remove("bgp-check").unwrap());
        assert!(store.schedule_list().unwrap().is_empty());
    }
}
