use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use zerith_providers::Cancel;
use zerith_security::{preset, Mode, Policy};

fn scratch(name: &str) -> PathBuf {
    // Under target/, like the other unit tests: never /tmp or %APPDATA%.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch")
        .join(format!("media-tool-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One-shot-per-connection fake: answers the typed list, `POST /images`, and
/// records every request body.
fn fake_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { break };
            let seen = seen2.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&buf[..i]).into_owned(),
                            buf[i + 4..].to_vec(),
                        );
                    }
                };
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while body.len() < len {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..n]);
                }
                let line = head.lines().next().unwrap_or("").to_string();
                seen.lock()
                    .unwrap()
                    .push(format!("{line} {}", String::from_utf8_lossy(&body)));
                let out = if line.starts_with("GET /api/v1/images/models") {
                    json!({"data": [{"id": "acme/pic-1", "name": "Pic", "created": 1,
                        "supported_parameters": {
                            "aspect_ratio": {"type": "enum", "values": ["1:1", "16:9"]},
                            "n": {"type": "range", "min": 1, "max": 4}}}]})
                } else if line.starts_with("GET /api/v1/models") {
                    json!({"total_count": 0, "data": []})
                } else {
                    json!({"data": [{"b64_json": "iVBORw0KGgo=", "media_type": "image/png"}],
                        "usage": {"cost": 0.04}})
                }
                .to_string();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
            });
        }
    });
    (base, seen)
}

fn settings(base: &str, root: &Path) -> (Settings, Paths) {
    let mut s = Settings::default();
    s.media.base_url = base.into();
    s.media.output_dir = root.join("out").to_string_lossy().into_owned();
    s.media.image.model = "acme/pic-1".into();
    s.media
        .image
        .params
        .insert("aspect_ratio".into(), json!("16:9"));
    s.media.image.params.insert("n".into(), json!(1));
    s.media.image.params.insert("quality".into(), json!("high"));
    (s, Paths::with_root(root.join("data")))
}

const LONG: &str = "A tiny black cat sitting on a rainy windowsill at dusk, close-up shot, \
soft cinematic lighting, muted teal and amber palette, melancholic cozy mood, 85mm lens, \
shallow depth of field, ultra detailed, avoid text and watermarks";

fn run_tool(tool: &dyn Tool, args: &Value, root: &Path) -> Result<String> {
    let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Default, root);
    let ctx = CallCtx {
        workspace: root,
        policy: &policy,
        memory: None,
        scope: None,
    };
    let host = crate::Host::default();
    let cancel = Cancel::default();
    let exec = Exec {
        cancel: &cancel,
        deadline: std::time::Instant::now() + Duration::from_secs(60),
        timeout: Duration::from_secs(60),
        host: &host,
    };
    tool.run(args, &ctx, &exec)
}

#[test]
fn tools_exist_only_for_configured_models() {
    let root = scratch("reg");
    let paths = Paths::with_root(root.join("data"));
    assert!(tools_with_key(&Settings::default(), &paths, "k".into()).is_empty());
    let (mut s, paths) = settings("http://127.0.0.1:1/api/v1", &root);
    let names = |s: &Settings| {
        tools_with_key(s, &paths, "k".into())
            .iter()
            .map(|t| t.spec().name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&s), ["generate_image"]);
    s.media.video.model = "acme/clip-1".into();
    assert_eq!(names(&s), ["generate_image", "generate_video"]);
    s.media.image.model = "  ".into();
    assert_eq!(names(&s), ["generate_video"]);
    // The registry offers them through the same path as every other tool.
    let mut reg = crate::Registry::empty();
    for t in tools_with_key(&s, &paths, "k".into()) {
        reg.register(t);
    }
    assert!(reg.get("generate_video").is_some() && reg.get("generate_image").is_none());
}

/// The video list as `/videos/models` returns it, saved where the tools
/// read it at start-up.
fn save_video_list(paths: &Paths) {
    let dir = MediaSettings::catalog_dir(paths);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("openrouter-video-models.json"),
        json!({"data": [{"id": "acme/clip-1", "name": "Acme: Clip", "created": 9,
            "supported_resolutions": ["720p", "1080p"], "supported_aspect_ratios": ["16:9", "9:16"],
            "supported_sizes": null, "supported_durations": [-1, 4, 6, 8],
            "supported_frame_images": ["first_frame"], "generate_audio": true, "seed": false,
            "pricing_skus": {"duration_seconds_720p": "0.1"}}]})
        .to_string(),
    )
    .unwrap();
}

#[test]
fn specs_list_the_parameters_the_chosen_model_accepts() {
    let root = scratch("spec");
    let (mut s, paths) = settings("http://x/api/v1", &root);
    s.media.video.model = "acme/clip-1".into();
    s.media
        .video
        .params
        .insert("resolution".into(), json!("720p"));
    save_video_list(&paths);
    let t = tools_with_key(&s, &paths, "k".into());
    let img = t[0].spec();
    assert_eq!(img.permission, Permission::Network);
    assert!(!img.local);
    // No saved image list: a free set, the API validates.
    for w in [
        "acme/pic-1",
        "lighting",
        "iluminação",
        "VOCÊ escolhe",
        "não foi carregada",
        "12 palavras",
    ] {
        assert!(img.description.contains(w), "{w}");
    }
    assert_eq!(img.parameters["additionalProperties"], json!(false));
    assert!(img.parameters["properties"].get("model").is_none());
    assert_eq!(
        img.parameters["properties"]["aspect_ratio"]["type"],
        "string"
    );
    assert!(img.parameters["properties"]["aspect_ratio"]["description"]
        .as_str()
        .unwrap()
        .contains("se omitido: 16:9"));
    assert_eq!(img.parameters["required"], json!(["prompt"]));

    // Saved video list: exactly its parameters, typed, with their values.
    let vid = t[1].spec();
    let props = &vid.parameters["properties"];
    assert_eq!(
        props["duration"],
        json!({"type": "integer", "enum": [4, 6, 8],
        "description": props["duration"]["description"]})
    );
    assert_eq!(props["aspect_ratio"]["enum"], json!(["16:9", "9:16"]));
    assert_eq!(props["generate_audio"]["type"], "boolean");
    assert!(props["resolution"]["description"]
        .as_str()
        .unwrap()
        .contains("se omitido: 720p"));
    assert!(props["duration"]["description"]
        .as_str()
        .unwrap()
        .contains("padrão do modelo"));
    assert!(props.get("seed").is_none() && props.get("size").is_none());
    for w in [
        "Acme: Clip, acme/clip-1",
        "duration (4, 6, 8)",
        "aspect_ratio (16:9, 9:16)",
        "duration=8",
        "camera movement",
        "pacing",
        "generate_audio=true",
        "18 palavras",
    ] {
        assert!(vid.description.contains(w), "{w}: {}", vid.description);
    }
    assert!(
        vid.timeout >= Duration::from_secs(660),
        "video needs more than the 10 min poll"
    );
    assert!(vid.timeout > img.timeout);
}

#[test]
fn unknown_arguments_and_short_prompts_are_refused_without_a_request() {
    let root = scratch("gate");
    let (s, paths) = settings("http://127.0.0.1:1/api/v1", &root);
    let tool = &tools_with_key(&s, &paths, "k".into())[0];
    // The model cannot swap the model or send a parameter not offered.
    for bad in [
        json!({"prompt": LONG, "model": "x/y"}),
        json!({"prompt": LONG, "duration": 5}),
    ] {
        let e = run_tool(tool.as_ref(), &bad, &root).unwrap_err();
        assert!(matches!(e, ToolError::BadArguments(_)), "{e:?}");
        let a = tool.action(&bad, &dummy_ctx(&root)).unwrap_err();
        assert!(matches!(a, ToolError::BadArguments(_)));
    }
    let e = run_tool(tool.as_ref(), &json!({"prompt": "um gato"}), &root).unwrap_err();
    let msg = e.to_string();
    assert!(matches!(e, ToolError::BadArguments(_)));
    for w in [
        "curto demais",
        "mínimo 12",
        "iluminação",
        "paleta de cores",
        "o que evitar",
    ] {
        assert!(msg.contains(w), "{w}: {msg}");
    }
    // The gate counts words, not characters.
    assert!(quality_gate(
        &"palavra ".repeat(MIN_WORDS_IMAGE),
        MIN_WORDS_IMAGE,
        Kind::Image
    )
    .is_ok());
    assert!(quality_gate(
        &"palavra ".repeat(MIN_WORDS_IMAGE - 1),
        MIN_WORDS_IMAGE,
        Kind::Image
    )
    .is_err());
    assert!(quality_gate("x y z", MIN_WORDS_VIDEO, Kind::Video)
        .unwrap_err()
        .to_string()
        .contains("movimento de câmera"));
    const { assert!(MIN_WORDS_VIDEO > MIN_WORDS_IMAGE) };
    assert!(run_tool(tool.as_ref(), &json!({}), &root).is_err());
}

fn dummy_ctx(root: &Path) -> CallCtx<'static> {
    let policy: &'static Policy = Box::leak(Box::new(Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        root,
    )));
    CallCtx {
        workspace: policy.workspace.as_path(),
        policy,
        memory: None,
        scope: None,
    }
}

#[test]
fn the_policy_asks_before_spending_money() {
    let root = scratch("policy");
    let (s, paths) = settings("http://127.0.0.1:1/api/v1", &root);
    let mut reg = crate::Registry::empty();
    for t in tools_with_key(&s, &paths, "k".into()) {
        reg.register(t);
    }
    let ctx = dummy_ctx(&root);
    let (verdict, action) = reg
        .authorize("generate_image", &json!({"prompt": LONG}), &ctx)
        .unwrap();
    assert!(matches!(verdict, Verdict::Ask(_)), "{verdict:?}");
    assert!(
        matches!(action, Some(Action::Network(u)) if u.contains("acme/pic-1") && u.contains("custo"))
    );
    assert!(matches!(
        reg.call("generate_image", &json!({"prompt": LONG}), &ctx),
        Err(ToolError::Denied(_))
    ));
}

#[test]
fn image_is_generated_with_fixed_params_and_reported_with_markdown() {
    let root = scratch("e2e");
    let (base, seen) = fake_server();
    let (s, paths) = settings(&base, &root);
    let tool = &tools_with_key(&s, &paths, "k".into())[0];
    let out = run_tool(
        tool.as_ref(),
        &json!({"prompt": LONG, "filename_hint": "../Gato Preto"}),
        &root,
    )
    .unwrap();

    // The file exists under <output_dir>/<date>/ with the decoded bytes.
    let day = today();
    let dir = root.join("out").join(&day);
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let path = files[0].path();
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("gato-preto-"));
    assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG\r\n\x1a\n");

    assert!(out.contains("acme/pic-1"), "{out}");
    assert!(
        out.contains("aspect_ratio=16:9") && out.contains("n=1"),
        "{out}"
    );
    assert!(
        out.contains("Aviso:") && out.contains("quality"),
        "unsupported param is reported: {out}"
    );
    assert!(out.contains("Custo: US$ 0.0400"), "{out}");
    let md = out
        .lines()
        .find(|l| l.starts_with("!["))
        .expect("markdown line");
    assert!(md.ends_with(')') && md.contains("gato-preto-"), "{md}");
    assert!(!md.contains('\\'));

    let posts: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.starts_with("POST /api/v1/images"))
        .cloned()
        .collect();
    assert_eq!(posts.len(), 1);
    let body: Value = serde_json::from_str(
        posts[0]
            .split_once("{")
            .map(|(_, b)| format!("{{{b}"))
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert_eq!(body["model"], "acme/pic-1");
    assert_eq!(body["aspect_ratio"], "16:9");
    assert!(body.get("quality").is_none());
}

#[test]
fn the_chat_model_picks_the_parameters_and_a_bad_value_names_the_options() {
    let root = scratch("choose");
    let (base, seen) = fake_server();
    let (s, paths) = settings(&base, &root);
    let tool = &tools_with_key(&s, &paths, "k".into())[0];
    // The chosen value wins over the saved default; the rest of the
    // defaults still apply.
    let out = run_tool(
        tool.as_ref(),
        &json!({"prompt": LONG, "aspect_ratio": "1:1", "n": 2}),
        &root,
    )
    .unwrap();
    assert!(
        out.contains("aspect_ratio=1:1") && out.contains("n=2"),
        "{out}"
    );
    let post = seen
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.starts_with("POST /api/v1/images"))
        .cloned()
        .unwrap();
    assert!(
        post.contains(r#""aspect_ratio":"1:1""#) && post.contains(r#""n":2"#),
        "{post}"
    );

    // Outside the descriptor: refused before any paid request, with the
    // accepted values, as an argument error the model can fix.
    seen.lock().unwrap().clear();
    let e = run_tool(
        tool.as_ref(),
        &json!({"prompt": LONG, "aspect_ratio": "21:9"}),
        &root,
    )
    .unwrap_err();
    let msg = e.to_string();
    assert!(matches!(e, ToolError::BadArguments(_)), "{e:?}");
    assert!(
        msg.contains("1:1, 16:9") && msg.contains("Chame de novo"),
        "{msg}"
    );
    let e = run_tool(tool.as_ref(), &json!({"prompt": LONG, "n": 9}), &root).unwrap_err();
    assert!(e.to_string().contains("1..4"), "{e}");
    assert!(seen.lock().unwrap().iter().all(|r| !r.starts_with("POST")));

    // The run saved the list: the next start describes the exact model.
    let t = tools_with_key(&s, &paths, "k".into());
    let props = &t[0].spec().parameters["properties"];
    assert_eq!(props["aspect_ratio"]["enum"], json!(["1:1", "16:9"]));
    assert_eq!(props["n"]["type"], "integer");
    assert_eq!(
        (
            props["n"]["minimum"].as_f64(),
            props["n"]["maximum"].as_f64()
        ),
        (Some(1.0), Some(4.0))
    );
    assert!(props.get("quality").is_none());
}

#[test]
fn local_references_outside_the_workspace_are_not_uploaded() {
    let root = scratch("refs");
    let (s, paths) = settings("http://127.0.0.1:1/api/v1", &root);
    let tool = &tools_with_key(&s, &paths, "k".into())[0];
    let e = run_tool(
        tool.as_ref(),
        &json!({"prompt": LONG, "reference_images": [if cfg!(windows) { "C:/Windows/win.ini" } else { "/etc/hosts" }]}),
        &root,
    )
    .unwrap_err();
    assert!(matches!(e, ToolError::Denied(_)), "{e:?}");

    let secret_png = root.join("secret.png");
    std::fs::write(&secret_png, b"\x89PNG").unwrap();
    let mut deny_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, &root);
    deny_pol.rules =
        vec![
            zerith_security::Rule::parse(zerith_security::RuleKind::Deny, "read_file(*.png)")
                .unwrap(),
        ];
    let deny_ctx = CallCtx {
        workspace: &root,
        policy: &deny_pol,
        memory: None,
        scope: None,
    };
    let cancel = zerith_providers::Cancel::default();
    let host = crate::Host::default();
    let exec = Exec {
        cancel: &cancel,
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        timeout: Duration::from_secs(5),
        host: &host,
    };
    let err = tool
        .run(
            &json!({"prompt": LONG, "reference_images": ["secret.png"]}),
            &deny_ctx,
            &exec,
        )
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied(_)), "{err:?}");
}

#[test]
fn markdown_paths_with_spaces_stay_one_link_and_alt_text_is_clean() {
    assert_eq!(
        markdown_image("a [cat]\n(x)", Path::new("C:\\a b\\c.png")),
        "![a  cat   x](<C:/a b/c.png>)"
    );
    assert_eq!(
        markdown_image("cat", Path::new("C:\\a\\c.png")),
        "![cat](C:/a/c.png)"
    );
}

#[test]
fn today_is_a_plausible_iso_date() {
    let d = today();
    assert_eq!(d.len(), 10);
    assert!(d.starts_with("20"), "{d}");
    assert_eq!(&d[4..5], "-");
}
