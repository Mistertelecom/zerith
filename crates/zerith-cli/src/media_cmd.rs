//! `zerith media ...`: pick the image/video model, store the OpenRouter key and
//! try the pipeline without the chat. Output is pt-BR; the key is never echoed.

use std::collections::BTreeMap;
use std::io::IsTerminal;

use serde_json::Value;
use zerith_core::{secrets, Paths, Settings};
use zerith_providers::media::{
    ImageRequest, Kind, MediaError, MediaModel, OpenRouter, ParamKind, VideoRequest,
};
use zerith_providers::Cancel;

type Res = Result<(), Box<dyn std::error::Error>>;

pub const USAGE: &str = "\
  zerith media show
  zerith media models image|video [--query Q] [--limit N] [--offset N]
  zerith media set image|video --model ID [--param k=v ...] [--clear-params]
  zerith media key set [valor] | check | delete
  zerith media generate image|video \"<prompt>\" [--ref <imagem>] [--first-frame <imagem>] [--last-frame <imagem>]";

pub fn run(paths: &Paths, args: &[String]) -> Res {
    match args.get(1).map(String::as_str) {
        None | Some("show") => show(paths),
        Some("models") => models(paths, args),
        Some("set") => set(paths, args),
        Some("key") => key(paths, args),
        Some("generate") => generate(paths, args),
        Some(other) => Err(format!("subcomando desconhecido: media {other}\n{USAGE}").into()),
    }
}

fn kind_arg(args: &[String]) -> Result<Kind, Box<dyn std::error::Error>> {
    args.get(2)
        .and_then(|s| Kind::parse(s))
        .ok_or_else(|| format!("informe image ou video\n{USAGE}").into())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

fn settings(paths: &Paths) -> Settings {
    let cwd = std::env::current_dir().ok();
    let loaded = Settings::load(paths, cwd.as_deref());
    for e in &loaded.errors {
        eprintln!("zerith: {e}");
    }
    loaded.settings
}

fn base_url(s: &Settings) -> Option<String> {
    Some(s.media.base_url.clone()).filter(|b| !b.trim().is_empty())
}

fn client(s: &Settings, with_key: bool) -> Result<OpenRouter, Box<dyn std::error::Error>> {
    let key = if with_key {
        secrets::resolve_openrouter_key()?
    } else {
        None
    };
    Ok(OpenRouter::new(key, base_url(s)))
}

/// `true`/`false`, integers and decimals keep their type; anything else is text.
pub fn parse_value(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => raw
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| raw.parse::<f64>().map(Value::from))
            .unwrap_or_else(|_| Value::String(raw.to_string())),
    }
}

fn show(paths: &Paths) -> Res {
    let s = settings(paths);
    let m = &s.media;
    println!("pasta de saída  {}", m.resolve_output_dir(paths).display());
    println!(
        "endpoint        {}",
        base_url(&s).unwrap_or_else(|| "https://openrouter.ai/api/v1 (padrão)".into())
    );
    for (name, slot) in [("imagem", &m.image), ("vídeo", &m.video)] {
        if !slot.configured() {
            println!("{name:<15} (nenhum modelo escolhido; a ferramenta não é oferecida à IA)");
            continue;
        }
        println!("{name:<15} {}", slot.model);
        for (k, v) in &slot.params {
            println!("                  {k} = {v}");
        }
    }
    let key = match secrets::resolve_openrouter_key() {
        Ok(Some(k)) => format!("configurada ({})", secrets::mask(&k)),
        Ok(None) => "não configurada (zerith media key set)".into(),
        Err(e) => format!("indisponível: {e}"),
    };
    println!("chave           {key}");
    Ok(())
}

fn models(paths: &Paths, args: &[String]) -> Res {
    let kind = kind_arg(args)?;
    let query = flag(args, "--query").unwrap_or_default();
    // All of them by default: the lists are a few dozen, newest first.
    let limit = flag(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let offset = flag(args, "--offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let page = client(&settings(paths), false)?
        .models(kind, &query, offset, limit)
        .map_err(|e| e.to_string())?;
    for m in &page.models {
        print_model(m);
    }
    println!(
        "{} de {} modelo(s){}",
        page.models.len(),
        page.total,
        if offset + page.models.len() < page.total {
            format!(
                "; use --offset {} para ver mais",
                offset + page.models.len()
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

fn print_model(m: &MediaModel) {
    println!(
        "{}  {}{}",
        m.id,
        m.name,
        if m.price_hint.is_empty() {
            String::new()
        } else {
            format!("  [{}]", m.price_hint)
        }
    );
    for p in &m.params {
        let what = match &p.kind {
            ParamKind::Enum(v) => v.join(" | "),
            ParamKind::Range { min, max } => format!("{min}..{max}"),
            ParamKind::Flag => "true | false".into(),
            ParamKind::Text => "livre".into(),
            ParamKind::Other => "?".into(),
        };
        println!("    {:<14} {what}", p.name);
    }
}

fn set(paths: &Paths, args: &[String]) -> Res {
    let kind = kind_arg(args)?;
    let model = flag(args, "--model");
    let mut params: BTreeMap<String, Value> = BTreeMap::new();
    for kv in flags(args, "--param") {
        let (k, v) = kv
            .split_once('=')
            .filter(|(k, _)| !k.trim().is_empty())
            .ok_or_else(|| format!("--param precisa ser chave=valor, recebi '{kv}'"))?;
        params.insert(k.trim().to_string(), parse_value(v.trim()));
    }
    let clear = args.iter().any(|a| a == "--clear-params");
    if model.is_none() && params.is_empty() && !clear {
        return Err(format!("nada para alterar\n{USAGE}").into());
    }

    // Validate against the public model list when it is reachable. A wrong
    // value is refused now, not on the first paid call; offline only warns.
    let current = settings(paths);
    let slot = match kind {
        Kind::Image => &current.media.image,
        Kind::Video => &current.media.video,
    };
    let final_model = model.clone().unwrap_or_else(|| slot.model.clone());
    let final_params = if clear || !params.is_empty() {
        params.clone()
    } else {
        slot.params.clone()
    };
    if final_model.is_empty() {
        return Err("escolha um modelo com --model".into());
    }
    let or = client(&current, false)?;
    let planned = match kind {
        Kind::Image => or.plan_image(&ImageRequest {
            model: final_model.clone(),
            params: final_params.clone(),
            ..Default::default()
        }),
        Kind::Video => or.plan_video(&VideoRequest {
            model: final_model.clone(),
            params: final_params.clone(),
            ..Default::default()
        }),
    };
    match planned {
        Ok(plan) => {
            for n in plan.notes {
                eprintln!("aviso: {n}");
            }
        }
        Err(MediaError::Transport(why)) => {
            eprintln!("aviso: não consegui validar contra a lista de modelos ({why}); gravando sem validar")
        }
        Err(e) => return Err(e.to_string().into()),
    }

    Settings::save_user(paths, |s| {
        let slot = match kind {
            Kind::Image => &mut s.media.image,
            Kind::Video => &mut s.media.video,
        };
        if let Some(m) = &model {
            slot.model = m.clone();
        }
        // `--param` states the full set; `--clear-params` alone empties it.
        if clear || !params.is_empty() {
            slot.params = params.clone();
        }
    })?;
    println!(
        "{} configurado: {final_model}{}",
        if kind == Kind::Image {
            "imagem"
        } else {
            "vídeo"
        },
        if final_params.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                final_params
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    Ok(())
}

fn key(paths: &Paths, args: &[String]) -> Res {
    match args.get(2).map(String::as_str) {
        Some("set") => {
            let value = match args.get(3) {
                Some(v) => v.trim().to_string(),
                None => {
                    if std::io::stdin().is_terminal() {
                        eprintln!("Cole a chave do OpenRouter (https://openrouter.ai/settings/keys) e tecle Enter:");
                    }
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };
            if value.is_empty() {
                return Err("chave vazia; nada gravado".into());
            }
            secrets::set_secret(secrets::OPENROUTER_ENTRY, &value)?;
            println!(
                "chave do OpenRouter gravada no cofre do sistema ({})",
                secrets::mask(&value)
            );
            Ok(())
        }
        Some("delete") => {
            secrets::delete_secret(secrets::OPENROUTER_ENTRY)?;
            println!("chave do OpenRouter removida do cofre");
            Ok(())
        }
        Some("check") => {
            let s = settings(paths);
            let or = client(&s, true)?;
            let info = or.check_key().map_err(|e| e.to_string())?;
            println!("chave válida: {}", info.label);
            match (info.usage_usd, info.limit_usd) {
                (Some(u), Some(l)) => println!("uso US$ {u:.4} de US$ {l:.2}"),
                (Some(u), None) => println!("uso US$ {u:.4} (sem limite definido)"),
                _ => println!("uso não informado"),
            }
            Ok(())
        }
        _ => Err(format!("usage:\n{USAGE}").into()),
    }
}

fn generate(paths: &Paths, args: &[String]) -> Res {
    let kind = kind_arg(args)?;
    let prompt = args
        .get(3)
        .filter(|p| !p.starts_with("--"))
        .ok_or("informe o prompt entre aspas")?
        .clone();
    let s = settings(paths);
    let slot = match kind {
        Kind::Image => &s.media.image,
        Kind::Video => &s.media.video,
    };
    if !slot.configured() {
        return Err(format!(
            "nenhum modelo de {} escolhido; rode `zerith media set {} --model ID`",
            kind.as_str(),
            kind.as_str()
        )
        .into());
    }
    let out_dir = s.media.resolve_output_dir(paths).join("cli");
    let or = client(&s, true)?;
    let cancel = Cancel::default();
    eprintln!(
        "gerando {} com {} — isto gera custo no OpenRouter",
        kind.as_str(),
        slot.model
    );
    let files = match kind {
        Kind::Image => {
            let req = ImageRequest {
                model: slot.model.clone(),
                prompt,
                params: slot.params.clone(),
                reference_images: flags(args, "--ref"),
                filename_hint: None,
            };
            for n in or.plan_image(&req).map_err(|e| e.to_string())?.notes {
                eprintln!("aviso: {n}");
            }
            or.generate_image(&req, &out_dir, &cancel)
                .map_err(|e| e.to_string())?
        }
        Kind::Video => {
            let req = VideoRequest {
                model: slot.model.clone(),
                prompt,
                params: slot.params.clone(),
                reference_images: flags(args, "--ref"),
                first_frame: flag(args, "--first-frame"),
                last_frame: flag(args, "--last-frame"),
                filename_hint: None,
            };
            for n in or.plan_video(&req).map_err(|e| e.to_string())?.notes {
                eprintln!("aviso: {n}");
            }
            vec![or
                .generate_video(&req, &out_dir, &cancel, &mut |p| eprintln!("  {p}"))
                .map_err(|e| e.to_string())?]
        }
    };
    for f in &files {
        println!(
            "{}  ({}, {} KB){}",
            f.path.display(),
            f.media_type,
            f.bytes.div_ceil(1024),
            f.cost_usd
                .map(|c| format!("  custo US$ {c:.4}"))
                .unwrap_or_else(|| "  custo não informado".into())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_keep_their_type() {
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("5"), Value::from(5));
        assert_eq!(parse_value("0.5"), Value::from(0.5));
        assert_eq!(parse_value("16:9"), Value::String("16:9".into()));
        assert_eq!(parse_value("2K"), Value::String("2K".into()));
        assert_eq!(parse_value("720p"), Value::String("720p".into()));
    }

    #[test]
    fn repeated_flags_are_all_read() {
        let a: Vec<String> = ["media", "set", "image", "--param", "a=1", "--param", "b=2"]
            .map(String::from)
            .to_vec();
        assert_eq!(flags(&a, "--param"), ["a=1", "b=2"]);
        assert_eq!(flag(&a, "--nope"), None);
        assert_eq!(kind_arg(&a).unwrap(), Kind::Image);
    }
}
