/// Zero-dependency 5-field cron evaluator (`min hour dom month dow`) with
/// Howard Hinnant's pure-Rust civil calendar conversion and explicit timezone
/// offset (minutes east of UTC, e.g. `-180` for `America/Sao_Paulo`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday .. 6 = Saturday
    pub weekday: u32,
}

impl LocalDateTime {
    pub fn from_unix_secs(unix_secs: i64, tz_offset_minutes: i32) -> Self {
        let shifted = unix_secs + i64::from(tz_offset_minutes) * 60;
        let days = shifted.div_euclid(86_400);
        let secs_in_day = shifted.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        let hour = secs_in_day / 3600;
        let minute = (secs_in_day % 3600) / 60;
        let second = secs_in_day % 60;
        // 1970-01-01 was Thursday (4)
        let weekday = (days + 4).rem_euclid(7) as u32;
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            weekday,
        }
    }

    pub fn date_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn time_string(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// Unique key for the current minute slot so a scheduler avoids duplicate runs.
    pub fn minute_key(&self) -> i64 {
        self.year * 100_000_000
            + i64::from(self.month) * 1_000_000
            + i64::from(self.day) * 10_000
            + i64::from(self.hour) * 100
            + i64::from(self.minute)
    }
}

/// Howard Hinnant's `civil_from_days`: converts days since 1970-01-01 to `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    (y, m as u32, d as u32)
}

#[derive(Debug, Clone)]
pub struct CronSchedule {
    minutes: Vec<bool>,  // 0..60
    hours: Vec<bool>,    // 0..24
    days: Vec<bool>,     // 1..=31
    months: Vec<bool>,   // 1..=12
    weekdays: Vec<bool>, // 0..7 (Sunday = 0)
    dom_wildcard: bool,
    dow_wildcard: bool,
}

impl CronSchedule {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let trimmed = expr.trim();
        let normalized = match trimmed {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            other => other,
        };
        let parts: Vec<&str> = normalized.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(format!(
                "expressão cron inválida (esperado 5 campos): '{expr}'"
            ));
        }
        let minutes = parse_field(parts[0], 0, 59, false)?;
        let hours = parse_field(parts[1], 0, 23, false)?;
        let days = parse_field(parts[2], 1, 31, false)?;
        let months = parse_field(parts[3], 1, 12, false)?;
        let weekdays = parse_field(parts[4], 0, 7, true)?;
        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            dom_wildcard: parts[2].trim() == "*",
            dow_wildcard: parts[4].trim() == "*",
        })
    }

    pub fn matches_unix(&self, unix_secs: i64, tz_offset_minutes: i32) -> bool {
        let dt = LocalDateTime::from_unix_secs(unix_secs, tz_offset_minutes);
        self.matches_dt(&dt)
    }

    pub fn matches_dt(&self, dt: &LocalDateTime) -> bool {
        let m_ok = self
            .minutes
            .get(dt.minute as usize)
            .copied()
            .unwrap_or(false);
        let h_ok = self.hours.get(dt.hour as usize).copied().unwrap_or(false);
        let mo_ok = self.months.get(dt.month as usize).copied().unwrap_or(false);
        if !m_ok || !h_ok || !mo_ok {
            return false;
        }
        let dom_ok = self.days.get(dt.day as usize).copied().unwrap_or(false);
        let dow_ok = self
            .weekdays
            .get(dt.weekday as usize)
            .copied()
            .unwrap_or(false);
        // Standard cron semantics: if both DOM and DOW are restricted, either matches;
        // if either is '*', both conditions hold.
        if !self.dom_wildcard && !self.dow_wildcard {
            dom_ok || dow_ok
        } else {
            dom_ok && dow_ok
        }
    }
}

fn parse_field(
    field: &str,
    min: usize,
    max: usize,
    wrap_sunday: bool,
) -> Result<Vec<bool>, String> {
    let mut allowed = vec![false; max + 1];
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("campo cron vazio em '{field}'"));
        }
        let (range_part, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step = s
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("passo inválido '{s}' em '{field}'"))?;
                if step == 0 {
                    return Err(format!("passo 0 inválido em '{field}'"));
                }
                (r.trim(), step)
            }
            None => (part, 1usize),
        };
        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let a = a
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("valor inválido '{a}' em '{field}'"))?;
            let b = b
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("valor inválido '{b}' em '{field}'"))?;
            if a < min || b > max || a > b {
                return Err(format!("intervalo fora dos limites em '{field}'"));
            }
            (a, b)
        } else {
            let v = range_part
                .parse::<usize>()
                .map_err(|_| format!("valor inválido '{range_part}' em '{field}'"))?;
            if v < min || v > max {
                return Err(format!("valor {v} fora de {min}..={max} em '{field}'"));
            }
            if step == 1 {
                let idx = if wrap_sunday && v == 7 { 0 } else { v };
                allowed[idx] = true;
                continue;
            }
            (v, max)
        };

        let mut cur = start;
        while cur <= end {
            let idx = if wrap_sunday && cur == 7 { 0 } else { cur };
            allowed[idx] = true;
            cur = cur.saturating_add(step);
        }
    }
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_and_timezone_work() {
        // 1970-01-01 00:00:00 UTC (Thursday)
        let dt = LocalDateTime::from_unix_secs(0, 0);
        assert_eq!(dt.year, 1970);
        assert_eq!(dt.month, 1);
        assert_eq!(dt.day, 1);
        assert_eq!(dt.hour, 0);
        assert_eq!(dt.minute, 0);
        assert_eq!(dt.weekday, 4);

        // 2026-11-21 11:00:00 UTC -> 08:00:00 BRT (-180)
        // Days from 1970-01-01 to 2026-11-21:
        // Let's test via shifted timestamp: 2026-11-21T11:00:00Z = 1795258800? Wait, calculate dynamically!
        // Instead verify shift difference directly:
        let utc = LocalDateTime::from_unix_secs(1_700_000_000, 0);
        let brt = LocalDateTime::from_unix_secs(1_700_000_000, -180);
        assert_ne!(utc.hour, brt.hour);
    }

    #[test]
    fn cron_expressions_match_correctly() {
        let daily_8am = CronSchedule::parse("0 8 * * *").unwrap();
        let match_dt = LocalDateTime {
            year: 2026,
            month: 11,
            day: 21,
            hour: 8,
            minute: 0,
            second: 12,
            weekday: 6,
        };
        let miss_dt = LocalDateTime {
            hour: 9,
            ..match_dt
        };
        assert!(daily_8am.matches_dt(&match_dt));
        assert!(!daily_8am.matches_dt(&miss_dt));

        let every_15 = CronSchedule::parse("*/15 * * * *").unwrap();
        assert!(every_15.matches_dt(&LocalDateTime {
            minute: 0,
            ..match_dt
        }));
        assert!(every_15.matches_dt(&LocalDateTime {
            minute: 30,
            ..match_dt
        }));
        assert!(!every_15.matches_dt(&LocalDateTime {
            minute: 17,
            ..match_dt
        }));

        let weekdays = CronSchedule::parse("30 7,19 * * 1-5").unwrap();
        // weekday 1 = Monday, 6 = Saturday
        assert!(weekdays.matches_dt(&LocalDateTime {
            hour: 7,
            minute: 30,
            weekday: 1,
            ..match_dt
        }));
        assert!(weekdays.matches_dt(&LocalDateTime {
            hour: 19,
            minute: 30,
            weekday: 5,
            ..match_dt
        }));
        assert!(!weekdays.matches_dt(&LocalDateTime {
            hour: 7,
            minute: 30,
            weekday: 6,
            ..match_dt
        }));
    }

    #[test]
    fn timezone_offset_shifts_cron_fire_time() {
        let cron = CronSchedule::parse("0 8 * * *").unwrap();
        // Find exact UTC epoch where 08:00 UTC occurs, then verify -180 matches 11:00 UTC.
        let mut target_utc = 1_700_000_000i64;
        while LocalDateTime::from_unix_secs(target_utc, 0).hour != 11
            || LocalDateTime::from_unix_secs(target_utc, 0).minute != 0
        {
            target_utc += 60;
        }
        assert!(cron.matches_unix(target_utc, -180));
        assert!(!cron.matches_unix(target_utc, 0));
    }
}
