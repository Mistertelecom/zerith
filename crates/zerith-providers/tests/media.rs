//! OpenRouter media client against a fake local server. No real network, no
//! paid call: every test talks to a `TcpListener` on 127.0.0.1.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zerith_providers::media::{
    b64_encode, ImageRequest, Kind, MediaError, OpenRouter, ParamKind, VideoRequest,
};
use zerith_providers::Cancel;

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn ok_json(v: Value) -> Resp {
    Resp {
        status: 200,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: v.to_string().into_bytes(),
    }
}

fn status_json(status: u16, v: Value) -> Resp {
    Resp {
        status,
        ..ok_json(v)
    }
}

type Handler = Box<dyn FnMut(&Req) -> Resp + Send>;

/// Routes by `METHOD path` (query string included, longest prefix wins).
struct Fake {
    base: String,
    seen: Arc<Mutex<Vec<Req>>>,
}

impl Fake {
    fn start(routes: Vec<(&str, Handler)>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let routes: Arc<Mutex<Vec<(String, Handler)>>> = Arc::new(Mutex::new(
            routes
                .into_iter()
                .map(|(k, h)| (k.to_string(), h))
                .collect(),
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let (routes, seen) = (routes.clone(), seen2.clone());
                std::thread::spawn(move || {
                    let Some(req) = read_request(&mut s) else {
                        return;
                    };
                    let key = format!("{} {}", req.method, req.path);
                    seen.lock().unwrap().push(req.clone());
                    let resp = {
                        let mut r = routes.lock().unwrap();
                        let best = r
                            .iter_mut()
                            .filter(|(k, _)| key.starts_with(k.as_str()))
                            .max_by_key(|(k, _)| k.len());
                        match best {
                            Some((_, h)) => h(&req),
                            None => status_json(404, json!({"error": {"message": "no route"}})),
                        }
                    };
                    let mut head = format!(
                        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        resp.status,
                        resp.body.len()
                    );
                    for (k, v) in &resp.headers {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(&resp.body);
                });
            }
        });
        Fake { base, seen }
    }

    fn requests(&self, prefix: &str) -> Vec<Req> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| format!("{} {}", r.method, r.path).starts_with(prefix))
            .cloned()
            .collect()
    }

    fn client(&self, key: Option<&str>) -> OpenRouter {
        OpenRouter::new(key.map(str::to_string), Some(self.base.clone()))
            .with_poll_interval(Duration::from_millis(20))
    }
}

fn read_request(s: &mut std::net::TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Req {
        method,
        path,
        headers,
        body,
    })
}

fn h(f: impl FnMut(&Req) -> Resp + Send + 'static) -> Handler {
    Box::new(f)
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("media-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn image_models() -> Value {
    json!({"data": [
        {"id": "acme/pic-1", "name": "Acme: Pic One", "description": "d", "created": 5,
         "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["image"]},
         "supported_parameters": {
            "aspect_ratio": {"type": "enum", "values": ["1:1", "16:9"]},
            "n": {"type": "range", "min": 1, "max": 4},
            "seed": {"type": "boolean"},
            "input_references": {"type": "range", "min": 0, "max": 2}}},
        {"id": "other/draw", "name": "Other: Draw", "description": "", "created": 1,
         "supported_parameters": {"quality": {"type": "enum", "values": ["low", "high"]}}}
    ]})
}

fn video_models() -> Value {
    json!({"data": [
        {"id": "acme/clip-1", "name": "Acme: Clip", "description": "d", "created": 9,
         "supported_resolutions": ["480p", "720p"], "supported_aspect_ratios": ["16:9", "9:16"],
         "supported_sizes": null, "supported_durations": [3, 5],
         "supported_frame_images": ["first_frame"], "generate_audio": true, "seed": true,
         "pricing_skus": {"cents_per_video_output_second_720p": "1"},
         "allowed_passthrough_parameters": []}
    ]})
}

/// 1x1 transparent PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89,
];

fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn image_req(p: BTreeMap<String, Value>) -> ImageRequest {
    ImageRequest {
        model: "acme/pic-1".into(),
        prompt: "a cat".into(),
        params: p,
        ..Default::default()
    }
}

#[test]
fn model_list_becomes_typed_specs_and_is_searchable_without_a_key() {
    let fake = Fake::start(vec![
        ("GET /api/v1/images/models", h(|_| ok_json(image_models()))),
        // Real per-image price; `other/draw` has no endpoint data (404).
        (
            "GET /api/v1/images/models/acme/pic-1/endpoints",
            h(|_| {
                ok_json(json!({"id": "acme/pic-1", "endpoints": [{"pricing": [
                    {"billable": "output_image", "unit": "image", "cost_usd": 0.04},
                    {"billable": "input_image", "unit": "image", "cost_usd": 0.008}]}]}))
            }),
        ),
        (
            "GET /api/v1/models?output_modalities=image",
            h(|_| {
                ok_json(json!({"total_count": 2, "data": [
                    {"id": "acme/pic-1", "pricing": {"image": "0.008"}},
                    {"id": "other/draw", "pricing": {"image_output": "0.000004"}}]}))
            }),
        ),
    ]);
    let or = fake.client(None);
    let page = or.models(Kind::Image, "", 0, 50).unwrap();
    assert_eq!(page.total, 2);
    let m = &page.models[0];
    assert_eq!(
        (m.id.as_str(), m.kind, m.created),
        ("acme/pic-1", Kind::Image, 5)
    );
    assert_eq!(m.price_hint, "US$ 0,04/imagem");
    assert_eq!(m.max_references, 2);
    let spec = |name: &str| {
        m.params
            .iter()
            .find(|p| p.name == name)
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(
        spec("aspect_ratio"),
        ParamKind::Enum(vec!["1:1".into(), "16:9".into()])
    );
    assert_eq!(spec("n"), ParamKind::Range { min: 1.0, max: 4.0 });
    assert_eq!(spec("seed"), ParamKind::Text);
    assert!(m.params.iter().all(|p| p.name != "input_references"));
    // Fallback: the generic listing's per-token price.
    assert_eq!(
        page.models[1].price_hint,
        "US$ 4,00 por 1M tokens de imagem gerada"
    );

    // Search, paging, and no Authorization header on the public lists.
    let q = or.models(Kind::Image, "OTHER draw", 0, 10).unwrap();
    assert_eq!((q.total, q.models.len()), (1, 1));
    let p2 = or.models(Kind::Image, "", 1, 1).unwrap();
    assert_eq!((p2.total, p2.models[0].id.as_str()), (2, "other/draw"));
    assert!(fake
        .requests("GET /api/v1/images/models")
        .iter()
        .all(|r| r.header("authorization").is_none()));
    // Cached: the second and third calls hit neither the list nor the prices.
    assert_eq!(fake.requests("GET /api/v1/images/models?").len(), 0);
    assert_eq!(
        fake.requests("GET /api/v1/images/models")
            .iter()
            .filter(|r| r.path == "/api/v1/images/models")
            .count(),
        1
    );
    assert_eq!(fake.requests("GET /api/v1/images/models/acme").len(), 1);
    assert_eq!(fake.requests("GET /api/v1/models?").len(), 1);
}

#[test]
fn video_list_builds_params_from_the_flat_fields() {
    let fake = Fake::start(vec![(
        "GET /api/v1/videos/models",
        h(|_| ok_json(video_models())),
    )]);
    let page = fake
        .client(None)
        .models(Kind::Video, "clip", 0, 10)
        .unwrap();
    let m = &page.models[0];
    let names: Vec<&str> = m.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "duration",
            "resolution",
            "aspect_ratio",
            "generate_audio",
            "seed"
        ]
    );
    assert_eq!(
        m.params[0].kind,
        ParamKind::Enum(vec!["3".into(), "5".into()])
    );
    assert_eq!(m.frame_images, ["first_frame"]);
    assert_eq!(m.price_hint, "US$ 0,01/s (720p)");
}

/// Shapes seen on the live lists on 2026-11-24: nulls everywhere, `-1` among
/// the durations, a `:free` twin, an entry without a name, the API's order
/// not guaranteed.
#[test]
fn every_model_is_kept_newest_first_whatever_the_fields() {
    let videos = json!({"data": [
        {"id": "google/veo-3.1", "name": "Google: Veo 3.1", "created": 1774277148,
         "supported_resolutions": ["720p", "1080p", "4K"], "supported_aspect_ratios": ["16:9", "9:16"],
         "supported_sizes": null, "supported_durations": [4, 6, 8],
         "supported_frame_images": ["first_frame", "last_frame"], "upscale_factor": null,
         "creativity": null, "generate_audio": true, "seed": true,
         "pricing_skus": {"duration_seconds_with_audio": "0.40"},
         "allowed_passthrough_parameters": null},
        {"id": "heygen/heygen-avatar-6-turbo", "canonical_slug": "x", "hugging_face_id": null,
         "name": "HeyGen: Avatar 6 Turbo", "created": 1794940883, "description": null,
         "supported_resolutions": ["1080p", "2K"], "supported_aspect_ratios": null,
         "supported_sizes": null, "supported_durations": null, "supported_frame_images": null,
         "generate_audio": false, "seed": false, "pricing_skus": null},
        {"id": "bytedance/seedance-2.5-flash", "name": "ByteDance: Seedance 2.5 Flash",
         "created": 1794874901, "supported_durations": [-1, 4, 5, 6],
         "supported_resolutions": [], "generate_audio": null, "seed": null,
         "pricing_skus": {"video_tokens": "0.000007"}},
        {"id": "", "name": "sem id"},
        {"name": "também sem id"},
        {"id": "google/veo-3.1", "name": "duplicado", "created": 1}
    ]});
    let all = zerith_providers::media::parse_list(Kind::Video, &videos).unwrap();
    let ids: Vec<&str> = all.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "heygen/heygen-avatar-6-turbo",
            "bytedance/seedance-2.5-flash",
            "google/veo-3.1"
        ]
    );
    assert_eq!(
        all[2].name, "Google: Veo 3.1",
        "the first entry of an id wins"
    );
    // -1 ("the model decides") is not a value to offer; an empty list is no param.
    let seedance = &all[1];
    assert_eq!(
        seedance.params[0].kind,
        ParamKind::Enum(vec!["4".into(), "5".into(), "6".into()])
    );
    assert_eq!(seedance.params.len(), 1);
    let heygen = &all[0];
    assert_eq!(heygen.description, "");
    assert_eq!(heygen.params.len(), 1);
    assert!(heygen.frame_images.is_empty());
    let now = 1794940883 + 86_400;
    assert!(heygen.is_new(now));
    assert!(!all[2].is_new(now));

    let images = json!({"data": [
        {"id": "old/pic", "created": 10, "supported_parameters": null},
        {"id": "recraft/recraft-v4.1-flash-v2:free", "name": "Recraft (free)", "created": 30,
         "supported_parameters": {"aspect_ratio": {"type": "enum", "values": ["1:1", "auto"]}}},
        {"id": "recraft/recraft-v4.1-flash-v2", "name": "Recraft", "created": 30,
         "architecture": null, "supported_parameters": {"n": {"type": "range", "min": 1, "max": 6}}},
        {"id": "x/no-date", "name": "Sem data", "created": null}
    ]});
    let all = zerith_providers::media::parse_list(Kind::Image, &images).unwrap();
    let ids: Vec<&str> = all.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "recraft/recraft-v4.1-flash-v2:free",
            "recraft/recraft-v4.1-flash-v2",
            "old/pic",
            "x/no-date"
        ]
    );
    assert_eq!(all[2].name, "old/pic", "no name falls back to the id");
    assert!(zerith_providers::media::parse_list(Kind::Image, &json!({"x": 1})).is_err());
}

#[test]
fn image_is_decoded_to_a_file_with_the_exact_bytes_and_fixed_params_typed() {
    let fake = Fake::start(vec![
        ("GET /api/v1/images/models", h(|_| ok_json(image_models()))),
        (
            "POST /api/v1/images",
            h(|_| {
                ok_json(json!({"created": 1, "data": [
                    {"b64_json": b64_encode(PNG), "media_type": "image/png"}],
                    "usage": {"cost": 0.04}}))
            }),
        ),
    ]);
    let dir = scratch("image");
    let mut req = image_req(params(&[
        ("aspect_ratio", json!("16:9")),
        ("n", json!("2")), // a string from a CLI flag must go out as an integer
        ("quality", json!("high")), // not supported by this model
    ]));
    req.filename_hint = Some("../../Meu Gato".into());
    let or = fake.client(Some("sk-or-test"));
    let plan = or.plan_image(&req).unwrap();
    assert_eq!(plan.sent["n"], json!(2));
    assert_eq!(plan.notes.len(), 1);
    assert!(plan.notes[0].contains("quality"), "{:?}", plan.notes);

    let files = or.generate_image(&req, &dir, &Cancel::default()).unwrap();
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(std::fs::read(&f.path).unwrap(), PNG);
    assert_eq!(
        (f.media_type.as_str(), f.bytes, f.cost_usd),
        ("image/png", PNG.len() as u64, Some(0.04))
    );
    assert_eq!(f.path.parent().unwrap(), dir);
    let name = f.path.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("meu-gato-") && name.ends_with(".png"),
        "{name}"
    );

    let sent = fake.requests("POST /api/v1/images")[0].clone();
    let b = sent.json();
    assert_eq!(b["model"], "acme/pic-1");
    assert_eq!(b["prompt"], "a cat");
    assert_eq!(b["aspect_ratio"], "16:9");
    assert_eq!(b["n"], 2);
    assert!(b.get("quality").is_none());
    assert_eq!(sent.header("authorization"), Some("Bearer sk-or-test"));
    assert_eq!(sent.header("x-title"), Some("Zerith"));
    assert!(sent.header("http-referer").is_some());
}

#[test]
fn a_bad_param_value_is_refused_before_any_paid_request() {
    let fake = Fake::start(vec![(
        "GET /api/v1/images/models",
        h(|_| ok_json(image_models())),
    )]);
    let or = fake.client(Some("k"));
    let dir = scratch("badparam");
    for (name, v) in [
        ("aspect_ratio", json!("7:3")),
        ("n", json!(9)),
        ("seed", json!("x")),
    ] {
        let err = or
            .generate_image(&image_req(params(&[(name, v)])), &dir, &Cancel::default())
            .unwrap_err();
        assert!(matches!(err, MediaError::Invalid(_)), "{name}: {err:?}");
        assert!(err.to_string().contains(name), "{err}");
    }
    assert!(fake.requests("POST").is_empty());
    // Unknown model: clear error naming the model, still no POST.
    let mut r = image_req(BTreeMap::new());
    r.model = "nope/none".into();
    let err = or.generate_image(&r, &dir, &Cancel::default()).unwrap_err();
    assert!(err.to_string().contains("nope/none"), "{err}");
    assert!(fake.requests("POST").is_empty());
    // Too many references.
    let mut r = image_req(BTreeMap::new());
    r.reference_images = vec!["https://x/a.png".into(); 3];
    assert!(matches!(
        or.generate_image(&r, &dir, &Cancel::default()),
        Err(MediaError::Invalid(_))
    ));
    // No key.
    let err = fake
        .client(None)
        .generate_image(&image_req(BTreeMap::new()), &dir, &Cancel::default())
        .unwrap_err();
    assert!(matches!(err, MediaError::NoKey));
}

#[test]
fn http_errors_map_to_distinct_pt_br_errors_and_never_leak_the_key() {
    for (status, extra, check) in [
        (401u16, vec![], "inválida"),
        (402, vec![], "créditos"),
        (403, vec![], "orçamento"),
        (
            429,
            vec![("Retry-After".to_string(), "7".to_string())],
            "7 s",
        ),
        (500, vec![], "500"),
    ] {
        let fake = Fake::start(vec![
            ("GET /api/v1/images/models", h(|_| ok_json(image_models()))),
            (
                "POST /api/v1/images",
                h(move |_| Resp {
                    status,
                    headers: extra.clone(),
                    // A hostile or sloppy gateway echoing the key back.
                    body: json!({"error": {"message": "bad Bearer sk-or-LEAK",
                        "metadata": {"limit_source": "key"}}})
                    .to_string()
                    .into_bytes(),
                }),
            ),
        ]);
        let err = fake
            .client(Some("sk-or-LEAK"))
            .generate_image(
                &image_req(BTreeMap::new()),
                &scratch("err"),
                &Cancel::default(),
            )
            .unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("sk-or-LEAK"), "{status}: {shown}");
        assert!(err.to_string().contains(check), "{status}: {err}");
        match status {
            401 | 403 | 500 => assert!(matches!(err, MediaError::Http { .. }), "{err:?}"),
            402 => {
                assert!(matches!(&err, MediaError::Payment(m) if m.contains("limit_source: key")))
            }
            _ => assert!(matches!(
                err,
                MediaError::RateLimited {
                    retry_after_ms: Some(7000),
                    ..
                }
            )),
        }
    }
}

fn video_req(p: BTreeMap<String, Value>) -> VideoRequest {
    VideoRequest {
        model: "acme/clip-1".into(),
        prompt: "a long elaborate prompt".into(),
        params: p,
        ..Default::default()
    }
}

#[test]
fn video_submits_polls_until_completed_then_downloads_with_the_key() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p2 = polls.clone();
    let fake = Fake::start(vec![
        ("GET /api/v1/videos/models", h(|_| ok_json(video_models()))),
        (
            "POST /api/v1/videos",
            h(|_| {
                status_json(
                    202,
                    json!({"id": "job-1", "polling_url": "http://unused/api/v1/videos/job-1"}),
                )
            }),
        ),
        (
            "GET /api/v1/videos/job-1/content",
            h(|_| Resp {
                status: 200,
                headers: vec![("Content-Type".into(), "video/mp4".into())],
                body: b"\0\0\0\x18ftypmp42-fake-video".to_vec(),
            }),
        ),
        (
            "GET /api/v1/videos/job-1",
            h(move |_| {
                let n = p2.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => ok_json(json!({"id": "job-1", "status": "pending"})),
                    1 => ok_json(json!({"id": "job-1", "status": "in_progress"})),
                    _ => ok_json(json!({"id": "job-1", "status": "completed",
                        "urls": ["http://unused/signed"], "usage": {"cost": 0.5, "is_byok": false}})),
                }
            }),
        ),
    ]);
    let dir = scratch("video");
    let mut seen = Vec::new();
    let mut req = video_req(params(&[
        ("duration", json!("5")),
        ("resolution", json!("720p")),
        ("generate_audio", json!(false)),
        ("size", json!("1280x720")), // unsupported here: dropped
    ]));
    req.first_frame = Some("https://x/f.png".into());
    req.last_frame = Some("https://x/l.png".into()); // model has no last_frame
    let f = fake
        .client(Some("k1"))
        .generate_video(&req, &dir, &Cancel::default(), &mut |s| {
            seen.push(s.to_string())
        })
        .unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 3);
    assert_eq!(
        std::fs::read(&f.path).unwrap(),
        b"\0\0\0\x18ftypmp42-fake-video"
    );
    assert_eq!(
        (f.media_type.as_str(), f.cost_usd),
        ("video/mp4", Some(0.5))
    );
    assert!(f.path.extension().unwrap() == "mp4");
    assert!(seen.iter().any(|s| s.starts_with("pending")), "{seen:?}");

    let sent = fake.requests("POST /api/v1/videos")[0].json();
    assert_eq!(sent["duration"], 5);
    assert_eq!(sent["resolution"], "720p");
    assert_eq!(sent["generate_audio"], false);
    assert!(sent.get("size").is_none());
    let frames = sent["frame_images"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["frame_type"], "first_frame");
    assert_eq!(frames[0]["image_url"]["url"], "https://x/f.png");
    let dl = &fake.requests("GET /api/v1/videos/job-1/content")[0];
    assert_eq!(dl.header("authorization"), Some("Bearer k1"));
}

#[test]
fn video_falls_back_to_the_signed_url_without_the_key() {
    let fake_cdn = Fake::start(vec![(
        "GET /signed",
        h(|_| Resp {
            status: 200,
            headers: vec![("Content-Type".into(), "video/mp4".into())],
            body: b"cdn-bytes".to_vec(),
        }),
    )]);
    let signed = fake_cdn.base.replace("/api/v1", "/signed");
    let fake = Fake::start(vec![
        ("GET /api/v1/videos/models", h(|_| ok_json(video_models()))),
        (
            "POST /api/v1/videos",
            h(|_| status_json(202, json!({"id": "j2"}))),
        ),
        (
            "GET /api/v1/videos/j2/content",
            h(|_| status_json(404, json!({"error": {"message": "gone"}}))),
        ),
        (
            "GET /api/v1/videos/j2",
            h(move |_| ok_json(json!({"status": "completed", "urls": [signed.clone()]}))),
        ),
    ]);
    let f = fake
        .client(Some("k"))
        .generate_video(
            &video_req(BTreeMap::new()),
            &scratch("signed"),
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(std::fs::read(&f.path).unwrap(), b"cdn-bytes");
    assert!(fake_cdn.requests("GET /signed")[0]
        .header("authorization")
        .is_none());
}

#[test]
fn video_failure_and_timeout_are_reported_and_cancel_stops_mid_poll() {
    let routes = |status: &'static str| -> Fake {
        Fake::start(vec![
            ("GET /api/v1/videos/models", h(|_| ok_json(video_models()))),
            (
                "POST /api/v1/videos",
                h(|_| status_json(202, json!({"id": "j3"}))),
            ),
            (
                "GET /api/v1/videos/j3",
                h(move |_| ok_json(json!({"status": status, "error": {"message": "moderação"}}))),
            ),
        ])
    };
    let dir = scratch("vfail");
    let err = routes("failed")
        .client(Some("k"))
        .generate_video(
            &video_req(BTreeMap::new()),
            &dir,
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        matches!(&err, MediaError::Failed(m) if m.contains("moderação")),
        "{err:?}"
    );

    let err = routes("in_progress")
        .client(Some("k"))
        .with_max_wait(Duration::from_millis(150))
        .generate_video(
            &video_req(BTreeMap::new()),
            &dir,
            &Cancel::default(),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(matches!(err, MediaError::Timeout), "{err:?}");

    let cancel = Cancel::default();
    let c2 = cancel.clone();
    let t = std::time::Instant::now();
    let err = routes("in_progress")
        .client(Some("k"))
        .generate_video(&video_req(BTreeMap::new()), &dir, &cancel, &mut |s| {
            if s.starts_with("in_progress") {
                c2.cancel();
            }
        })
        .unwrap_err();
    assert!(matches!(err, MediaError::Cancelled), "{err:?}");
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "no partial file"
    );
}

#[test]
fn check_key_reads_label_and_spend() {
    let fake = Fake::start(vec![(
        "GET /api/v1/key",
        h(|r| {
            if r.header("authorization") == Some("Bearer good") {
                ok_json(
                    json!({"data": {"label": "sk-or-v1-abc...xyz", "usage": 1.25, "limit": 10.0}}),
                )
            } else {
                status_json(401, json!({"error": {"message": "No auth"}}))
            }
        }),
    )]);
    let k = fake.client(Some("good")).check_key().unwrap();
    assert_eq!((k.usage_usd, k.limit_usd), (Some(1.25), Some(10.0)));
    assert_eq!(k.label, "sk-or-v1-abc...xyz");
    assert!(matches!(
        fake.client(Some("bad")).check_key(),
        Err(MediaError::Http { status: 401, .. })
    ));
    assert!(matches!(
        fake.client(None).check_key(),
        Err(MediaError::NoKey)
    ));
}

/// Hits the real public, free list endpoints. Run with `--ignored`.
#[test]
#[ignore = "usa a rede real (endpoints públicos e gratuitos)"]
fn live_public_lists_parse() {
    let or = OpenRouter::new(None, None);
    for kind in [Kind::Image, Kind::Video] {
        let page = or.models(kind, "", 0, 500).unwrap();
        assert!(page.total > 0, "{kind:?}");
        assert!(page.models.iter().all(|m| !m.id.is_empty()));
    }
}
