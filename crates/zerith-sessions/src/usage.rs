//! Persistent usage tracking, rolling-window quotas, Token Saver savings, and
//! per-account cooldowns (migration 7).

use crate::migrations::now_ms;
use crate::store::{Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

const MS_PER_HOUR: i64 = 3_600_000;

/// One completed or attempted LLM request to record in `usage_records`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewUsageRecord {
    pub session_id: Option<String>,
    pub account_id: String,
    pub provider: String,
    pub model: String,
    /// Role of the caller, e.g. `"main"`, `"subagent"`, `"plan"`.
    pub role_kind: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Tokens avoided by the RTK protocol for this request.
    pub saved_rtk_tokens: u64,
    /// Tokens avoided by the Ponytail protocol for this request.
    pub saved_ponytail_tokens: u64,
    /// Tokens avoided by the Caveman protocol for this request.
    pub saved_caveman_tokens: u64,
    /// Total tokens avoided by Token Saver for this request.
    pub saved_tokens: u64,
    /// Request outcome, e.g. `"ok"`, `"rate_limited"`, `"error"`.
    pub status: String,
    pub latency_ms: u64,
    #[serde(default)]
    pub ttft_ms: u64,
}

impl Default for NewUsageRecord {
    fn default() -> Self {
        Self {
            session_id: None,
            account_id: String::new(),
            provider: String::new(),
            model: String::new(),
            role_kind: "main".to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            saved_rtk_tokens: 0,
            saved_ponytail_tokens: 0,
            saved_caveman_tokens: 0,
            saved_tokens: 0,
            status: "ok".to_string(),
            latency_ms: 0,
            ttft_ms: 0,
        }
    }
}

/// Aggregated usage and cooldown status for a single account over a rolling window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountWindowStats {
    pub account_id: String,
    pub provider: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub saved_rtk_tokens: u64,
    pub saved_ponytail_tokens: u64,
    pub saved_caveman_tokens: u64,
    pub saved_tokens: u64,
    pub requests: u64,
    pub rate_limits: u64,
    pub cooldown_until: i64,
    pub last_error: Option<String>,
    pub last_model: Option<String>,
    pub last_used_at: Option<i64>,
}

/// Aggregated usage grouped by `(model, provider, role_kind)` over a rolling window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsageStats {
    pub model: String,
    pub provider: String,
    pub role_kind: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub saved_rtk_tokens: u64,
    pub saved_ponytail_tokens: u64,
    pub saved_caveman_tokens: u64,
    pub saved_tokens: u64,
    pub requests: u64,
}

/// Global usage summary across all accounts and models for a rolling window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub accounts: Vec<AccountWindowStats>,
    pub models: Vec<ModelUsageStats>,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    #[serde(default)]
    pub total_cache_read_tokens: u64,
    #[serde(default)]
    pub total_cache_write_tokens: u64,
    pub total_saved_rtk_tokens: u64,
    pub total_saved_ponytail_tokens: u64,
    pub total_saved_caveman_tokens: u64,
    pub total_saved_tokens: u64,
    pub total_requests: u64,
    pub active_account: Option<String>,
    pub active_model: Option<String>,
}

/// Aggregated token usage and prompt cache metrics for one conversation session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageTotals {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub saved_tokens: u64,
    pub saved_rtk_tokens: u64,
    pub requests: u64,
}

/// Tokens de um grupo (dia local, sessão, conta, modelo): a base do custo,
/// que é calculado fora daqui (o preço vem do catálogo, não do banco).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCostRow {
    /// `AAAA-MM-DD` no fuso local.
    pub day: String,
    pub session_id: Option<String>,
    pub session_title: Option<String>,
    pub account_id: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub requests: u64,
}

fn window_since_ms(window_hours: u32) -> i64 {
    if window_hours == 0 {
        0
    } else {
        now_ms().saturating_sub(i64::from(window_hours).saturating_mul(MS_PER_HOUR))
    }
}

fn to_sql_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn from_sql_u64(v: i64) -> u64 {
    v.max(0) as u64
}

impl Store {
    /// Inserts one usage record and returns its rowid.
    ///
    /// If `session_id` is supplied but no matching row exists in `sessions`
    /// (for instance in ephemeral subagent or test runs), it is stored as `NULL`
    /// rather than failing the foreign key and dropping accounting data.
    pub fn record_usage(&self, rec: &NewUsageRecord) -> Result<i64> {
        let tx = self.write_tx()?;
        let session_id = match rec.session_id.as_deref() {
            Some(sid) if !sid.is_empty() => {
                let exists: Option<i64> = tx
                    .query_row("SELECT 1 FROM sessions WHERE id = ?1", params![sid], |r| {
                        r.get(0)
                    })
                    .optional()?;
                exists.map(|_| sid)
            }
            _ => None,
        };
        let role_kind = if rec.role_kind.trim().is_empty() {
            "main"
        } else {
            rec.role_kind.as_str()
        };
        let status = if rec.status.trim().is_empty() {
            "ok"
        } else {
            rec.status.as_str()
        };
        let specific_sum = rec
            .saved_rtk_tokens
            .saturating_add(rec.saved_ponytail_tokens)
            .saturating_add(rec.saved_caveman_tokens);
        let (saved_rtk, saved_ponytail, saved_caveman, saved_total) = if specific_sum > 0 {
            (
                rec.saved_rtk_tokens,
                rec.saved_ponytail_tokens,
                rec.saved_caveman_tokens,
                rec.saved_tokens.max(specific_sum),
            )
        } else {
            (rec.saved_tokens, 0, 0, rec.saved_tokens)
        };
        tx.execute(
            "INSERT INTO usage_records(
                 session_id, account_id, provider, model, role_kind,
                 prompt_tokens, completion_tokens,
                 cache_read_tokens, cache_write_tokens,
                 saved_rtk_tokens, saved_ponytail_tokens, saved_caveman_tokens, saved_tokens,
                 status, latency_ms, ttft_ms, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                session_id,
                rec.account_id,
                rec.provider,
                rec.model,
                role_kind,
                to_sql_i64(rec.prompt_tokens),
                to_sql_i64(rec.completion_tokens),
                to_sql_i64(rec.cache_read_tokens),
                to_sql_i64(rec.cache_write_tokens),
                to_sql_i64(saved_rtk),
                to_sql_i64(saved_ponytail),
                to_sql_i64(saved_caveman),
                to_sql_i64(saved_total),
                status,
                to_sql_i64(rec.latency_ms),
                to_sql_i64(rec.ttft_ms),
                now_ms(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Sets or updates a cooldown deadline (Unix ms) and optional error reason for `account_id`.
    pub fn set_account_cooldown(
        &self,
        account_id: &str,
        until_ms: i64,
        reason: Option<&str>,
    ) -> Result<()> {
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO account_cooldowns(account_id, cooldown_until, last_error, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id) DO UPDATE SET
                 cooldown_until = excluded.cooldown_until,
                 last_error = excluded.last_error,
                 updated_at = excluded.updated_at",
            params![account_id, until_ms, reason, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Removes any stored cooldown for `account_id`.
    pub fn clear_account_cooldown(&self, account_id: &str) -> Result<()> {
        let tx = self.write_tx()?;
        tx.execute(
            "DELETE FROM account_cooldowns WHERE account_id = ?1",
            params![account_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Computes rolling-window usage totals and cooldown state for `account_id`.
    /// Passing `window_hours = 0` aggregates across all history.
    pub fn account_window_stats(
        &self,
        account_id: &str,
        window_hours: u32,
    ) -> Result<AccountWindowStats> {
        let since = window_since_ms(window_hours);
        let (
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            saved_rtk_tokens,
            saved_ponytail_tokens,
            saved_caveman_tokens,
            saved_tokens,
            requests,
            rate_limits,
        ) = self.conn().query_row(
            "SELECT
                 COALESCE(SUM(prompt_tokens), 0),
                 COALESCE(SUM(completion_tokens), 0),
                 COALESCE(SUM(cache_read_tokens), 0),
                 COALESCE(SUM(cache_write_tokens), 0),
                 COALESCE(SUM(saved_rtk_tokens), 0),
                 COALESCE(SUM(saved_ponytail_tokens), 0),
                 COALESCE(SUM(saved_caveman_tokens), 0),
                 COALESCE(SUM(saved_tokens), 0),
                 COUNT(*),
                 COALESCE(SUM(CASE WHEN status = 'rate_limited' THEN 1 ELSE 0 END), 0)
             FROM usage_records
             WHERE account_id = ?1 AND created_at >= ?2",
            params![account_id, since],
            |r| {
                Ok((
                    from_sql_u64(r.get::<_, i64>(0)?),
                    from_sql_u64(r.get::<_, i64>(1)?),
                    from_sql_u64(r.get::<_, i64>(2)?),
                    from_sql_u64(r.get::<_, i64>(3)?),
                    from_sql_u64(r.get::<_, i64>(4)?),
                    from_sql_u64(r.get::<_, i64>(5)?),
                    from_sql_u64(r.get::<_, i64>(6)?),
                    from_sql_u64(r.get::<_, i64>(7)?),
                    from_sql_u64(r.get::<_, i64>(8)?),
                    from_sql_u64(r.get::<_, i64>(9)?),
                ))
            },
        )?;

        let latest: Option<(String, String, i64)> = self
            .conn()
            .query_row(
                "SELECT provider, model, created_at
                 FROM usage_records
                 WHERE account_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                params![account_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let cooldown: Option<(i64, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT cooldown_until, last_error
                 FROM account_cooldowns
                 WHERE account_id = ?1",
                params![account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let (provider, last_model, last_used_at) = match latest {
            Some((p, m, ts)) => (p, Some(m), Some(ts)),
            None => (String::new(), None, None),
        };
        let (cooldown_until, last_error) = cooldown.unwrap_or((0, None));

        Ok(AccountWindowStats {
            account_id: account_id.to_string(),
            provider,
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
            cache_read_tokens,
            cache_write_tokens,
            saved_rtk_tokens,
            saved_ponytail_tokens,
            saved_caveman_tokens,
            saved_tokens,
            requests,
            rate_limits,
            cooldown_until,
            last_error,
            last_model,
            last_used_at,
        })
    }

    /// Aggregates all recorded usage rows belonging to `session_id`.
    pub fn session_usage(&self, session_id: &str) -> Result<SessionUsageTotals> {
        self.conn()
            .query_row(
                "SELECT
                     COALESCE(SUM(prompt_tokens), 0),
                     COALESCE(SUM(completion_tokens), 0),
                     COALESCE(SUM(cache_read_tokens), 0),
                     COALESCE(SUM(cache_write_tokens), 0),
                     COALESCE(SUM(saved_tokens), 0),
                     COALESCE(SUM(saved_rtk_tokens), 0),
                     COUNT(*)
                 FROM usage_records
                 WHERE session_id = ?1",
                [session_id],
                |r| {
                    Ok(SessionUsageTotals {
                        prompt_tokens: from_sql_u64(r.get::<_, i64>(0)?),
                        completion_tokens: from_sql_u64(r.get::<_, i64>(1)?),
                        cache_read_tokens: from_sql_u64(r.get::<_, i64>(2)?),
                        cache_write_tokens: from_sql_u64(r.get::<_, i64>(3)?),
                        saved_tokens: from_sql_u64(r.get::<_, i64>(4)?),
                        saved_rtk_tokens: from_sql_u64(r.get::<_, i64>(5)?),
                        requests: from_sql_u64(r.get::<_, i64>(6)?),
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Tokens agrupados por dia, sessão, conta e modelo desde `window_hours`
    /// (`0` = tudo). Com `session`, só ela e os subagentes que ela abriu.
    pub fn usage_cost_rows(
        &self,
        window_hours: u32,
        session: Option<&str>,
    ) -> Result<Vec<UsageCostRow>> {
        let since = window_since_ms(window_hours);
        let mut st = self.conn().prepare(
            "SELECT date(u.created_at / 1000, 'unixepoch', 'localtime') AS day,
                    u.session_id, s.title, u.account_id, u.provider, u.model,
                    COALESCE(SUM(u.prompt_tokens), 0), COALESCE(SUM(u.completion_tokens), 0),
                    COALESCE(SUM(u.cache_read_tokens), 0), COALESCE(SUM(u.cache_write_tokens), 0),
                    COUNT(*)
             FROM usage_records u LEFT JOIN sessions s ON s.id = u.session_id
             WHERE u.created_at >= ?1
               AND (?2 IS NULL OR u.session_id = ?2 OR u.session_id IN
                   (SELECT id FROM sessions WHERE source = 'subagent' AND source_id = ?2))
             GROUP BY day, u.session_id, u.account_id, u.provider, u.model
             ORDER BY day, u.session_id, u.model",
        )?;
        let rows = st.query_map(params![since, session], |r| {
            let n = |i| r.get::<_, i64>(i).map(from_sql_u64);
            Ok(UsageCostRow {
                day: r.get(0)?,
                session_id: r.get(1)?,
                session_title: r.get(2)?,
                account_id: r.get(3)?,
                provider: r.get(4)?,
                model: r.get(5)?,
                prompt_tokens: n(6)?,
                completion_tokens: n(7)?,
                cache_read_tokens: n(8)?,
                cache_write_tokens: n(9)?,
                requests: n(10)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Returns `true` when `account_id` is neither in an active cooldown nor at/above
    /// its rolling-window token or request quota.
    pub fn is_account_available(
        &self,
        account_id: &str,
        window_hours: u32,
        quota_tokens: Option<u64>,
        quota_requests: Option<u64>,
    ) -> Result<bool> {
        let stats = self.account_window_stats(account_id, window_hours)?;
        if stats.cooldown_until > now_ms() {
            return Ok(false);
        }
        if quota_tokens.is_some_and(|q| stats.total_tokens >= q) {
            return Ok(false);
        }
        if quota_requests.is_some_and(|q| stats.requests >= q) {
            return Ok(false);
        }
        Ok(true)
    }

    /// Summarizes per-account, per-model/role, and global usage across `window_hours`
    /// (`0` for all time).
    pub fn usage_summary(&self, window_hours: u32) -> Result<UsageSummary> {
        let since = window_since_ms(window_hours);

        let account_ids: Vec<String> = {
            let mut st = self.conn().prepare(
                "SELECT account_id FROM usage_records
                 UNION
                 SELECT account_id FROM account_cooldowns
                 ORDER BY account_id",
            )?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut accounts = Vec::with_capacity(account_ids.len());
        for account_id in &account_ids {
            accounts.push(self.account_window_stats(account_id, window_hours)?);
        }

        let models: Vec<ModelUsageStats> = {
            let mut st = self.conn().prepare(
                "SELECT
                     model,
                     provider,
                     role_kind,
                     COALESCE(SUM(prompt_tokens), 0),
                     COALESCE(SUM(completion_tokens), 0),
                     COALESCE(SUM(cache_read_tokens), 0),
                     COALESCE(SUM(cache_write_tokens), 0),
                     COALESCE(SUM(saved_rtk_tokens), 0),
                     COALESCE(SUM(saved_ponytail_tokens), 0),
                     COALESCE(SUM(saved_caveman_tokens), 0),
                     COALESCE(SUM(saved_tokens), 0),
                     COUNT(*)
                 FROM usage_records
                 WHERE created_at >= ?1
                 GROUP BY model, provider, role_kind
                 ORDER BY (COALESCE(SUM(prompt_tokens), 0) + COALESCE(SUM(completion_tokens), 0)) DESC,
                          model ASC,
                          provider ASC,
                          role_kind ASC",
            )?;
            let rows = st.query_map(params![since], |r| {
                let prompt_tokens = from_sql_u64(r.get::<_, i64>(3)?);
                let completion_tokens = from_sql_u64(r.get::<_, i64>(4)?);
                Ok(ModelUsageStats {
                    model: r.get(0)?,
                    provider: r.get(1)?,
                    role_kind: r.get(2)?,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens.saturating_add(completion_tokens),
                    cache_read_tokens: from_sql_u64(r.get::<_, i64>(5)?),
                    cache_write_tokens: from_sql_u64(r.get::<_, i64>(6)?),
                    saved_rtk_tokens: from_sql_u64(r.get::<_, i64>(7)?),
                    saved_ponytail_tokens: from_sql_u64(r.get::<_, i64>(8)?),
                    saved_caveman_tokens: from_sql_u64(r.get::<_, i64>(9)?),
                    saved_tokens: from_sql_u64(r.get::<_, i64>(10)?),
                    requests: from_sql_u64(r.get::<_, i64>(11)?),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let (
            total_prompt_tokens,
            total_completion_tokens,
            total_cache_read_tokens,
            total_cache_write_tokens,
            total_saved_rtk_tokens,
            total_saved_ponytail_tokens,
            total_saved_caveman_tokens,
            total_saved_tokens,
            total_requests,
        ) = self.conn().query_row(
            "SELECT
                 COALESCE(SUM(prompt_tokens), 0),
                 COALESCE(SUM(completion_tokens), 0),
                 COALESCE(SUM(cache_read_tokens), 0),
                 COALESCE(SUM(cache_write_tokens), 0),
                 COALESCE(SUM(saved_rtk_tokens), 0),
                 COALESCE(SUM(saved_ponytail_tokens), 0),
                 COALESCE(SUM(saved_caveman_tokens), 0),
                 COALESCE(SUM(saved_tokens), 0),
                 COUNT(*)
             FROM usage_records
             WHERE created_at >= ?1",
            params![since],
            |r| {
                Ok((
                    from_sql_u64(r.get::<_, i64>(0)?),
                    from_sql_u64(r.get::<_, i64>(1)?),
                    from_sql_u64(r.get::<_, i64>(2)?),
                    from_sql_u64(r.get::<_, i64>(3)?),
                    from_sql_u64(r.get::<_, i64>(4)?),
                    from_sql_u64(r.get::<_, i64>(5)?),
                    from_sql_u64(r.get::<_, i64>(6)?),
                    from_sql_u64(r.get::<_, i64>(7)?),
                    from_sql_u64(r.get::<_, i64>(8)?),
                ))
            },
        )?;

        let active = match self
            .conn()
            .query_row(
                "SELECT account_id, model
                 FROM usage_records
                 WHERE status = 'ok'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            Some(pair) => Some(pair),
            None => self
                .conn()
                .query_row(
                    "SELECT account_id, model
                     FROM usage_records
                     ORDER BY created_at DESC, id DESC
                     LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?,
        };

        let (active_account, active_model) = match active {
            Some((account, model)) => (Some(account), Some(model)),
            None => (None, None),
        };

        Ok(UsageSummary {
            accounts,
            models,
            total_prompt_tokens,
            total_completion_tokens,
            total_cache_read_tokens,
            total_cache_write_tokens,
            total_saved_rtk_tokens,
            total_saved_ponytail_tokens,
            total_saved_caveman_tokens,
            total_saved_tokens,
            total_requests,
            active_account,
            active_model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionSource;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn record_usage_and_summary_aggregate_accounts_roles_and_saved_tokens() {
        let s = store();
        assert_eq!(
            s.schema_version().unwrap(),
            crate::migrations::CURRENT_VERSION
        );
        assert!(s.integrity_ok().unwrap());

        let empty = s.usage_summary(24).unwrap();
        assert!(empty.accounts.is_empty());
        assert!(empty.models.is_empty());
        assert_eq!(empty.total_requests, 0);
        assert_eq!(empty.total_saved_rtk_tokens, 0);
        assert_eq!(empty.total_saved_ponytail_tokens, 0);
        assert_eq!(empty.total_saved_caveman_tokens, 0);
        assert_eq!(empty.total_saved_tokens, 0);
        assert_eq!(empty.active_account, None);
        assert_eq!(empty.active_model, None);

        let sess = s
            .create_session("usage-test", SessionSource::Native)
            .unwrap();

        // Explicit per-protocol savings with saved_tokens = 0 -> computed from specific_sum.
        let id1 = s
            .record_usage(&NewUsageRecord {
                session_id: Some(sess.id.clone()),
                account_id: "acct-a".into(),
                provider: "openai".into(),
                model: "gpt-4o".into(),
                role_kind: "main".into(),
                prompt_tokens: 1000,
                completion_tokens: 250,
                cache_read_tokens: 700,
                cache_write_tokens: 100,
                saved_rtk_tokens: 200,
                saved_ponytail_tokens: 120,
                saved_caveman_tokens: 80,
                saved_tokens: 0,
                status: "ok".into(),
                latency_ms: 320,
                ttft_ms: 90,
            })
            .unwrap();
        // Explicit per-protocol savings with saved_tokens >= specific_sum.
        let id2 = s
            .record_usage(&NewUsageRecord {
                session_id: None,
                account_id: "acct-a".into(),
                provider: "openai".into(),
                model: "gpt-4o".into(),
                role_kind: "subagent".into(),
                prompt_tokens: 500,
                completion_tokens: 100,
                saved_rtk_tokens: 60,
                saved_ponytail_tokens: 50,
                saved_caveman_tokens: 40,
                saved_tokens: 150,
                status: "ok".into(),
                latency_ms: 210,
                ..Default::default()
            })
            .unwrap();
        // Legacy fallback: only saved_tokens populated -> attributed to RTK.
        let id3 = s
            .record_usage(&NewUsageRecord {
                session_id: Some("missing-session-id".into()),
                account_id: "acct-b".into(),
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                role_kind: "main".into(),
                prompt_tokens: 800,
                completion_tokens: 200,
                saved_rtk_tokens: 0,
                saved_ponytail_tokens: 0,
                saved_caveman_tokens: 0,
                saved_tokens: 300,
                status: "rate_limited".into(),
                latency_ms: 50,
                ..Default::default()
            })
            .unwrap();

        assert!(id1 > 0 && id2 > id1 && id3 > id2);

        let future = now_ms() + 60_000;
        s.set_account_cooldown("acct-cd-only", future, Some("429"))
            .unwrap();

        let summary = s.usage_summary(24).unwrap();
        assert_eq!(summary.total_prompt_tokens, 2300);
        assert_eq!(summary.total_completion_tokens, 550);
        assert_eq!(summary.total_cache_read_tokens, 700);
        assert_eq!(summary.total_cache_write_tokens, 100);
        assert_eq!(summary.total_saved_rtk_tokens, 560);
        assert_eq!(summary.total_saved_ponytail_tokens, 170);
        assert_eq!(summary.total_saved_caveman_tokens, 120);
        assert_eq!(summary.total_saved_tokens, 850);
        assert_eq!(
            summary.total_saved_tokens,
            summary.total_saved_rtk_tokens
                + summary.total_saved_ponytail_tokens
                + summary.total_saved_caveman_tokens
        );
        assert_eq!(summary.total_requests, 3);
        assert_eq!(summary.active_account.as_deref(), Some("acct-a"));
        assert_eq!(summary.active_model.as_deref(), Some("gpt-4o"));

        assert_eq!(summary.accounts.len(), 3);
        let acct_a = summary
            .accounts
            .iter()
            .find(|a| a.account_id == "acct-a")
            .unwrap();
        assert_eq!(acct_a.provider, "openai");
        assert_eq!(acct_a.prompt_tokens, 1500);
        assert_eq!(acct_a.completion_tokens, 350);
        assert_eq!(acct_a.total_tokens, 1850);
        assert_eq!(acct_a.cache_read_tokens, 700);
        assert_eq!(acct_a.cache_write_tokens, 100);
        assert_eq!(acct_a.saved_rtk_tokens, 260);
        assert_eq!(acct_a.saved_ponytail_tokens, 170);
        assert_eq!(acct_a.saved_caveman_tokens, 120);
        assert_eq!(acct_a.saved_tokens, 550);
        assert_eq!(acct_a.requests, 2);
        assert_eq!(acct_a.rate_limits, 0);
        assert_eq!(acct_a.last_model.as_deref(), Some("gpt-4o"));
        assert!(acct_a.last_used_at.is_some());

        let acct_b = summary
            .accounts
            .iter()
            .find(|a| a.account_id == "acct-b")
            .unwrap();
        assert_eq!(acct_b.provider, "anthropic");
        assert_eq!(acct_b.total_tokens, 1000);
        assert_eq!(acct_b.saved_rtk_tokens, 300);
        assert_eq!(acct_b.saved_ponytail_tokens, 0);
        assert_eq!(acct_b.saved_caveman_tokens, 0);
        assert_eq!(acct_b.saved_tokens, 300);
        assert_eq!(acct_b.requests, 1);
        assert_eq!(acct_b.rate_limits, 1);
        assert_eq!(acct_b.last_model.as_deref(), Some("claude-sonnet-4"));

        let acct_cd = summary
            .accounts
            .iter()
            .find(|a| a.account_id == "acct-cd-only")
            .unwrap();
        assert_eq!(acct_cd.requests, 0);
        assert_eq!(acct_cd.total_tokens, 0);
        assert_eq!(acct_cd.saved_rtk_tokens, 0);
        assert_eq!(acct_cd.saved_ponytail_tokens, 0);
        assert_eq!(acct_cd.saved_caveman_tokens, 0);
        assert_eq!(acct_cd.saved_tokens, 0);
        assert_eq!(acct_cd.cooldown_until, future);
        assert_eq!(acct_cd.last_error.as_deref(), Some("429"));
        assert_eq!(acct_cd.last_model, None);
        assert_eq!(acct_cd.last_used_at, None);

        assert_eq!(summary.models.len(), 3);
        let main_gpt = summary
            .models
            .iter()
            .find(|m| m.model == "gpt-4o" && m.role_kind == "main")
            .unwrap();
        assert_eq!(main_gpt.total_tokens, 1250);
        assert_eq!(main_gpt.saved_rtk_tokens, 200);
        assert_eq!(main_gpt.saved_ponytail_tokens, 120);
        assert_eq!(main_gpt.saved_caveman_tokens, 80);
        assert_eq!(main_gpt.saved_tokens, 400);
        assert_eq!(main_gpt.requests, 1);

        let sub_gpt = summary
            .models
            .iter()
            .find(|m| m.model == "gpt-4o" && m.role_kind == "subagent")
            .unwrap();
        assert_eq!(sub_gpt.total_tokens, 600);
        assert_eq!(sub_gpt.saved_rtk_tokens, 60);
        assert_eq!(sub_gpt.saved_ponytail_tokens, 50);
        assert_eq!(sub_gpt.saved_caveman_tokens, 40);
        assert_eq!(sub_gpt.saved_tokens, 150);
        assert_eq!(sub_gpt.requests, 1);

        let main_claude = summary
            .models
            .iter()
            .find(|m| m.model == "claude-sonnet-4" && m.role_kind == "main")
            .unwrap();
        assert_eq!(main_claude.saved_rtk_tokens, 300);
        assert_eq!(main_claude.saved_ponytail_tokens, 0);
        assert_eq!(main_claude.saved_caveman_tokens, 0);
        assert_eq!(main_claude.saved_tokens, 300);

        // Deleting the session keeps usage rows with session_id set to NULL.
        s.conn()
            .execute("DELETE FROM sessions WHERE id = ?1", params![sess.id])
            .unwrap();
        assert_eq!(s.usage_summary(24).unwrap().total_requests, 3);
    }

    #[test]
    fn record_usage_preserves_explicit_total_when_higher_than_protocol_sum() {
        let s = store();
        s.record_usage(&NewUsageRecord {
            account_id: "acct-max".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            saved_rtk_tokens: 10,
            saved_ponytail_tokens: 20,
            saved_caveman_tokens: 30,
            saved_tokens: 100,
            ..Default::default()
        })
        .unwrap();

        let stats = s.account_window_stats("acct-max", 24).unwrap();
        assert_eq!(stats.saved_rtk_tokens, 10);
        assert_eq!(stats.saved_ponytail_tokens, 20);
        assert_eq!(stats.saved_caveman_tokens, 30);
        assert_eq!(stats.saved_tokens, 100);
    }

    #[test]
    fn active_account_falls_back_to_most_recent_when_no_ok_record_exists() {
        let s = store();
        s.record_usage(&NewUsageRecord {
            account_id: "acct-err".into(),
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            status: "error".into(),
            ..Default::default()
        })
        .unwrap();
        s.record_usage(&NewUsageRecord {
            account_id: "acct-rl".into(),
            provider: "anthropic".into(),
            model: "claude-haiku".into(),
            status: "rate_limited".into(),
            ..Default::default()
        })
        .unwrap();

        let summary = s.usage_summary(24).unwrap();
        assert_eq!(summary.active_account.as_deref(), Some("acct-rl"));
        assert_eq!(summary.active_model.as_deref(), Some("claude-haiku"));
    }

    #[test]
    fn is_account_available_respects_quotas_and_rolling_window() {
        let s = store();
        assert!(s
            .is_account_available("fresh-acct", 24, Some(100), Some(1))
            .unwrap());

        let old_id = s
            .record_usage(&NewUsageRecord {
                account_id: "acct-w".into(),
                provider: "openai".into(),
                model: "gpt-4o".into(),
                prompt_tokens: 900,
                completion_tokens: 100,
                saved_tokens: 500,
                ..Default::default()
            })
            .unwrap();
        let three_hours_ago = now_ms() - 3 * MS_PER_HOUR;
        s.conn()
            .execute(
                "UPDATE usage_records SET created_at = ?1 WHERE id = ?2",
                params![three_hours_ago, old_id],
            )
            .unwrap();

        // Outside the 1-hour window, old usage does not block quotas.
        let stats_1h = s.account_window_stats("acct-w", 1).unwrap();
        assert_eq!(stats_1h.requests, 0);
        assert_eq!(stats_1h.total_tokens, 0);
        assert_eq!(stats_1h.last_used_at, Some(three_hours_ago));
        assert!(s
            .is_account_available("acct-w", 1, Some(500), Some(1))
            .unwrap());

        // Inside the 6-hour window (or all-time window 0), quotas block at exact boundary.
        assert!(!s
            .is_account_available("acct-w", 6, Some(1000), None)
            .unwrap());
        assert!(!s.is_account_available("acct-w", 0, None, Some(1)).unwrap());
        assert!(s
            .is_account_available("acct-w", 6, Some(1001), Some(2))
            .unwrap());

        // Add fresh in-window usage; saved_tokens must not count against total_tokens quota.
        s.record_usage(&NewUsageRecord {
            account_id: "acct-w".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            prompt_tokens: 200,
            completion_tokens: 50,
            saved_rtk_tokens: 2000,
            saved_ponytail_tokens: 1500,
            saved_caveman_tokens: 1500,
            ..Default::default()
        })
        .unwrap();

        assert!(s
            .is_account_available("acct-w", 1, Some(1000), Some(2))
            .unwrap());
        assert!(!s
            .is_account_available("acct-w", 1, Some(250), None)
            .unwrap());
        assert!(!s.is_account_available("acct-w", 1, None, Some(1)).unwrap());
        assert!(s
            .is_account_available("acct-w", 1, Some(251), Some(2))
            .unwrap());
    }

    #[test]
    fn set_and_clear_account_cooldown_control_availability() {
        let s = store();
        let future = now_ms() + 60_000;
        s.set_account_cooldown("acct-c", future, Some("429 rate limit"))
            .unwrap();
        assert!(!s.is_account_available("acct-c", 24, None, None).unwrap());
        let stats = s.account_window_stats("acct-c", 24).unwrap();
        assert_eq!(stats.cooldown_until, future);
        assert_eq!(stats.last_error.as_deref(), Some("429 rate limit"));

        // Expired cooldown in the past allows requests again.
        let past = now_ms() - 1_000;
        s.set_account_cooldown("acct-c", past, Some("old error"))
            .unwrap();
        assert!(s.is_account_available("acct-c", 24, None, None).unwrap());
        let stats = s.account_window_stats("acct-c", 24).unwrap();
        assert_eq!(stats.cooldown_until, past);
        assert_eq!(stats.last_error.as_deref(), Some("old error"));

        // Active cooldown is cleared immediately by clear_account_cooldown.
        s.set_account_cooldown("acct-c", now_ms() + 60_000, Some("quota exceeded"))
            .unwrap();
        assert!(!s.is_account_available("acct-c", 24, None, None).unwrap());

        s.clear_account_cooldown("acct-c").unwrap();
        assert!(s.is_account_available("acct-c", 24, None, None).unwrap());
        let stats = s.account_window_stats("acct-c", 24).unwrap();
        assert_eq!(stats.cooldown_until, 0);
        assert_eq!(stats.last_error, None);
    }

    #[test]
    fn session_usage_aggregates_per_session_and_defaults_to_zero() {
        let s = Store::open_in_memory().unwrap();
        let empty = s.session_usage("missing").unwrap();
        assert_eq!(empty, SessionUsageTotals::default());

        let sess = s
            .create_session("usage-test", crate::SessionSource::Native)
            .unwrap();
        let other = s
            .create_session("other-test", crate::SessionSource::Native)
            .unwrap();
        s.record_usage(&NewUsageRecord {
            session_id: Some(sess.id.clone()),
            account_id: "acct".into(),
            provider: "openai".into(),
            model: "gpt-5".into(),
            role_kind: "main".into(),
            prompt_tokens: 1000,
            completion_tokens: 250,
            cache_read_tokens: 600,
            cache_write_tokens: 100,
            saved_rtk_tokens: 120,
            saved_tokens: 150,
            ..Default::default()
        })
        .unwrap();
        s.record_usage(&NewUsageRecord {
            session_id: Some(sess.id.clone()),
            provider: "openai".into(),
            model: "gpt-5".into(),
            prompt_tokens: 500,
            completion_tokens: 100,
            cache_read_tokens: 200,
            saved_tokens: 50,
            ..Default::default()
        })
        .unwrap();
        s.record_usage(&NewUsageRecord {
            session_id: Some(other.id.clone()),
            provider: "openai".into(),
            model: "gpt-5".into(),
            prompt_tokens: 9999,
            ..Default::default()
        })
        .unwrap();

        let totals = s.session_usage(&sess.id).unwrap();
        assert_eq!(totals.prompt_tokens, 1500);
        assert_eq!(totals.completion_tokens, 350);
        assert_eq!(totals.cache_read_tokens, 800);
        assert_eq!(totals.cache_write_tokens, 100);
        assert_eq!(totals.saved_tokens, 200);
        assert_eq!(totals.saved_rtk_tokens, 170);
        assert_eq!(totals.requests, 2);
    }

    #[test]
    fn usage_cost_rows_group_by_day_session_and_model_including_subagents() {
        let s = Store::open_in_memory().unwrap();
        let sess = s
            .create_session("principal", crate::SessionSource::Native)
            .unwrap();
        let child = s
            .create_session("filho", crate::SessionSource::Native)
            .unwrap();
        s.conn()
            .execute(
                "UPDATE sessions SET source = 'subagent', source_id = ?1 WHERE id = ?2",
                params![sess.id, child.id],
            )
            .unwrap();
        let rec = |sid: &str, model: &str, p: u64| NewUsageRecord {
            session_id: Some(sid.into()),
            account_id: "acc".into(),
            provider: "anthropic".into(),
            model: model.into(),
            prompt_tokens: p,
            completion_tokens: 10,
            cache_read_tokens: 5,
            ..Default::default()
        };
        s.record_usage(&rec(&sess.id, "m1", 100)).unwrap();
        s.record_usage(&rec(&sess.id, "m1", 200)).unwrap();
        s.record_usage(&rec(&child.id, "m2", 50)).unwrap();
        s.record_usage(&NewUsageRecord {
            model: "m3".into(),
            ..rec("", "", 7)
        })
        .unwrap();

        let all = s.usage_cost_rows(0, None).unwrap();
        assert_eq!(all.len(), 3);
        let m1 = all.iter().find(|r| r.model == "m1").unwrap();
        assert_eq!(
            (m1.prompt_tokens, m1.completion_tokens, m1.cache_read_tokens),
            (300, 20, 10)
        );
        assert_eq!(m1.requests, 2);
        assert_eq!(m1.session_title.as_deref(), Some("principal"));
        assert_eq!(m1.day.len(), 10);
        assert!(all
            .iter()
            .any(|r| r.model == "m3" && r.session_id.is_none()));

        let mine = s.usage_cost_rows(24, Some(&sess.id)).unwrap();
        let models: Vec<&str> = mine.iter().map(|r| r.model.as_str()).collect();
        assert_eq!(models, ["m1", "m2"]);
    }
}
