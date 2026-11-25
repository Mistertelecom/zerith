//! Transform `offline_report`: lista de equipamentos fora do ar pronta para o
//! canal. Entrada: saída de `print detail` do RouterOS (The Dude) ou o JSON
//! normalizado do nó `zabbix` (`[{host, ip, problema, severidade, desde}]`).

use serde_json::Value;

/// Um registro de `print detail`: pares `chave=valor` (valor entre aspas ou
/// até o próximo espaço). Registros começam com o índice (`  0  name=...`);
/// linhas recuadas continuam o registro anterior; o resto (`Flags:`) é ignorado.
pub(crate) fn parse_routeros_detail(text: &str) -> Vec<Vec<(String, String)>> {
    let mut records: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let starts_record = t.split_whitespace().next().is_some_and(|w| {
            let w = w.trim_end_matches(';');
            !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())
        });
        if starts_record {
            records.push(t.to_string());
        } else if line.starts_with(char::is_whitespace) && !t.is_empty() {
            if let Some(last) = records.last_mut() {
                last.push(' ');
                last.push_str(t);
            }
        }
    }
    records.iter().map(|r| pairs(r)).collect()
}

fn pairs(rec: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = rec.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let Some(eq) = rec[i..].find('=').map(|p| p + i) else {
            break;
        };
        let key_start = rec[..eq].rfind(char::is_whitespace).map_or(0, |p| p + 1);
        let key = rec[key_start.max(i)..eq].to_string();
        let mut j = eq + 1;
        let val = if b.get(j) == Some(&b'"') {
            j += 1;
            let mut v = String::new();
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' && j + 1 < b.len() {
                    j += 1;
                }
                let ch = rec[j..].chars().next().unwrap_or(' ');
                v.push(ch);
                j += ch.len_utf8();
            }
            j += 1;
            v
        } else {
            let end = rec[j..]
                .find(char::is_whitespace)
                .map_or(b.len(), |p| p + j);
            let v = rec[j..end].to_string();
            j = end;
            v
        };
        if !key.is_empty() {
            out.push((key, val));
        }
        i = j.min(b.len());
    }
    out
}

fn field<'a>(rec: &'a [(String, String)], keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| {
        rec.iter()
            .find(|(rk, v)| rk.eq_ignore_ascii_case(k) && !v.trim().is_empty())
            .map(|(_, v)| v.trim())
    })
}

/// "há 15min" / "há 3h" / "há 2d".
fn ago(now_unix: i64, since: i64) -> String {
    let m = (now_unix - since).max(0) / 60;
    match m {
        0..=59 => format!("{m}min"),
        60..=2879 => format!("{}h", m / 60),
        _ => format!("{}d", m / 1440),
    }
}

/// Opções do modo (config do nó): `status_field` (padrão `status`),
/// `down_values` (vírgulas, padrão `down`), `title` e `empty_text`.
pub(crate) struct ReportOpts<'a> {
    pub status_field: &'a str,
    pub down_values: &'a str,
    pub title: &'a str,
    pub empty_text: &'a str,
}

impl Default for ReportOpts<'_> {
    fn default() -> Self {
        Self {
            status_field: "status",
            down_values: "down",
            title: "Equipamentos offline",
            empty_text: "Nenhum equipamento offline",
        }
    }
}

/// Monta "Equipamentos offline às HH:MM (N):\n- nome (ip) …" ou
/// "Nenhum equipamento offline às HH:MM".
pub(crate) fn offline_report(input: &str, now_unix: i64, hhmm: &str, o: &ReportOpts) -> String {
    let items: Vec<String> = match serde_json::from_str::<Value>(input.trim()) {
        Ok(Value::Array(arr)) => arr.iter().map(|v| zabbix_line(v, now_unix)).collect(),
        _ => {
            let downs: Vec<String> = o
                .down_values
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            parse_routeros_detail(input)
                .iter()
                .filter(|r| {
                    field(r, &[o.status_field])
                        .is_some_and(|s| downs.contains(&s.to_ascii_lowercase()))
                })
                .map(|r| {
                    let name = field(r, &["name"]).unwrap_or("?");
                    match field(r, &["address", "addresses", "ip"]) {
                        Some(a) => format!("{name} ({a})"),
                        None => name.to_string(),
                    }
                })
                .collect()
        }
    };
    if items.is_empty() {
        return format!("{} às {hhmm}", o.empty_text);
    }
    let mut out = format!("{} às {hhmm} ({}):", o.title, items.len());
    for it in items {
        out.push_str("\n- ");
        out.push_str(&it);
    }
    out
}

fn zabbix_line(v: &Value, now_unix: i64) -> String {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let mut line = s("host").to_string();
    if line.is_empty() {
        line.push('?');
    }
    if !s("ip").is_empty() {
        line.push_str(&format!(" ({})", s("ip")));
    }
    if !s("problema").is_empty() {
        line.push_str(&format!(" — {}", s("problema")));
    }
    let since = v.get("desde").and_then(|d| {
        d.as_i64()
            .or_else(|| d.as_str().and_then(|s| s.parse().ok()))
    });
    if let Some(t) = since.filter(|t| *t > 0) {
        line.push_str(&format!(" há {}", ago(now_unix, t)));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exemplo ilustrativo (não é saída capturada de um equipamento real).
    const DUDE_EXEMPLO: &str = include_str!("fixtures/exemplo-dude-device-print-detail.txt");

    #[test]
    fn dude_print_detail_lista_so_os_down() {
        let recs = parse_routeros_detail(DUDE_EXEMPLO);
        assert_eq!(recs.len(), 4);
        assert_eq!(field(&recs[1], &["name"]), Some("Switch Bloco B"));
        let out = offline_report(DUDE_EXEMPLO, 0, "07:50", &ReportOpts::default());
        assert_eq!(
            out,
            "Equipamentos offline às 07:50 (2):\n- Switch Bloco B (198.18.1.20)\n- radio-torre-3 (198.18.1.31)"
        );
        let all_up = DUDE_EXEMPLO.replace("status=down", "status=up");
        assert_eq!(
            offline_report(&all_up, 0, "07:50", &ReportOpts::default()),
            "Nenhum equipamento offline às 07:50"
        );
        assert_eq!(
            offline_report("", 0, "08:00", &ReportOpts::default()),
            "Nenhum equipamento offline às 08:00"
        );
    }

    #[test]
    fn json_do_zabbix_vira_linhas_com_tempo() {
        let now = 1_795_580_000;
        let input = r#"[{"host":"olt-01","ip":"198.18.2.1","problema":"Unavailable by ICMP ping","severidade":"4","desde":1795567712},
                        {"host":"sw-02","ip":"","problema":"Zabbix agent is not available","severidade":"3","desde":"1795579100"}]"#;
        let out = offline_report(input, now, "07:50", &ReportOpts::default());
        assert_eq!(
            out,
            "Equipamentos offline às 07:50 (2):\n- olt-01 (198.18.2.1) — Unavailable by ICMP ping há 3h\n- sw-02 — Zabbix agent is not available há 15min"
        );
        assert_eq!(
            offline_report("[]", now, "07:50", &ReportOpts::default()),
            "Nenhum equipamento offline às 07:50"
        );
    }
}
