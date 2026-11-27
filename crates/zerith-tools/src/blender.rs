//! `blender`: hybrid Live TCP Bridge + Headless CLI (`blender --background --python`)
//! 3D CAD & engineering tool.
//!
//! Architecture:
//! - **Live Bridge (`127.0.0.1:9877` default)**: framed newline-delimited JSON over
//!   loopback TCP authenticated with a random session token (`hmac.compare_digest`
//!   in the Blender addon, dispatched on Blender's main thread via
//!   `bpy.app.timers.register()` + `queue.Queue`).
//! - **Headless Fallback (`blender --background [blend_file] --python`)**: automatic
//!   when `mode` is `"auto"` (default) and no live bridge is reachable/configured, or
//!   explicit when `mode` is `"headless"`.
//! - **Engineering CAD pipeline**: millimeter metrics, manifold/watertight validation,
//!   flipped-normal detection, `BVHTree` collision checks, 4-view 2×2 PNG renders
//!   emitted with `IMAGE_MARKER` for vision inspection, and verified mesh exports.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zerith_core::Paths;
use zerith_security::Action;

use crate::fntool::{opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const DEFAULT_BRIDGE_HOST: &str = "127.0.0.1";
const DEFAULT_BRIDGE_PORT: u16 = 9877;
const MAX_BATCH_STEPS: usize = 12;
const MAX_BRIDGE_LINE_BYTES: usize = 16 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const RESULT_PREFIX: &str = "ZERITH_RESULT:";
const DEFAULT_ADDON_REL_PATH: &str = ".zerith/zerith_blender_bridge.py";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Auto,
    Live,
    Headless,
}

#[derive(Debug, Clone)]
struct BridgeEndpoint {
    host: String,
    port: u16,
    token: Option<String>,
}

struct BlenderTool {
    spec: ToolSpec,
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(BlenderTool {
        spec: ToolSpec {
            name: "blender".into(),
            description: "Ferramenta híbrida de engenharia 3D/CAD no Blender (Live TCP Bridge autenticado em 127.0.0.1:9877 ou modo headless `blender --background --python`). \
Ações: `status` (binário, versão e bridge), `install_addon` (gera o addon seguro `zerith_blender_bridge.py`), `scene_info` (cena, unidades, objetos e bounding box), \
`inspect_object` (dimensões em mm, volume, área, manifold, escala aplicada e massa estimada em PLA/PETG/ABS/Nylon/Alumínio/Aço), \
`exec_bpy` (executa Python `bpy`/`bmesh`/`mathutils` com undo e `save_blend` opcional), \
`cad_check` (DfM: estanqueidade manifold, normais invertidas/inconsistentes, faces degeneradas, escala e colisões pairwise via `BVHTree`), \
`render_views` (renderiza grid 2×2 ortográfico/isométrico `front`/`right`/`top`/`iso_45` em PNG e anexa via IMAGE_MARKER para inspeção visual), \
`export` (stl, 3mf, glb, gltf, obj, fbx com verificação de escala e `apply_transforms`), \
`batch` (1 a 12 passos sequenciais na mesma cena)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "status",
                            "install_addon",
                            "scene_info",
                            "inspect_object",
                            "exec_bpy",
                            "cad_check",
                            "render_views",
                            "export",
                            "batch"
                        ]
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "live", "headless"],
                        "description": "auto (padrão: tenta bridge ao vivo e cai para blender --background), live ou headless"
                    },
                    "blend_file": {
                        "type": "string",
                        "description": "arquivo .blend no workspace para abrir em modo headless"
                    },
                    "save_blend": {
                        "type": "string",
                        "description": "exec_bpy: caminho opcional para salvar o .blend modificado em modo headless"
                    },
                    "object_name": {
                        "type": "string",
                        "description": "inspect_object/cad_check/render_views/export: nome do objeto alvo"
                    },
                    "object_names": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "cad_check/export: lista opcional de objetos alvo"
                    },
                    "code": {
                        "type": "string",
                        "description": "exec_bpy: código Python com bpy, bmesh, mathutils, math e json já importados"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "install_addon/render_views/export: caminho de saída no workspace"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["stl", "3mf", "glb", "gltf", "obj", "fbx"],
                        "description": "export: formato de saída (inferido de output_path se omitido)"
                    },
                    "apply_transforms": {
                        "type": "boolean",
                        "description": "export: aplica rotação e escala antes de exportar (padrão: true)"
                    },
                    "unit_scale": {
                        "type": "number",
                        "description": "export: multiplicador de escala opcional (padrão: 1.0)"
                    },
                    "engine": {
                        "type": "string",
                        "enum": ["workbench", "eevee", "cycles"],
                        "description": "render_views: motor de render (padrão: workbench)"
                    },
                    "resolution": {
                        "type": "integer",
                        "description": "render_views: largura/altura de cada quadrante em px (256..1024, padrão: 512)"
                    },
                    "samples": {
                        "type": "integer",
                        "description": "render_views: amostras para Cycles/EEVEE (padrão: 16)"
                    },
                    "min_wall_mm": {
                        "type": "number",
                        "description": "cad_check: dimensão/espessura mínima esperada em mm (padrão: 0.4)"
                    },
                    "host": {
                        "type": "string",
                        "description": "host loopback do bridge ao vivo (padrão: 127.0.0.1)"
                    },
                    "port": {
                        "type": "integer",
                        "description": "porta TCP do bridge ao vivo (padrão: 9877)"
                    },
                    "token": {
                        "type": "string",
                        "description": "token de sessão do bridge ao vivo (ou ZERITH_BLENDER_TOKEN)"
                    },
                    "steps": {
                        "type": "array",
                        "items": {"type": "object"},
                        "description": "batch: sequência de 1 a 12 passos (sem batch aninhado)"
                    }
                },
                "required": ["action"]
            }),
            returns: "status/scene_info/inspect_object/cad_check: JSON com métricas 3D e DfM; exec_bpy/export/install_addon: resultado e caminhos; render_views: IMAGE_MARKER + grid 2×2 PNG + métricas",
            permission: Permission::Write,
            timeout: Duration::from_secs(180),
            cancellable: true,
            local: true,
            source: "built-in".into(),
        },
    })]
}

impl Tool for BlenderTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        blender_action(args, ctx)
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        run_blender(args, ctx, exec)
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        matches!(
            args["action"].as_str(),
            Some("status" | "scene_info" | "inspect_object" | "cad_check")
        )
    }
}

fn blender_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    let action = str_arg(args, "action")?;
    match action {
        "status" | "scene_info" | "inspect_object" | "cad_check" => Ok(None),
        "install_addon" => {
            let out = opt_str(args, "output_path").unwrap_or(DEFAULT_ADDON_REL_PATH);
            Ok(Some(Action::Write(ctx.resolve(out))))
        }
        "export" => {
            let out = str_arg(args, "output_path")?;
            Ok(Some(Action::Write(ctx.resolve(out))))
        }
        "render_views" => {
            let out = opt_str(args, "output_path").unwrap_or(".zerith/blender/render_views.png");
            Ok(Some(Action::Write(ctx.resolve(out))))
        }
        "exec_bpy" => {
            if let Some(save) = opt_str(args, "save_blend") {
                Ok(Some(Action::Write(ctx.resolve(save))))
            } else {
                Ok(Some(Action::Shell(
                    "blender --background --python <exec_bpy>".into(),
                )))
            }
        }
        "batch" => {
            let steps = validate_batch_steps(args)?;
            let mut write_action: Option<Action> = None;
            for step in steps {
                if let Some(act) = blender_action(step, ctx)? {
                    match act {
                        Action::Shell(_) => return Ok(Some(act)),
                        Action::Write(_) if write_action.is_none() => write_action = Some(act),
                        _ => {}
                    }
                }
            }
            Ok(write_action)
        }
        other => Err(bad_action(other)),
    }
}

fn bad_action(action: &str) -> ToolError {
    ToolError::BadArguments(format!(
        "ação desconhecida em blender: {action}; use status, install_addon, scene_info, inspect_object, exec_bpy, cad_check, render_views, export ou batch"
    ))
}

fn parse_run_mode(args: &Value) -> Result<RunMode> {
    match opt_str(args, "mode").unwrap_or("auto") {
        "auto" => Ok(RunMode::Auto),
        "live" => Ok(RunMode::Live),
        "headless" => Ok(RunMode::Headless),
        other => Err(ToolError::BadArguments(format!(
            "mode inválido: {other}; use auto, live ou headless"
        ))),
    }
}

fn validate_batch_steps(args: &Value) -> Result<&Vec<Value>> {
    let steps = args["steps"]
        .as_array()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ToolError::BadArguments("batch exige `steps` com 1 a 12 objetos de ação".into())
        })?;
    if steps.len() > MAX_BATCH_STEPS {
        return Err(ToolError::BadArguments(format!(
            "batch aceita no máximo {MAX_BATCH_STEPS} passos por chamada (recebido {})",
            steps.len()
        )));
    }
    for (idx, step) in steps.iter().enumerate() {
        let act = step["action"].as_str().ok_or_else(|| {
            ToolError::BadArguments(format!("steps[{idx}] precisa do campo string `action`"))
        })?;
        if act == "batch" {
            return Err(ToolError::BadArguments(format!(
                "steps[{idx}]: `batch` aninhado não é permitido"
            )));
        }
        if !matches!(
            act,
            "status"
                | "install_addon"
                | "scene_info"
                | "inspect_object"
                | "exec_bpy"
                | "cad_check"
                | "render_views"
                | "export"
        ) {
            return Err(ToolError::BadArguments(format!(
                "steps[{idx}]: ação inválida `{act}`"
            )));
        }
    }
    Ok(steps)
}

fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim().trim_start_matches('[').trim_end_matches(']');
    matches!(trimmed, "127.0.0.1" | "localhost" | "::1")
}

fn bridge_config_path() -> PathBuf {
    Paths::resolve().root.join("blender").join("bridge.json")
}

fn resolve_bridge_endpoint(args: &Value) -> Result<BridgeEndpoint> {
    let env_host = env::var("ZERITH_BLENDER_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let host = opt_str(args, "host")
        .map(String::from)
        .or(env_host)
        .unwrap_or_else(|| DEFAULT_BRIDGE_HOST.into());
    if !is_loopback_host(&host) {
        return Err(ToolError::BadArguments(format!(
            "bridge aceita apenas host loopback (127.0.0.1, localhost ou ::1), recebido `{host}`"
        )));
    }

    let config_val = fs::read_to_string(bridge_config_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());

    let port = if let Some(p) = args["port"].as_u64() {
        u16::try_from(p)
            .ok()
            .filter(|&v| v > 0)
            .ok_or_else(|| ToolError::BadArguments(format!("port inválido: {p}")))?
    } else if let Ok(raw_p) = env::var("ZERITH_BLENDER_PORT") {
        raw_p
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|&v| v > 0)
            .ok_or_else(|| {
                ToolError::BadArguments(format!("ZERITH_BLENDER_PORT inválido: {raw_p}"))
            })?
    } else if let Some(cfg_port) = config_val
        .as_ref()
        .and_then(|v| v["port"].as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&v| v > 0)
    {
        cfg_port
    } else {
        DEFAULT_BRIDGE_PORT
    };

    let token = opt_str(args, "token")
        .map(String::from)
        .or_else(|| {
            env::var("ZERITH_BLENDER_TOKEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            config_val
                .as_ref()
                .and_then(|v| v["token"].as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    Ok(BridgeEndpoint { host, port, token })
}

/// Extracts a `(major, minor, patch)` hint from paths like `Blender 4.3` or `blender-4.2.1-linux-x64`.
fn parse_blender_version_hint(path: &Path) -> (u32, u32, u32) {
    let mut best = (0u32, 0u32, 0u32);
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let mut parts = [0u32; 3];
                let mut p_idx = 0;
                let mut cur = i;
                while cur < bytes.len() && p_idx < 3 {
                    let start = cur;
                    while cur < bytes.len() && bytes[cur].is_ascii_digit() {
                        cur += 1;
                    }
                    if start == cur {
                        break;
                    }
                    if let Ok(num) = s[start..cur].parse::<u32>() {
                        parts[p_idx] = num;
                        p_idx += 1;
                    }
                    if cur < bytes.len() && bytes[cur] == b'.' {
                        cur += 1;
                    } else {
                        break;
                    }
                }
                if p_idx >= 2 && (2..=9).contains(&parts[0]) {
                    let cand = (parts[0], parts[1], parts[2]);
                    if cand > best {
                        best = cand;
                    }
                }
                i = cur.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    best
}

fn pick_best_blender_candidate(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    let mut best: Option<((u32, u32, u32), usize, PathBuf)> = None;
    let mut seen = HashSet::new();
    for (idx, path) in candidates.into_iter().enumerate() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let ver = parse_blender_version_hint(&path);
        let replace = match &best {
            None => true,
            Some((best_ver, best_idx, _)) => {
                ver > *best_ver || (ver == *best_ver && idx < *best_idx)
            }
        };
        if replace {
            best = Some((ver, idx, path));
        }
    }
    best.map(|(_, _, p)| p)
}

fn collect_blender_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = env::var_os("ZERITH_BLENDER_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            out.push(p);
            return out;
        }
    }

    let bin_names: &[&str] = if cfg!(windows) {
        &["blender.exe", "blender"]
    } else {
        &["blender"]
    };

    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            for name in bin_names {
                let p = dir.join(name);
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }

    #[cfg(windows)]
    {
        let mut roots = vec![
            PathBuf::from(r"C:\Program Files\Blender Foundation"),
            PathBuf::from(r"C:\Program Files (x86)\Blender Foundation"),
        ];
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            roots.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Blender Foundation"),
            );
        }
        for root in roots {
            if let Ok(entries) = fs::read_dir(root) {
                for entry in entries.flatten() {
                    let exe = entry.path().join("blender.exe");
                    if exe.is_file() {
                        out.push(exe);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mac = PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender");
        if mac.is_file() {
            out.push(mac);
        }
        if let Some(home) = env::var_os("HOME") {
            let user_mac = PathBuf::from(home)
                .join("Applications")
                .join("Blender.app")
                .join("Contents")
                .join("MacOS")
                .join("Blender");
            if user_mac.is_file() {
                out.push(user_mac);
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in [
            "/usr/bin/blender",
            "/usr/local/bin/blender",
            "/snap/bin/blender",
            "/opt/blender/blender",
            "/var/lib/flatpak/exports/bin/org.blender.Blender",
        ] {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                out.push(pb);
            }
        }
        if let Ok(entries) = fs::read_dir("/opt") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("blender") {
                    let bin = entry.path().join("blender");
                    if bin.is_file() {
                        out.push(bin);
                    }
                }
            }
        }
    }

    out
}

fn find_blender_binary() -> Option<PathBuf> {
    pick_best_blender_candidate(collect_blender_candidates())
}

fn query_blender_version(bin: &Path, exec: &Exec<'_>) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let child = cmd.spawn().ok()?;
    let (status, stdout, _) = wait_child_output(child, exec, Duration::from_secs(10)).ok()?;
    if !status.success() {
        return None;
    }
    stdout.lines().next().map(|l| l.trim().to_string())
}

fn wait_child_output(
    mut child: Child,
    exec: &Exec<'_>,
    local_budget: Duration,
) -> Result<(std::process::ExitStatus, String, String)> {
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf
        })
    });

    let local_deadline = Instant::now() + local_budget;
    let status = loop {
        if let Err(err) = exec.check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        if Instant::now() >= local_deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::Timeout(local_budget));
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Io(e));
            }
        }
    };

    let stdout_bytes = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr_bytes = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    Ok((
        status,
        String::from_utf8_lossy(&stdout_bytes).into_owned(),
        String::from_utf8_lossy(&stderr_bytes).into_owned(),
    ))
}

fn send_bridge_request(
    host: &str,
    port: u16,
    token: &str,
    req: &Value,
    exec: &Exec<'_>,
) -> Result<Value> {
    let addr = format!("{host}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| ToolError::Failed(format!("endereço inválido: {host}:{port}")))?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| ToolError::Failed(format!("bridge indisponível em {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut payload = req.as_object().cloned().unwrap_or_default();
    payload.insert("token".into(), Value::String(token.to_string()));
    let mut line = serde_json::to_string(&Value::Object(payload))
        .map_err(|e| ToolError::Failed(format!("falha ao serializar requisição: {e}")))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut raw = Vec::new();
    let mut line_buf = Vec::new();
    loop {
        exec.check()?;
        line_buf.clear();
        match reader.read_until(b'\n', &mut line_buf) {
            Ok(0) => {
                if raw.is_empty() {
                    return Err(ToolError::Failed(
                        "bridge fechou a conexão sem responder".into(),
                    ));
                }
                break;
            }
            Ok(_) => {
                if raw.len() + line_buf.len() > MAX_BRIDGE_LINE_BYTES {
                    return Err(ToolError::Failed("resposta do bridge excedeu 16 MB".into()));
                }
                raw.extend_from_slice(&line_buf);
                if raw.ends_with(b"\n") {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue;
            }
            Err(e) => return Err(ToolError::Io(e)),
        }
    }

    let text = String::from_utf8_lossy(&raw);
    let resp: Value = serde_json::from_str(text.trim())
        .map_err(|e| ToolError::Failed(format!("JSON inválido do bridge: {e}")))?;
    if resp["ok"].as_bool() != Some(true) {
        let msg = resp["error"]
            .as_str()
            .unwrap_or("erro desconhecido no bridge Blender");
        return Err(ToolError::Failed(format!("bridge Blender recusou: {msg}")));
    }
    Ok(resp["result"].clone())
}

fn resolve_export_format(args: &Value, output_path: &Path) -> Result<String> {
    let raw = if let Some(fmt) = opt_str(args, "format") {
        fmt.trim().trim_start_matches('.').to_ascii_lowercase()
    } else if let Some(ext) = output_path.extension().and_then(|e| e.to_str()) {
        ext.trim().to_ascii_lowercase()
    } else {
        return Err(ToolError::BadArguments(
            "export exige `format` ou extensão válida em `output_path` (stl, 3mf, glb, gltf, obj, fbx)"
                .into(),
        ));
    };

    if matches!(raw.as_str(), "stl" | "3mf" | "glb" | "gltf" | "obj" | "fbx") {
        Ok(raw)
    } else {
        Err(ToolError::BadArguments(format!(
            "formato de exportação não suportado: `{raw}`; use stl, 3mf, glb, gltf, obj ou fbx"
        )))
    }
}

fn resolve_render_output_path(args: &Value, ctx: &CallCtx<'_>) -> Result<PathBuf> {
    let out = if let Some(p) = opt_str(args, "output_path") {
        ctx.resolve(p)
    } else {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        ctx.workspace
            .join(".zerith")
            .join("blender")
            .join(format!("views_{ms}.png"))
    };
    let is_png = out
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !is_png {
        return Err(ToolError::BadArguments(format!(
            "render_views exige `output_path` com extensão .png (recebido `{}`)",
            out.display()
        )));
    }
    Ok(out)
}

fn resolve_existing_blend(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<PathBuf>> {
    let Some(raw) = opt_str(args, "blend_file") else {
        return Ok(None);
    };
    let path = ctx.resolve(raw);
    if !path.is_file() {
        return Err(ToolError::BadArguments(format!(
            "blend_file não encontrado: {}",
            path.display()
        )));
    }
    Ok(Some(path))
}

fn validate_action_args(args: &Value, ctx: &CallCtx<'_>) -> Result<()> {
    let action = str_arg(args, "action")?;
    match action {
        "status" | "scene_info" | "inspect_object" | "cad_check" | "install_addon" => Ok(()),
        "exec_bpy" => {
            let _ = str_arg(args, "code")?;
            Ok(())
        }
        "render_views" => {
            let _ = resolve_render_output_path(args, ctx)?;
            if let Some(eng) =
                opt_str(args, "engine").filter(|e| !matches!(*e, "workbench" | "eevee" | "cycles"))
            {
                return Err(ToolError::BadArguments(format!(
                    "engine inválido: `{eng}`; use workbench, eevee ou cycles"
                )));
            }
            Ok(())
        }
        "export" => {
            let out = ctx.resolve(str_arg(args, "output_path")?);
            let _ = resolve_export_format(args, &out)?;
            Ok(())
        }
        "batch" => {
            let steps = validate_batch_steps(args)?;
            for step in steps {
                validate_action_args(step, ctx)?;
            }
            Ok(())
        }
        other => Err(bad_action(other)),
    }
}

fn normalize_request_for_blender(args: &Value, ctx: &CallCtx<'_>) -> Result<Value> {
    let action = str_arg(args, "action")?;
    let mut obj = args.as_object().cloned().unwrap_or_default();

    if action == "render_views" {
        let out = resolve_render_output_path(args, ctx)?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        obj.insert(
            "output_path".into(),
            Value::String(out.to_string_lossy().into_owned()),
        );
    } else if action == "export" {
        let out = ctx.resolve(str_arg(args, "output_path")?);
        let fmt = resolve_export_format(args, &out)?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        obj.insert(
            "output_path".into(),
            Value::String(out.to_string_lossy().into_owned()),
        );
        obj.insert("format".into(), Value::String(fmt));
    } else if action == "exec_bpy" {
        if let Some(save) = opt_str(args, "save_blend") {
            let save_path = ctx.resolve(save);
            if let Some(parent) = save_path.parent() {
                fs::create_dir_all(parent)?;
            }
            obj.insert(
                "save_blend".into(),
                Value::String(save_path.to_string_lossy().into_owned()),
            );
        }
    }

    // Never leak client-only fields as raw strings if unneeded, keep everything clean.
    obj.remove("token");
    Ok(Value::Object(obj))
}

fn parse_zerith_result(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(RESULT_PREFIX))
        .and_then(|json_part| serde_json::from_str::<Value>(json_part.trim()).ok())
}

fn tail_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn run_headless_python(
    bin: &Path,
    blend_file: Option<&Path>,
    script_body: &str,
    exec: &Exec<'_>,
) -> Result<Value> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!("zerith-blender-{}-{seq}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let script_path = dir.join("run.py");
    fs::write(&script_path, script_body)?;

    let res = (|| {
        let mut cmd = Command::new(bin);
        cmd.arg("--background");
        if let Some(bf) = blend_file {
            cmd.arg(bf);
        }
        cmd.arg("--python-exit-code")
            .arg("1")
            .arg("--python")
            .arg(&script_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }

        let child = cmd.spawn().map_err(|e| {
            ToolError::Failed(format!(
                "falha ao executar Blender `{}`: {e}",
                bin.display()
            ))
        })?;
        let remaining = exec
            .deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1));
        let (status, stdout, stderr) = wait_child_output(child, exec, remaining)?;

        if let Some(val) = parse_zerith_result(&stdout) {
            if val["ok"].as_bool() == Some(true) {
                return Ok(val["result"].clone());
            }
            let err = val["error"]
                .as_str()
                .unwrap_or("erro ao executar script Python no Blender");
            return Err(ToolError::Failed(err.to_string()));
        }

        Err(ToolError::Failed(format!(
            "Blender terminou (código {:?}) sem retornar {RESULT_PREFIX}.\nstdout:\n{}\nstderr:\n{}",
            status.code(),
            tail_lines(&stdout, 30),
            tail_lines(&stderr, 20)
        )))
    })();

    let _ = fs::remove_dir_all(&dir);
    res
}

fn build_headless_script(payload: &Value) -> Result<String> {
    let req_json = serde_json::to_string(payload)
        .map_err(|e| ToolError::Failed(format!("falha ao serializar payload: {e}")))?;
    Ok(format!(
        "{BPY_HELPERS_PY}\n\n\
_ZERITH_REQ = json.loads({req_literal})\n\
try:\n\
    _res = _zerith_dispatch(_ZERITH_REQ.get('action', ''), _ZERITH_REQ)\n\
    print({prefix_literal} + json.dumps({{'ok': True, 'result': _res}}, ensure_ascii=False), flush=True)\n\
except Exception as _exc:\n\
    import traceback\n\
    print({prefix_literal} + json.dumps({{'ok': False, 'error': f'{{type(_exc).__name__}}: {{_exc}}\\n{{traceback.format_exc()}}'}}, ensure_ascii=False), flush=True)\n\
    sys.exit(1)\n",
        req_literal = python_string_literal(&req_json),
        prefix_literal = python_string_literal(RESULT_PREFIX),
    ))
}

fn build_headless_batch_script(steps: &[Value]) -> Result<String> {
    let steps_json = serde_json::to_string(steps)
        .map_err(|e| ToolError::Failed(format!("falha ao serializar passos batch: {e}")))?;
    Ok(format!(
        "{BPY_HELPERS_PY}\n\n\
_ZERITH_STEPS = json.loads({steps_literal})\n\
_results = []\n\
try:\n\
    for _idx, _step in enumerate(_ZERITH_STEPS):\n\
        _act = _step.get('action', '')\n\
        _r = _zerith_dispatch(_act, _step)\n\
        _results.append({{'step': _idx + 1, 'action': _act, 'result': _r}})\n\
    print({prefix_literal} + json.dumps({{'ok': True, 'result': {{'steps': _results}}}}, ensure_ascii=False), flush=True)\n\
except Exception as _exc:\n\
    import traceback\n\
    print({prefix_literal} + json.dumps({{'ok': False, 'error': f'{{type(_exc).__name__}}: {{_exc}}\\n{{traceback.format_exc()}}', 'partial_steps': _results}}, ensure_ascii=False), flush=True)\n\
    sys.exit(1)\n",
        steps_literal = python_string_literal(&steps_json),
        prefix_literal = python_string_literal(RESULT_PREFIX),
    ))
}

fn python_string_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn format_render_views_output(result: &Value) -> Result<String> {
    let raw_path = result["output_path"]
        .as_str()
        .ok_or_else(|| ToolError::Failed("render_views não retornou output_path".into()))?;
    let path = PathBuf::from(raw_path);
    let meta = fs::metadata(&path).map_err(|e| {
        ToolError::Failed(format!(
            "PNG de render_views não foi gravado em {}: {e}",
            path.display()
        ))
    })?;
    let kb = meta.len().div_ceil(1024);
    let shown = path.display().to_string().replace('\\', "/");
    let md = if shown.contains(char::is_whitespace) || shown.contains(')') {
        format!("![render_views](<{shown}>)")
    } else {
        format!("![render_views]({shown})")
    };
    let summary = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    Ok(format!(
        "{} png, {kb} KB: ![imagem](<{shown}>)\nGrid 2×2 (front, right, top, iso_45) salvo em {} ({kb} KB).\n{md}\n{summary}",
        crate::IMAGE_MARKER,
        path.display()
    ))
}

fn format_action_output(action: &str, result: &Value) -> Result<String> {
    if action == "render_views" {
        return format_render_views_output(result);
    }
    Ok(serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()))
}

fn run_blender(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    exec.check()?;
    validate_action_args(args, ctx)?;
    let action = str_arg(args, "action")?;

    if action == "status" {
        return run_status(args, ctx, exec);
    }
    if action == "install_addon" {
        return run_install_addon(args, ctx);
    }
    if action == "batch" {
        return run_batch(args, ctx, exec);
    }

    let mode = parse_run_mode(args)?;
    let blend_file = resolve_existing_blend(args, ctx)?;
    let payload = normalize_request_for_blender(args, ctx)?;
    let endpoint = resolve_bridge_endpoint(args)?;

    // If a specific .blend file was provided in `auto` mode, headless opens that exact file;
    // in `live` mode, refuse if blend_file is set to avoid silently ignoring it.
    if mode == RunMode::Live && blend_file.is_some() {
        return Err(ToolError::BadArguments(
            "`blend_file` só é suportado em `mode: \"headless\"` ou `mode: \"auto\"`".into(),
        ));
    }

    let try_live = match mode {
        RunMode::Live => true,
        RunMode::Headless => false,
        RunMode::Auto => blend_file.is_none() && endpoint.token.is_some(),
    };

    if try_live {
        let Some(token) = endpoint.token.as_deref() else {
            return Err(ToolError::Failed(
                "nenhum token configurado para o bridge Blender ao vivo; passe `token`, defina `ZERITH_BLENDER_TOKEN` ou rode `install_addon`"
                    .into(),
            ));
        };
        match send_bridge_request(&endpoint.host, endpoint.port, token, &payload, exec) {
            Ok(result) => return format_action_output(action, &result),
            Err(err) if mode == RunMode::Live => return Err(err),
            Err(_) => {}
        }
    }

    let bin = find_blender_binary().ok_or_else(|| {
        ToolError::Failed(
            "binário do Blender não encontrado (defina `ZERITH_BLENDER_BIN` ou adicione `blender` ao PATH)"
                .into(),
        )
    })?;
    let script = build_headless_script(&payload)?;
    let result = run_headless_python(&bin, blend_file.as_deref(), &script, exec)?;
    format_action_output(action, &result)
}

fn run_status(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let bin = find_blender_binary();
    let version = bin.as_deref().and_then(|b| query_blender_version(b, exec));
    let endpoint = resolve_bridge_endpoint(args)?;

    let (bridge_reachable, bridge_ping) = match endpoint.token.as_deref() {
        Some(tok) => {
            let ping = json!({"action": "ping"});
            match send_bridge_request(&endpoint.host, endpoint.port, tok, &ping, exec) {
                Ok(v) => (true, Some(v)),
                Err(e) => (false, Some(json!({"error": e.to_string()}))),
            }
        }
        None => (false, None),
    };

    let addon_ws = ctx.resolve(DEFAULT_ADDON_REL_PATH);
    let cfg_path = bridge_config_path();
    let status = json!({
        "blender_found": bin.is_some(),
        "blender_bin": bin.map(|p| p.display().to_string()),
        "blender_version": version,
        "bridge": {
            "host": endpoint.host,
            "port": endpoint.port,
            "token_configured": endpoint.token.is_some(),
            "reachable": bridge_reachable,
            "ping": bridge_ping,
            "config_file": cfg_path.display().to_string(),
        },
        "addon_workspace_path": addon_ws.is_file().then(|| addon_ws.display().to_string()),
    });
    Ok(serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string()))
}

fn build_addon_source(default_port: u16) -> String {
    let cfg_dir_hint = bridge_config_path()
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        r####"# Zerith Blender Bridge Addon (Blender 3.6+ / 4.x)
# Loopback-only (127.0.0.1) + HMAC constant-time session token + main-thread timer pump.

bl_info = {{
    "name": "Zerith Blender Bridge",
    "author": "Zerith",
    "version": (1, 0, 0),
    "blender": (3, 6, 0),
    "location": "View3D > Sidebar > Zerith Bridge",
    "description": "Authenticated local TCP JSON bridge for Zerith CAD & 3D engineering",
    "category": "Development",
}}

import bpy
import hmac
import os
import queue
import secrets
import socket
import threading

{helpers}

_DEFAULT_HOST = "127.0.0.1"
_DEFAULT_PORT = {default_port}
_CONFIG_DIR_HINT = {cfg_dir_hint_lit}

_BRIDGE_LOCK = threading.Lock()
_BRIDGE_STATE = {{
    "running": False,
    "server": None,
    "thread": None,
    "token": None,
    "port": _DEFAULT_PORT,
    "queue": queue.Queue(),
    "timer_registered": False,
}}


def _zerith_config_path():
    base = os.environ.get("ZERITH_HOME", "").strip()
    if base:
        d = os.path.join(base, "blender")
    elif _CONFIG_DIR_HINT:
        d = _CONFIG_DIR_HINT
    else:
        d = os.path.join(os.path.expanduser("~"), ".zerith", "blender")
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, "bridge.json")


def _load_or_create_token(port):
    env_tok = os.environ.get("ZERITH_BLENDER_TOKEN", "").strip()
    if env_tok:
        return env_tok
    cfg_path = _zerith_config_path()
    try:
        with open(cfg_path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
            tok = str(data.get("token", "")).strip()
            if len(tok) >= 16:
                return tok
    except Exception:
        pass
    tok = secrets.token_hex(24)
    try:
        with open(cfg_path, "w", encoding="utf-8") as fh:
            json.dump({{"host": _DEFAULT_HOST, "port": port, "token": tok}}, fh, indent=2)
        if os.name == "posix":
            os.chmod(cfg_path, 0o600)
    except Exception:
        pass
    return tok


def _main_thread_pump():
    q = _BRIDGE_STATE["queue"]
    for _ in range(16):
        try:
            req, evt, box = q.get_nowait()
        except queue.Empty:
            break
        try:
            act = str(req.get("action", ""))
            if act == "ping":
                box["resp"] = {{
                    "ok": True,
                    "result": {{
                        "pong": True,
                        "blender_version": ".".join(str(x) for x in bpy.app.version),
                        "blend_filepath": bpy.data.filepath,
                    }},
                }}
            else:
                if act in ("exec_bpy", "export"):
                    try:
                        bpy.ops.ed.undo_push(message=f"Zerith before {{act}}")
                    except Exception:
                        pass
                res = _zerith_dispatch(act, req)
                if act in ("exec_bpy", "export"):
                    try:
                        bpy.ops.ed.undo_push(message=f"Zerith: {{act}}")
                    except Exception:
                        pass
                box["resp"] = {{"ok": True, "result": res}}
        except Exception as exc:
            if str(req.get("action", "")) == "exec_bpy":
                try:
                    bpy.ops.ed.undo()
                except Exception:
                    pass
            box["resp"] = {{"ok": False, "error": f"{{type(exc).__name__}}: {{exc}}"}}
        finally:
            evt.set()
    if _BRIDGE_STATE["running"]:
        return 0.05
    _BRIDGE_STATE["timer_registered"] = False
    return None


def _handle_client(conn):
    try:
        conn.settimeout(30.0)
        buf = bytearray()
        while len(buf) < 16 * 1024 * 1024:
            chunk = conn.recv(65536)
            if not chunk:
                break
            nl = chunk.find(b"\n")
            if nl != -1:
                buf.extend(chunk[:nl])
                break
            buf.extend(chunk)
        req = json.loads(buf.decode("utf-8"))
        if not isinstance(req, dict):
            raise ValueError("request must be a JSON object")
        expected = _BRIDGE_STATE["token"] or ""
        provided = str(req.get("token", ""))
        if not expected or not hmac.compare_digest(provided, expected):
            resp = {{"ok": False, "error": "unauthorized"}}
        else:
            evt = threading.Event()
            box = {{}}
            _BRIDGE_STATE["queue"].put((req, evt, box))
            if not evt.wait(timeout=120.0):
                resp = {{"ok": False, "error": "main thread dispatch timeout"}}
            else:
                resp = box.get("resp", {{"ok": False, "error": "empty response"}})
    except Exception as exc:
        resp = {{"ok": False, "error": f"{{type(exc).__name__}}: {{exc}}"}}
    try:
        out = (json.dumps(resp, ensure_ascii=False) + "\n").encode("utf-8")
        conn.sendall(out)
    except Exception:
        pass
    finally:
        try:
            conn.close()
        except Exception:
            pass


def _accept_loop(srv):
    while _BRIDGE_STATE["running"]:
        try:
            conn, _addr = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break
        t = threading.Thread(target=_handle_client, args=(conn,), daemon=True)
        t.start()


def start_bridge(port=_DEFAULT_PORT):
    with _BRIDGE_LOCK:
        if _BRIDGE_STATE["running"]:
            return
        tok = _load_or_create_token(port)
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind((_DEFAULT_HOST, int(port)))
        srv.listen(8)
        srv.settimeout(0.5)
        _BRIDGE_STATE["server"] = srv
        _BRIDGE_STATE["token"] = tok
        _BRIDGE_STATE["port"] = int(port)
        _BRIDGE_STATE["running"] = True
        if not _BRIDGE_STATE["timer_registered"]:
            bpy.app.timers.register(_main_thread_pump, persistent=True)
            _BRIDGE_STATE["timer_registered"] = True
        t = threading.Thread(target=_accept_loop, args=(srv,), daemon=True)
        _BRIDGE_STATE["thread"] = t
        t.start()


def stop_bridge():
    with _BRIDGE_LOCK:
        _BRIDGE_STATE["running"] = False
        srv = _BRIDGE_STATE.get("server")
        _BRIDGE_STATE["server"] = None
        if srv is not None:
            try:
                srv.close()
            except Exception:
                pass


class ZERITH_OT_start_bridge(bpy.types.Operator):
    bl_idname = "zerith.start_bridge"
    bl_label = "Start Zerith Bridge"

    def execute(self, context):
        start_bridge(_BRIDGE_STATE["port"])
        return {{"FINISHED"}}


class ZERITH_OT_stop_bridge(bpy.types.Operator):
    bl_idname = "zerith.stop_bridge"
    bl_label = "Stop Zerith Bridge"

    def execute(self, context):
        stop_bridge()
        return {{"FINISHED"}}


class ZERITH_PT_bridge_panel(bpy.types.Panel):
    bl_label = "Zerith Bridge"
    bl_idname = "ZERITH_PT_bridge_panel"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = "Zerith Bridge"

    def draw(self, context):
        layout = self.layout
        running = _BRIDGE_STATE["running"]
        layout.label(text=f"Status: {{'Running' if running else 'Stopped'}}")
        layout.label(text=f"Bind: {{_DEFAULT_HOST}}:{{_BRIDGE_STATE['port']}}")
        tok = _BRIDGE_STATE.get("token") or ""
        if tok:
            layout.label(text=f"Token: {{tok[:8]}}...{{tok[-6:]}}")
        if running:
            layout.operator("zerith.stop_bridge", icon="PAUSE")
        else:
            layout.operator("zerith.start_bridge", icon="PLAY")


_CLASSES = (
    ZERITH_OT_start_bridge,
    ZERITH_OT_stop_bridge,
    ZERITH_PT_bridge_panel,
)


def register():
    for cls in _CLASSES:
        bpy.utils.register_class(cls)
    start_bridge(_DEFAULT_PORT)


def unregister():
    stop_bridge()
    for cls in reversed(_CLASSES):
        bpy.utils.unregister_class(cls)


if __name__ == "__main__":
    register()
"####,
        helpers = BPY_HELPERS_PY,
        default_port = default_port,
        cfg_dir_hint_lit = python_string_literal(&cfg_dir_hint),
    )
}

fn run_install_addon(args: &Value, ctx: &CallCtx<'_>) -> Result<String> {
    let endpoint = resolve_bridge_endpoint(args)?;
    let out = ctx.resolve(opt_str(args, "output_path").unwrap_or(DEFAULT_ADDON_REL_PATH));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let addon_src = build_addon_source(endpoint.port);
    fs::write(&out, &addon_src)?;

    let cfg_path = bridge_config_path();
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let token = match endpoint.token {
        Some(t) => t,
        None => {
            let tok = generate_hex_token();
            let cfg = json!({
                "host": endpoint.host,
                "port": endpoint.port,
                "token": tok,
            });
            fs::write(
                &cfg_path,
                serde_json::to_string_pretty(&cfg).unwrap_or_default(),
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o600));
            }
            tok
        }
    };

    let token_head: String = token.chars().take(6).collect();
    let token_tail: String = token
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let info = json!({
        "addon_path": out.display().to_string(),
        "bridge_config_path": cfg_path.display().to_string(),
        "host": endpoint.host,
        "port": endpoint.port,
        "token_preview": format!("{token_head}...{token_tail}"),
        "install_instructions": [
            "No Blender: Edit -> Preferences -> Add-ons -> Install... e selecione o arquivo addon_path",
            "Ative o checkbox 'Development: Zerith Blender Bridge'",
            "Ou execute direto na inicialização: blender --python <addon_path>"
        ]
    });
    Ok(serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.to_string()))
}

fn generate_hex_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut out = String::with_capacity(48);
    for idx in 0..3u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(idx);
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn run_batch(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let steps = validate_batch_steps(args)?;
    let mode = parse_run_mode(args)?;
    let blend_file = resolve_existing_blend(args, ctx)?;
    let endpoint = resolve_bridge_endpoint(args)?;

    if mode == RunMode::Live && blend_file.is_some() {
        return Err(ToolError::BadArguments(
            "`blend_file` só é suportado em `mode: \"headless\"` ou `mode: \"auto\"`".into(),
        ));
    }

    let mut normalized_steps = Vec::with_capacity(steps.len());
    for step in steps {
        let act = step["action"].as_str().unwrap_or("");
        if matches!(act, "status" | "install_addon") {
            return run_batch_sequential(steps, ctx, exec);
        }
        normalized_steps.push(normalize_request_for_blender(step, ctx)?);
    }

    let try_live = match mode {
        RunMode::Live => true,
        RunMode::Headless => false,
        RunMode::Auto => blend_file.is_none() && endpoint.token.is_some(),
    };

    if try_live {
        if let Some(token) = endpoint.token.as_deref() {
            let mut live_results: Vec<(String, Value)> = Vec::with_capacity(normalized_steps.len());
            let mut live_failed: Option<ToolError> = None;
            for (idx, step_req) in normalized_steps.iter().enumerate() {
                let act = step_req["action"].as_str().unwrap_or("");
                match send_bridge_request(&endpoint.host, endpoint.port, token, step_req, exec) {
                    Ok(res) => live_results.push((act.to_string(), res)),
                    Err(e) => {
                        live_failed = Some(ToolError::Failed(format!(
                            "passo {} ({act}) no bridge falhou: {e}",
                            idx + 1
                        )));
                        break;
                    }
                }
            }
            match live_failed {
                None => return format_batch_outputs(&live_results),
                Some(e) if mode == RunMode::Live || !live_results.is_empty() => return Err(e),
                Some(_) => {}
            }
        } else if mode == RunMode::Live {
            return Err(ToolError::Failed(
                "nenhum token configurado para o bridge Blender ao vivo".into(),
            ));
        }
    }

    let bin = find_blender_binary().ok_or_else(|| {
        ToolError::Failed(
            "binário do Blender não encontrado (defina `ZERITH_BLENDER_BIN` ou adicione `blender` ao PATH)"
                .into(),
        )
    })?;
    let script = build_headless_batch_script(&normalized_steps)?;
    let batch_val = run_headless_python(&bin, blend_file.as_deref(), &script, exec)?;
    let step_arr = batch_val["steps"]
        .as_array()
        .ok_or_else(|| ToolError::Failed("batch headless não retornou array `steps`".into()))?;
    let mut pairs = Vec::with_capacity(step_arr.len());
    for item in step_arr {
        let act = item["action"].as_str().unwrap_or("?").to_string();
        pairs.push((act, item["result"].clone()));
    }
    format_batch_outputs(&pairs)
}

fn run_batch_sequential(steps: &[Value], ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let mut out = Vec::with_capacity(steps.len());
    let mut marker_line: Option<String> = None;
    for (idx, step) in steps.iter().enumerate() {
        let act = step["action"].as_str().unwrap_or("?");
        match run_blender(step, ctx, exec) {
            Ok(text) => {
                if act == "render_views" {
                    marker_line = text
                        .lines()
                        .next()
                        .filter(|l| l.starts_with(crate::IMAGE_MARKER))
                        .map(str::to_string)
                        .or(marker_line);
                }
                out.push(format!("### passo {} ({act})\n{text}", idx + 1));
            }
            Err(err) => {
                return Err(ToolError::Failed(format!(
                    "passo {} ({act}) falhou: {err}",
                    idx + 1
                )));
            }
        }
    }
    let body = out.join("\n\n");
    Ok(match marker_line {
        Some(m) => format!("{m}\n{body}"),
        None => body,
    })
}

fn format_batch_outputs(steps: &[(String, Value)]) -> Result<String> {
    let mut sections = Vec::with_capacity(steps.len());
    let mut first_image_marker: Option<String> = None;
    for (idx, (act, val)) in steps.iter().enumerate() {
        let formatted = format_action_output(act, val)?;
        if act == "render_views" {
            first_image_marker = formatted
                .lines()
                .next()
                .filter(|l| l.starts_with(crate::IMAGE_MARKER))
                .map(str::to_string)
                .or(first_image_marker);
        }
        sections.push(format!("### passo {} ({act})\n{formatted}", idx + 1));
    }
    let joined = sections.join("\n\n");
    Ok(match first_image_marker {
        Some(marker) => format!("{marker}\n{joined}"),
        None => joined,
    })
}

const BPY_HELPERS_PY: &str = r####"import contextlib
import io
import json
import math
import os
import shutil
import sys
import tempfile
import bmesh
import bpy
import mathutils
from mathutils.bvhtree import BVHTree

_ZERITH_DENSITIES_G_CM3 = {
    "PLA": 1.24,
    "PETG": 1.27,
    "ABS": 1.04,
    "Nylon": 1.14,
    "Aluminum_6061": 2.70,
    "Steel_304": 7.93,
}


def _zerith_bu_to_mm(scene):
    us = scene.unit_settings
    if us.system in ("METRIC", "IMPERIAL"):
        return float(us.scale_length or 1.0) * 1000.0
    return float(us.scale_length or 1.0)


def _zerith_mesh_objects(scene, object_name=None, object_names=None):
    if object_name:
        obj = bpy.data.objects.get(object_name)
        if obj is None:
            raise ValueError(f"objeto não encontrado: {object_name}")
        return [obj]
    if object_names:
        objs = []
        for nm in object_names:
            obj = bpy.data.objects.get(nm)
            if obj is None:
                raise ValueError(f"objeto não encontrado: {nm}")
            objs.append(obj)
        return objs
    return [o for o in scene.objects if o.type == "MESH"]


def _zerith_world_bbox(eval_obj, bu_to_mm):
    mw = eval_obj.matrix_world
    corners = [mw @ mathutils.Vector(c) for c in eval_obj.bound_box]
    bmin = [min(c[i] for c in corners) * bu_to_mm for i in range(3)]
    bmax = [max(c[i] for c in corners) * bu_to_mm for i in range(3)]
    dims = [bmax[i] - bmin[i] for i in range(3)]
    return bmin, bmax, dims


def _zerith_eval_bmesh(obj, depsgraph):
    eval_obj = obj.evaluated_get(depsgraph)
    mesh = eval_obj.to_mesh()
    bm = bmesh.new()
    try:
        bm.from_mesh(mesh)
    finally:
        eval_obj.to_mesh_clear()
    bm.transform(eval_obj.matrix_world)
    bm.normal_update()
    bm.verts.ensure_lookup_table()
    bm.edges.ensure_lookup_table()
    bm.faces.ensure_lookup_table()
    return eval_obj, bm


def _zerith_inspect_single(obj, depsgraph, bu_to_mm):
    if obj.type != "MESH":
        raise ValueError(f"objeto `{obj.name}` não é MESH (tipo={obj.type})")
    eval_obj, bm = _zerith_eval_bmesh(obj, depsgraph)
    try:
        bmin, bmax, dims = _zerith_world_bbox(eval_obj, bu_to_mm)
        non_manifold = sum(1 for e in bm.edges if not e.is_manifold)
        boundary = sum(1 for e in bm.edges if e.is_boundary)
        zero_faces = sum(1 for f in bm.faces if f.calc_area() <= 1e-12)
        inconsistent = 0
        for e in bm.edges:
            if len(e.link_faces) == 2 and len(e.link_loops) == 2:
                if e.link_loops[0].vert == e.link_loops[1].vert:
                    inconsistent += 1
        is_manifold = len(bm.verts) > 0 and len(bm.faces) > 0 and non_manifold == 0 and boundary == 0
        signed_vol = bm.calc_volume(signed=True) if len(bm.faces) > 0 else 0.0
        inverted_normals = bool(is_manifold and inconsistent == 0 and signed_vol < -1e-12)
        vol_mm3 = abs(signed_vol) * (bu_to_mm ** 3)
        vol_cm3 = vol_mm3 / 1000.0
        area_mm2 = sum(f.calc_area() for f in bm.faces) * (bu_to_mm ** 2)
        scale_applied = all(abs(s - 1.0) < 1e-4 for s in obj.scale)
        mass_g = {
            mat: round(vol_cm3 * dens, 3)
            for mat, dens in _ZERITH_DENSITIES_G_CM3.items()
        }
        return {
            "name": obj.name,
            "dimensions_mm": {
                "x": round(dims[0], 4),
                "y": round(dims[1], 4),
                "z": round(dims[2], 4),
            },
            "world_bbox_min_mm": [round(v, 4) for v in bmin],
            "world_bbox_max_mm": [round(v, 4) for v in bmax],
            "volume_mm3": round(vol_mm3, 4),
            "volume_cm3": round(vol_cm3, 5),
            "surface_area_mm2": round(area_mm2, 4),
            "is_manifold": is_manifold,
            "non_manifold_edges": non_manifold,
            "boundary_edges": boundary,
            "zero_area_faces": zero_faces,
            "inconsistent_normal_edges": inconsistent,
            "inverted_normals": inverted_normals,
            "vertices": len(bm.verts),
            "polygons": len(bm.faces),
            "scale": [round(float(s), 5) for s in obj.scale],
            "scale_applied": scale_applied,
            "scale_warning": None if scale_applied else "scale != (1,1,1); aplique transformações antes de exportar",
            "estimated_mass_g": mass_g,
        }, bm
    except Exception:
        bm.free()
        raise


def _zerith_scene_info(_req):
    scene = bpy.context.scene
    depsgraph = bpy.context.evaluated_depsgraph_get()
    bu_to_mm = _zerith_bu_to_mm(scene)
    objs = []
    global_min = None
    global_max = None
    for obj in scene.objects:
        eval_obj = obj.evaluated_get(depsgraph)
        bmin, bmax, dims = _zerith_world_bbox(eval_obj, bu_to_mm)
        if obj.type == "MESH":
            if global_min is None:
                global_min = list(bmin)
                global_max = list(bmax)
            else:
                global_min = [min(global_min[i], bmin[i]) for i in range(3)]
                global_max = [max(global_max[i], bmax[i]) for i in range(3)]
        verts = len(obj.data.vertices) if obj.type == "MESH" and obj.data else 0
        polys = len(obj.data.polygons) if obj.type == "MESH" and obj.data else 0
        objs.append({
            "name": obj.name,
            "type": obj.type,
            "location_mm": [round(float(v) * bu_to_mm, 4) for v in obj.location],
            "rotation_deg": [round(math.degrees(float(r)), 3) for r in obj.rotation_euler],
            "scale": [round(float(s), 5) for s in obj.scale],
            "dimensions_mm": [round(d, 4) for d in dims],
            "collections": [c.name for c in obj.users_collection],
            "modifiers": [m.name for m in obj.modifiers],
            "vertices": verts,
            "polygons": polys,
        })
    return {
        "blender_version": ".".join(str(x) for x in bpy.app.version),
        "scene": scene.name,
        "unit_system": scene.unit_settings.system,
        "scale_length": float(scene.unit_settings.scale_length),
        "bu_to_mm": bu_to_mm,
        "object_count": len(objs),
        "objects": objs,
        "materials": [m.name for m in bpy.data.materials],
        "global_bbox_min_mm": [round(v, 4) for v in global_min] if global_min else None,
        "global_bbox_max_mm": [round(v, 4) for v in global_max] if global_max else None,
    }


def _zerith_inspect_object(req):
    scene = bpy.context.scene
    depsgraph = bpy.context.evaluated_depsgraph_get()
    bu_to_mm = _zerith_bu_to_mm(scene)
    obj_name = req.get("object_name")
    if obj_name:
        targets = _zerith_mesh_objects(scene, object_name=obj_name)
    elif bpy.context.selected_objects:
        targets = [o for o in bpy.context.selected_objects if o.type == "MESH"]
    elif bpy.context.active_object and bpy.context.active_object.type == "MESH":
        targets = [bpy.context.active_object]
    else:
        targets = _zerith_mesh_objects(scene)
    if not targets:
        raise ValueError("nenhum objeto MESH encontrado para inspeção")
    reports = []
    for obj in targets:
        rep, bm = _zerith_inspect_single(obj, depsgraph, bu_to_mm)
        bm.free()
        reports.append(rep)
    return reports[0] if len(reports) == 1 else {"objects": reports}


def _zerith_cad_check(req):
    scene = bpy.context.scene
    depsgraph = bpy.context.evaluated_depsgraph_get()
    bu_to_mm = _zerith_bu_to_mm(scene)
    min_wall_mm = float(req.get("min_wall_mm", 0.4))
    targets = _zerith_mesh_objects(
        scene,
        object_name=req.get("object_name"),
        object_names=req.get("object_names"),
    )
    if not targets:
        return {"passed": False, "objects_checked": [], "issues": [{"object": None, "kind": "empty_scene", "message": "nenhum objeto MESH na cena"}], "overlaps": []}

    issues = []
    checked = []
    bvh_entries = []
    try:
        for obj in targets:
            rep, bm = _zerith_inspect_single(obj, depsgraph, bu_to_mm)
            checked.append(rep)
            if not rep["is_manifold"]:
                issues.append({
                    "object": obj.name,
                    "kind": "non_manifold",
                    "message": f"malha não-manifold (non_manifold_edges={rep['non_manifold_edges']}, boundary_edges={rep['boundary_edges']})",
                })
            if not rep["scale_applied"]:
                issues.append({
                    "object": obj.name,
                    "kind": "unapplied_scale",
                    "message": f"escala não aplicada {rep['scale']}; rode transform_apply(scale=True)",
                })
            if rep["zero_area_faces"] > 0:
                issues.append({
                    "object": obj.name,
                    "kind": "degenerate_faces",
                    "message": f"{rep['zero_area_faces']} face(s) com área zero",
                })
            if rep["inconsistent_normal_edges"] > 0 or rep["inverted_normals"]:
                issues.append({
                    "object": obj.name,
                    "kind": "bad_normals",
                    "message": f"normais inconsistentes/invertidas (inconsistent_edges={rep['inconsistent_normal_edges']}, inverted={rep['inverted_normals']})",
                })
            dims = rep["dimensions_mm"]
            smallest_dim = min(dims["x"], dims["y"], dims["z"])
            if smallest_dim < min_wall_mm:
                issues.append({
                    "object": obj.name,
                    "kind": "thin_dimension",
                    "message": f"menor dimensão ({smallest_dim:.3f} mm) abaixo do mínimo ({min_wall_mm:.3f} mm)",
                })
            if len(bm.verts) > 0 and len(bm.faces) > 0:
                # World-space BVHTree via transformed bmesh (or BVHTree.FromObject for local space)
                bvh = BVHTree.FromBMesh(bm)
                bvh_entries.append((obj.name, rep["world_bbox_min_mm"], rep["world_bbox_max_mm"], bvh))
            bm.free()

        overlaps = []
        for i in range(len(bvh_entries)):
            name_a, min_a, max_a, bvh_a = bvh_entries[i]
            for j in range(i + 1, len(bvh_entries)):
                name_b, min_b, max_b, bvh_b = bvh_entries[j]
                if not all(max_a[k] > min_b[k] + 1e-4 and max_b[k] > min_a[k] + 1e-4 for k in range(3)):
                    continue
                pairs = bvh_a.overlap(bvh_b)
                if pairs:
                    overlaps.append({
                        "objects": [name_a, name_b],
                        "overlapping_face_pairs": len(pairs),
                    })
                    issues.append({
                        "object": f"{name_a} <-> {name_b}",
                        "kind": "collision_overlap",
                        "message": f"sobreposição detectada por BVHTree ({len(pairs)} pares de faces)",
                    })

        return {
            "passed": len(issues) == 0,
            "objects_checked": [c["name"] for c in checked],
            "metrics": checked,
            "overlaps": overlaps,
            "issues": issues,
        }
    except Exception:
        raise


def _zerith_exec_bpy(req):
    code = req.get("code")
    if not isinstance(code, str) or not code.strip():
        raise ValueError("exec_bpy exige `code` não vazio")
    buf = io.StringIO()
    ns = {
        "bpy": bpy,
        "bmesh": bmesh,
        "mathutils": mathutils,
        "math": math,
        "json": json,
        "__name__": "__main__",
    }
    with contextlib.redirect_stdout(buf):
        exec(compile(code, "<zerith_exec_bpy>", "exec"), ns)
    save_blend = req.get("save_blend")
    if save_blend:
        os.makedirs(os.path.dirname(os.path.abspath(save_blend)), exist_ok=True)
        bpy.ops.wm.save_as_mainfile(filepath=save_blend)
    return {
        "stdout": buf.getvalue(),
        "saved_blend": save_blend,
        "object_count": len(bpy.context.scene.objects),
    }


def _zerith_stitch_2x2(quad_paths, out_path, res):
    total_w = res * 2
    total_h = res * 2
    loaded = []
    grid = bpy.data.images.new("_zerith_grid_2x2", width=total_w, height=total_h, alpha=True)
    try:
        try:
            grid.colorspace_settings.name = "sRGB"
        except Exception:
            pass
        for qp in quad_paths:
            img = bpy.data.images.load(qp, check_existing=False)
            loaded.append(img)

        try:
            import numpy as np
            canvas = np.zeros((total_h, total_w, 4), dtype=np.float32)
            # Blender image buffers are bottom-up: row 0 is bottom.
            placements = [
                (loaded[0], res, 0),    # top-left: front
                (loaded[1], res, res),  # top-right: right
                (loaded[2], 0, 0),      # bottom-left: top
                (loaded[3], 0, res),    # bottom-right: iso_45
            ]
            for img, row_off, col_off in placements:
                arr = np.array(img.pixels[:], dtype=np.float32).reshape((res, res, 4))
                canvas[row_off:row_off + res, col_off:col_off + res, :] = arr
            grid.pixels.foreach_set(canvas.ravel())
        except ImportError:
            buf = [0.0] * (total_w * total_h * 4)
            placements = [
                (loaded[0], res, 0),
                (loaded[1], res, res),
                (loaded[2], 0, 0),
                (loaded[3], 0, res),
            ]
            for img, row_off, col_off in placements:
                px = list(img.pixels[:])
                for r in range(res):
                    src_start = r * res * 4
                    dst_start = ((row_off + r) * total_w + col_off) * 4
                    buf[dst_start:dst_start + res * 4] = px[src_start:src_start + res * 4]
            grid.pixels[:] = buf

        os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
        grid.filepath_raw = out_path
        grid.file_format = "PNG"
        grid.save()
    finally:
        for img in loaded:
            try:
                bpy.data.images.remove(img)
            except Exception:
                pass
        try:
            bpy.data.images.remove(grid)
        except Exception:
            pass


def _zerith_render_views(req):
    scene = bpy.context.scene
    depsgraph = bpy.context.evaluated_depsgraph_get()
    targets = _zerith_mesh_objects(
        scene,
        object_name=req.get("object_name"),
        object_names=req.get("object_names"),
    )
    if not targets:
        raise ValueError("nenhum objeto MESH encontrado para render_views")

    out_path = req.get("output_path")
    if not out_path:
        raise ValueError("render_views exige output_path")

    res = max(256, min(1024, int(req.get("resolution", 512))))
    samples = max(1, min(256, int(req.get("samples", 16))))
    req_engine = str(req.get("engine", "workbench")).lower()

    gmin = [float("inf")] * 3
    gmax = [float("-inf")] * 3
    for obj in targets:
        eval_obj = obj.evaluated_get(depsgraph)
        mw = eval_obj.matrix_world
        for corner in eval_obj.bound_box:
            wc = mw @ mathutils.Vector(corner)
            for i in range(3):
                gmin[i] = min(gmin[i], wc[i])
                gmax[i] = max(gmax[i], wc[i])

    center = mathutils.Vector([(gmin[i] + gmax[i]) * 0.5 for i in range(3)])
    size = mathutils.Vector([max(gmax[i] - gmin[i], 1e-4) for i in range(3)])
    max_dim = max(size.x, size.y, size.z, 1e-3)
    dist = max_dim * 3.0

    orig_cam = scene.camera
    orig_engine = scene.render.engine
    orig_rx = scene.render.resolution_x
    orig_ry = scene.render.resolution_y
    orig_pct = scene.render.resolution_percentage
    orig_fmt = scene.render.image_settings.file_format
    orig_fp = scene.render.filepath

    cam_data = bpy.data.cameras.new("_zerith_cam")
    cam_data.type = "ORTHO"
    cam_data.ortho_scale = max_dim * 1.45
    cam_data.clip_start = max_dim * 0.001
    cam_data.clip_end = max_dim * 25.0
    cam_obj = bpy.data.objects.new("_zerith_cam_obj", cam_data)
    scene.collection.objects.link(cam_obj)

    temp_light = None
    if not any(o.type == "LIGHT" for o in scene.objects):
        ldata = bpy.data.lights.new("_zerith_sun", type="SUN")
        ldata.energy = 3.0
        temp_light = bpy.data.objects.new("_zerith_sun_obj", ldata)
        temp_light.rotation_euler = (math.radians(45.0), math.radians(15.0), math.radians(35.0))
        scene.collection.objects.link(temp_light)

    tmp_dir = tempfile.mkdtemp(prefix="zerith_views_")
    used_engine = None
    try:
        scene.camera = cam_obj
        scene.render.resolution_x = res
        scene.render.resolution_y = res
        scene.render.resolution_percentage = 100
        scene.render.image_settings.file_format = "PNG"

        eevee_id = "BLENDER_EEVEE_NEXT" if bpy.app.version >= (4, 2, 0) else "BLENDER_EEVEE"
        engine_map = {
            "workbench": "BLENDER_WORKBENCH",
            "eevee": eevee_id,
            "cycles": "CYCLES",
        }
        preferred = engine_map.get(req_engine, "BLENDER_WORKBENCH")
        candidates = [preferred]
        if preferred != "CYCLES":
            candidates.append("CYCLES")

        views = [
            ("front", mathutils.Vector((0.0, -1.0, 0.0))),
            ("right", mathutils.Vector((1.0, 0.0, 0.0))),
            ("top", mathutils.Vector((0.0, 0.0, 1.0))),
            ("iso_45", mathutils.Vector((1.0, -1.0, 0.85)).normalized()),
        ]
        quad_paths = [os.path.join(tmp_dir, f"{name}.png") for name, _ in views]

        last_err = None
        for eng in candidates:
            try:
                scene.render.engine = eng
                if eng == "CYCLES":
                    scene.cycles.device = "CPU"
                    scene.cycles.samples = samples
                elif eng == "BLENDER_WORKBENCH":
                    scene.display.shading.light = "STUDIO"
                    scene.display.shading.show_cavity = True
                for (name, vec), qp in zip(views, quad_paths):
                    cam_obj.location = center + vec * dist
                    direction = (center - cam_obj.location).normalized()
                    up_axis = "Y"
                    cam_obj.rotation_euler = direction.to_track_quat("-Z", up_axis).to_euler()
                    scene.render.filepath = qp
                    bpy.ops.render.render(write_still=True)
                used_engine = eng
                break
            except Exception as exc:
                last_err = exc
        if used_engine is None:
            raise RuntimeError(f"falha ao renderizar vistas: {last_err}")

        _zerith_stitch_2x2(quad_paths, out_path, res)
        bu_to_mm = _zerith_bu_to_mm(scene)
        return {
            "output_path": out_path,
            "grid": "2x2",
            "views": ["front", "right", "top", "iso_45"],
            "quadrant_resolution": res,
            "image_resolution": res * 2,
            "engine": used_engine,
            "objects": [o.name for o in targets],
            "bbox_size_mm": [round(size[i] * bu_to_mm, 4) for i in range(3)],
        }
    finally:
        scene.camera = orig_cam
        scene.render.engine = orig_engine
        scene.render.resolution_x = orig_rx
        scene.render.resolution_y = orig_ry
        scene.render.resolution_percentage = orig_pct
        scene.render.image_settings.file_format = orig_fmt
        scene.render.filepath = orig_fp
        bpy.data.objects.remove(cam_obj, do_unlink=True)
        bpy.data.cameras.remove(cam_data)
        if temp_light is not None:
            ldata_rm = temp_light.data
            bpy.data.objects.remove(temp_light, do_unlink=True)
            bpy.data.lights.remove(ldata_rm)
        shutil.rmtree(tmp_dir, ignore_errors=True)


def _zerith_export(req):
    scene = bpy.context.scene
    out_path = req.get("output_path")
    fmt = str(req.get("format", "")).lower()
    if not out_path or not fmt:
        raise ValueError("export exige output_path e format")

    apply_tf = bool(req.get("apply_transforms", True))
    unit_scale = float(req.get("unit_scale", 1.0))
    targets = _zerith_mesh_objects(
        scene,
        object_name=req.get("object_name"),
        object_names=req.get("object_names"),
    )
    if not targets:
        raise ValueError("nenhum objeto MESH selecionado/encontrado para exportar")

    if bpy.context.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")
    bpy.ops.object.select_all(action="DESELECT")
    for obj in targets:
        obj.select_set(True)
    bpy.context.view_layer.objects.active = targets[0]

    if apply_tf:
        bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)

    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    if fmt == "stl":
        if hasattr(bpy.ops.wm, "stl_export"):
            bpy.ops.wm.stl_export(filepath=out_path, export_selected_objects=True, global_scale=unit_scale)
        else:
            bpy.ops.export_mesh.stl(filepath=out_path, use_selection=True, global_scale=unit_scale)
    elif fmt == "obj":
        if hasattr(bpy.ops.wm, "obj_export"):
            bpy.ops.wm.obj_export(filepath=out_path, export_selected_objects=True, global_scale=unit_scale)
        else:
            bpy.ops.export_scene.obj(filepath=out_path, use_selection=True, global_scale=unit_scale)
    elif fmt == "glb":
        bpy.ops.export_scene.gltf(filepath=out_path, use_selection=True, export_format="GLB")
    elif fmt == "gltf":
        bpy.ops.export_scene.gltf(filepath=out_path, use_selection=True, export_format="GLTF_SEPARATE")
    elif fmt == "fbx":
        bpy.ops.export_scene.fbx(filepath=out_path, use_selection=True, global_scale=unit_scale)
    elif fmt == "3mf":
        if not hasattr(bpy.ops.export_mesh, "threemf"):
            try:
                bpy.ops.preferences.addon_enable(module="io_mesh_3mf")
            except Exception:
                pass
        if not hasattr(bpy.ops.export_mesh, "threemf"):
            raise RuntimeError("addon 3MF (io_mesh_3mf) não está disponível neste Blender; exporte como stl ou glb")
        bpy.ops.export_mesh.threemf(filepath=out_path, use_selection=True, global_scale=unit_scale)
    else:
        raise ValueError(f"formato não suportado: {fmt}")

    if not os.path.isfile(out_path) or os.path.getsize(out_path) == 0:
        raise RuntimeError(f"arquivo exportado vazio ou ausente: {out_path}")

    return {
        "output_path": out_path,
        "format": fmt,
        "bytes": os.path.getsize(out_path),
        "objects_exported": [o.name for o in targets],
        "apply_transforms": apply_tf,
        "unit_scale": unit_scale,
    }


def _zerith_dispatch(action, req):
    if action == "scene_info":
        return _zerith_scene_info(req)
    if action == "inspect_object":
        return _zerith_inspect_object(req)
    if action == "cad_check":
        return _zerith_cad_check(req)
    if action == "exec_bpy":
        return _zerith_exec_bpy(req)
    if action == "render_views":
        return _zerith_render_views(req)
    if action == "export":
        return _zerith_export(req)
    raise ValueError(f"ação desconhecida no Blender: {action}")
"####;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;
    use zerith_providers::Cancel;
    use zerith_security::{preset, Mode, Policy};

    use crate::Host;

    fn test_ctx(workspace: &Path) -> CallCtx<'static> {
        let policy = Box::leak(Box::new(Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            workspace,
        )));
        let ws = Box::leak(Box::new(workspace.to_path_buf()));
        CallCtx {
            workspace: ws,
            policy,
            memory: None,
            scope: None,
        }
    }

    #[test]
    fn blender_version_hint_and_candidate_sorting_picks_highest() {
        assert_eq!(
            parse_blender_version_hint(Path::new(
                r"C:\Program Files\Blender Foundation\Blender 4.3\blender.exe"
            )),
            (4, 3, 0)
        );
        assert_eq!(
            parse_blender_version_hint(Path::new("/opt/blender-4.2.1-linux-x64/blender")),
            (4, 2, 1)
        );
        assert_eq!(
            parse_blender_version_hint(Path::new("/usr/bin/blender")),
            (0, 0, 0)
        );

        let picked = pick_best_blender_candidate(vec![
            PathBuf::from("/usr/bin/blender"),
            PathBuf::from("/opt/blender-3.6.12/blender"),
            PathBuf::from("/opt/blender-4.3.2/blender"),
            PathBuf::from("/opt/blender-4.1.0/blender"),
        ]);
        assert_eq!(picked, Some(PathBuf::from("/opt/blender-4.3.2/blender")));
        assert_eq!(pick_best_blender_candidate(Vec::new()), None);
    }

    #[test]
    fn batch_validates_empty_overflow_nesting_and_actions() {
        assert!(validate_batch_steps(&json!({"action": "batch"})).is_err());
        assert!(validate_batch_steps(&json!({"action": "batch", "steps": []})).is_err());
        let too_many: Vec<Value> = (0..=MAX_BATCH_STEPS)
            .map(|_| json!({"action": "scene_info"}))
            .collect();
        assert!(validate_batch_steps(&json!({"action": "batch", "steps": too_many})).is_err());
        assert!(validate_batch_steps(&json!({
            "action": "batch",
            "steps": [{"action": "batch", "steps": [{"action": "scene_info"}]}]
        }))
        .is_err());
        assert!(validate_batch_steps(&json!({
            "action": "batch",
            "steps": [{"action": "unknown_op"}]
        }))
        .is_err());
        let batch_args = json!({
            "action": "batch",
            "steps": [
                {"action": "exec_bpy", "code": "pass"},
                {"action": "cad_check"},
                {"action": "render_views"}
            ]
        });
        let ok = validate_batch_steps(&batch_args).unwrap();
        assert_eq!(ok.len(), 3);
    }

    #[test]
    fn export_and_render_views_validate_extensions_and_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());

        assert_eq!(
            resolve_export_format(&json!({}), Path::new("part.STL")).unwrap(),
            "stl"
        );
        assert_eq!(
            resolve_export_format(&json!({"format": "3mf"}), Path::new("part.3mf")).unwrap(),
            "3mf"
        );
        assert_eq!(
            resolve_export_format(&json!({}), Path::new("assembly.glb")).unwrap(),
            "glb"
        );
        assert!(resolve_export_format(&json!({}), Path::new("part.exe")).is_err());
        assert!(resolve_export_format(&json!({}), Path::new("no_ext")).is_err());

        let auto_png =
            resolve_render_output_path(&json!({"action": "render_views"}), &ctx).unwrap();
        assert!(auto_png.starts_with(tmp.path()));
        assert_eq!(auto_png.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(resolve_render_output_path(&json!({"output_path": "out.jpg"}), &ctx).is_err());
    }

    #[test]
    fn tool_spec_and_concurrency_safe_match_contract() {
        let tools = all();
        assert_eq!(tools.len(), 1);
        let t = &tools[0];
        assert_eq!(t.spec().name, "blender");
        assert_eq!(t.spec().permission, Permission::Write);
        assert!(t.concurrency_safe(&json!({"action": "status"})));
        assert!(t.concurrency_safe(&json!({"action": "scene_info"})));
        assert!(t.concurrency_safe(&json!({"action": "inspect_object"})));
        assert!(t.concurrency_safe(&json!({"action": "cad_check"})));
        assert!(!t.concurrency_safe(&json!({"action": "exec_bpy"})));
        assert!(!t.concurrency_safe(&json!({"action": "export"})));
        assert!(!t.concurrency_safe(&json!({"action": "render_views"})));
        assert!(!t.concurrency_safe(&json!({"action": "batch"})));

        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        assert!(t
            .action(&json!({"action": "cad_check"}), &ctx)
            .unwrap()
            .is_none());
        assert_eq!(
            t.action(&json!({"action": "export", "output_path": "out.stl"}), &ctx)
                .unwrap(),
            Some(Action::Write(tmp.path().join("out.stl")))
        );
        assert!(matches!(
            t.action(&json!({"action": "exec_bpy", "code": "pass"}), &ctx)
                .unwrap(),
            Some(Action::Shell(_))
        ));
    }

    #[test]
    fn loopback_mock_bridge_enforces_token_and_roundtrips_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let secret = "test_secret_token_abc123";

        let handle = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(line.trim()).unwrap();
                let resp = if req["token"].as_str() != Some(secret) {
                    json!({"ok": false, "error": "unauthorized"})
                } else {
                    match req["action"].as_str() {
                        Some("scene_info") => json!({
                            "ok": true,
                            "result": {
                                "blender_version": "4.2.0",
                                "unit_system": "METRIC",
                                "scale_length": 0.001,
                                "object_count": 1
                            }
                        }),
                        Some("inspect_object") => json!({
                            "ok": true,
                            "result": {
                                "name": req["object_name"],
                                "is_manifold": true,
                                "volume_cm3": 12.5,
                                "estimated_mass_g": {"PLA": 15.5}
                            }
                        }),
                        Some("exec_bpy") => json!({
                            "ok": true,
                            "result": {
                                "stdout": "created box\n",
                                "object_count": 2
                            }
                        }),
                        _ => json!({"ok": false, "error": "unknown action"}),
                    }
                };
                let msg = format!("{}\n", serde_json::to_string(&resp).unwrap());
                stream.write_all(msg.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });

        let cancel = Cancel::default();
        let host = Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(10),
            timeout: Duration::from_secs(10),
            host: &host,
        };
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let tool = &all()[0];

        // 1. Wrong token rejected
        let bad = send_bridge_request(
            "127.0.0.1",
            port,
            "wrong_token",
            &json!({"action": "scene_info"}),
            &exec,
        );
        assert!(bad.is_err());

        // 2. Authenticated scene_info via BlenderTool::run
        let scene_out = tool
            .run(
                &json!({
                    "action": "scene_info",
                    "mode": "live",
                    "host": "127.0.0.1",
                    "port": port,
                    "token": secret
                }),
                &ctx,
                &exec,
            )
            .unwrap();
        let scene_json: Value = serde_json::from_str(&scene_out).unwrap();
        assert_eq!(scene_json["blender_version"], "4.2.0");
        assert_eq!(scene_json["object_count"], 1);

        // 3. Authenticated inspect_object
        let inspect_out = tool
            .run(
                &json!({
                    "action": "inspect_object",
                    "mode": "live",
                    "port": port,
                    "token": secret,
                    "object_name": "Enclosure"
                }),
                &ctx,
                &exec,
            )
            .unwrap();
        let inspect_json: Value = serde_json::from_str(&inspect_out).unwrap();
        assert_eq!(inspect_json["name"], "Enclosure");
        assert_eq!(inspect_json["is_manifold"], true);

        // 4. Authenticated exec_bpy
        let exec_out = tool
            .run(
                &json!({
                    "action": "exec_bpy",
                    "mode": "live",
                    "port": port,
                    "token": secret,
                    "code": "print('created box')"
                }),
                &ctx,
                &exec,
            )
            .unwrap();
        let exec_json: Value = serde_json::from_str(&exec_out).unwrap();
        assert_eq!(exec_json["stdout"], "created box\n");
        assert_eq!(exec_json["object_count"], 2);

        handle.join().unwrap();
    }

    #[test]
    fn generated_addon_and_scripts_contain_required_safety_and_cad_primitives() {
        let addon = build_addon_source(DEFAULT_BRIDGE_PORT);
        assert!(addon.contains("hmac.compare_digest"));
        assert!(addon.contains("127.0.0.1"));
        assert!(addon.contains("bpy.app.timers.register"));
        assert!(addon.contains("undo_push"));
        assert!(addon.contains("secrets.token_hex"));
        assert!(addon.contains("0o600"));
        assert!(addon.contains("BVHTree"));

        let cad_script = build_headless_script(&json!({"action": "cad_check"})).unwrap();
        assert!(cad_script.contains("BVHTree"));
        assert!(cad_script.contains(".overlap("));
        assert!(cad_script.contains("is_manifold"));
        assert!(cad_script.contains("is_boundary"));

        let render_script = build_headless_script(&json!({
            "action": "render_views",
            "output_path": "/tmp/out.png"
        }))
        .unwrap();
        assert!(render_script.contains("iso_45"));
        assert!(render_script.contains("_zerith_stitch_2x2"));
    }

    #[test]
    fn parse_zerith_result_ignores_blender_stdout_noise() {
        let noisy = "Blender 4.2.0\nRead blend: /tmp/a.blend\nFra:1 Mem:12M\nZERITH_RESULT:{\"ok\":true,\"result\":{\"passed\":true}}\nBlender quit\n";
        let val = parse_zerith_result(noisy).unwrap();
        assert_eq!(val["ok"], true);
        assert_eq!(val["result"]["passed"], true);
    }
}
