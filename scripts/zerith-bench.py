#!/usr/bin/env python3
"""Official Zerith regression benchmark runner (`zerith-bench`).

Runs curated end-to-end tasks against a `zerith` binary across Networking/ISP
(Cisco ASR 1001-X, Datacom + Ubiquiti RF, Zabbix + Grafana NOC) and Programming
(Python Tkinter GUI + HTTP/HTML/JS NOC web panel), measuring elapsed time,
steps, skill hits, tool usage, token/cache stats, and automated `unittest` results.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from pathlib import Path
from typing import Any

DEFAULT_MODEL = "openrouter:deepseek/deepseek-v4.1-flash-1017"

CASES: list[dict[str, Any]] = [
    {
        "name": "cisco-asr-1001x",
        "category": "network",
        "expected_skills": ["cisco-iosxe-asr-master"],
        "expected_tools": ["net_audit"],
        "max_turns": 12,
        "run_unittest": False,
        "prompt": (
            "Estou analisando um Cisco ASR1001-X com lentidão para clientes PPPoE e instabilidade BGP. "
            "Use sua skill e auditoria Cisco para diagnosticar estes dados reais do roteador e me diga as "
            "causas-raiz e o plano seguro com revert timer:\n\n"
            "Throughput: 2500 Mbps (Current license: 2.5G)\n"
            "Five secs: 88% (QFP datapath utilization)\n"
            "Statistics drop:\n"
            "NatNoPort              45890\n"
            "TailDrop               120400\n"
            "show policy-map control-plane:\n"
            "  Class-map: class-bgp -> exceeded 950 pkts, violated 420 pkts\n"
            "show pppoe summary:\n"
            "  Te0/0/1.100  total 1250  PTA 810  TRANS 440\n"
        ),
    },
    {
        "name": "datacom-ubiquiti",
        "category": "network",
        "expected_skills": ["datacom-master", "ubiquiti-rf-ops"],
        "expected_tools": ["net_audit"],
        "max_turns": 12,
        "run_unittest": False,
        "prompt": (
            "Preciso auditar dois problemas em campo:\n"
            "1) Switch Datacom DM2104G2 com clientes da VLAN 775 sem navegação apesar de estarem untagged na porta 1/3:\n"
            "interface vlan 775\n set-member untagged ethernet 1/3\n!\ninterface ethernet 1/3\n switchport native vlan 1\n!\n"
            "2) Enlace Ubiquiti airMAX AC com sinal oscilando (12 km em 5800 MHz, 24 dBm tx, 27 dBi cada lado):\n"
            "mca-status: signal=-54 chain0=-54 chain1=-65 noise=-92 ccq=78 distance=12000\n"
            "Use as ferramentas e skills apropriadas para auditar ambos e apontar causa-raiz e correção."
        ),
    },
    {
        "name": "noc-zabbix-grafana",
        "category": "noc",
        "expected_skills": ["noc-zabbix-grafana-master"],
        "expected_tools": ["net_grafana", "net_zabbix"],
        "expected_files": ["dashboards/asr1001x.json", "zabbix/asr1001x.yaml"],
        "max_turns": 12,
        "run_unittest": False,
        "prompt": (
            "Sem acessar API externa (modo offline), gere no workspace:\n"
            "1) Um dashboard NOC completo em formato both (grafana_json + noc_portable_json) para o preset "
            "isp_backbone_cisco_asr salvo em dashboards/asr1001x.json;\n"
            "2) Um template Zabbix 7.0 LTS YAML para cisco_asr_1001x salvo em zabbix/asr1001x.yaml.\n"
            "Depois confira rapidamente os arquivos gerados."
        ),
    },
    {
        "name": "net-gui-monitor",
        "category": "code",
        "expected_skills": ["gui-engineering"],
        "expected_tools": ["write_file"],
        "max_turns": 25,
        "run_unittest": True,
        "prompt": (
            "Crie um app Desktop em Python com Tkinter (ping_monitor_gui.py) para monitorar hosts de rede com "
            "tabela de status, filtro por estado, botão Iniciar/Parar rodando em thread separada de forma segura "
            "(sem travar a UI nem dar erro de main thread is not in main loop) e exportação CSV. "
            "Inclua testes automatizados que validem a lógica e a inicialização da UI sem travar."
        ),
    },
    {
        "name": "net-noc-web",
        "category": "code",
        "expected_skills": [],
        "expected_tools": ["write_file", "browser"],
        "max_turns": 25,
        "run_unittest": True,
        "prompt": (
            "Crie um mini painel NOC local (servidor Python http.server + frontend HTML/CSS/JS moderno) que "
            "serve GET /api/status com enlaces/OLTs simulados, filtros por severidade, busca e auto-refresh, "
            "e inclua um teste automatizado que sobe o servidor em porta livre, valida /api/status e o HTML e encerra limpo."
        ),
    },
]


def default_zerith_bin() -> str:
    if os.name == "nt":
        candidates = [
            Path.home() / "bin" / "zerith.exe",
            Path(__file__).resolve().parent.parent / "target" / "release" / "zerith.exe",
            Path(__file__).resolve().parent.parent / "target" / "debug" / "zerith.exe",
        ]
    else:
        win_bin = Path("/mnt/c/Users/Joao/bin/zerith.exe")
        candidates = [
            win_bin,
            Path.home() / ".local" / "bin" / "zerith",
            Path(__file__).resolve().parent.parent / "target" / "release" / "zerith",
            Path(__file__).resolve().parent.parent / "target" / "debug" / "zerith",
        ]
    for cand in candidates:
        if cand.is_file():
            return str(cand)
    return shutil.which("zerith") or "zerith"


def default_python_bin() -> str:
    if os.name == "nt":
        return sys.executable
    win_py = Path("/mnt/c/Python314/python.exe")
    if win_py.is_file():
        return str(win_py)
    return sys.executable


def run_case(
    case: dict[str, Any],
    zerith_bin: str,
    model: str,
    max_turns_override: int | None,
    work_root: Path,
    timeout_secs: int,
) -> dict[str, Any]:
    name = case["name"]
    workdir = work_root / name
    if workdir.exists():
        shutil.rmtree(workdir)
    workdir.mkdir(parents=True, exist_ok=True)

    max_turns = max_turns_override or case["max_turns"]
    cmd = [
        zerith_bin,
        "-p",
        case["prompt"],
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--permission-mode",
        "bypass",
        "--max-turns",
        str(max_turns),
    ]

    print(f"[zerith-bench] running {name} (model={model}, max_turns={max_turns})...", flush=True)
    t0 = time.monotonic()
    events: list[dict[str, Any]] = []
    timed_out = False

    proc = subprocess.Popen(
        cmd,
        cwd=str(workdir),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    q: queue.Queue[bytes | None] = queue.Queue()

    def reader() -> None:
        assert proc.stdout is not None
        try:
            for line in iter(proc.stdout.readline, b""):
                q.put(line)
        finally:
            q.put(None)

    threading.Thread(target=reader, daemon=True).start()
    last_activity = t0
    while True:
        try:
            raw = q.get(timeout=5.0)
        except queue.Empty:
            if time.monotonic() - last_activity > timeout_secs:
                timed_out = True
                proc.kill()
                break
            continue
        if raw is None:
            break
        last_activity = time.monotonic()
        line = raw.decode("utf-8", errors="replace").strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except Exception:
            pass

    rc = proc.wait()
    stderr_text = proc.stderr.read().decode("utf-8", errors="replace")[-1200:] if proc.stderr else ""
    elapsed = round(time.monotonic() - t0, 3)

    calls: Counter[str] = Counter()
    fails: Counter[str] = Counter()
    skills_called: list[str] = []
    steps = 0
    stop_reason = None
    prompt_tokens = 0
    out_tokens = 0
    cached_tokens = 0
    reason_chars = 0

    for ev in events:
        t = ev.get("type")
        if t == "route":
            steps += 1
        elif t == "reasoning":
            reason_chars += len(ev.get("text") or "")
        elif t == "tool_call":
            tn = ev.get("tool") or ev.get("name") or "?"
            calls[tn] += 1
            args = ev.get("args") or ev.get("arguments") or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except Exception:
                    args = {}
            if tn == "skill" and isinstance(args, dict):
                sn = args.get("name") or args.get("skill")
                if sn:
                    skills_called.append(str(sn))
        elif t == "tool_result":
            tn = ev.get("tool") or ev.get("name") or "?"
            if ev.get("ok") is False or ev.get("is_error") is True:
                fails[tn] += 1
        elif t == "usage":
            prompt_tokens += int(ev.get("input") or ev.get("prompt_tokens") or 0)
            out_tokens += int(ev.get("output") or ev.get("completion_tokens") or 0)
            cached_tokens += int(ev.get("cache_read") or ev.get("cached_tokens") or 0)
        elif t == "result":
            stop_reason = ev.get("stop_reason") or ev.get("reason")
            u = ev.get("usage") or {}
            if not prompt_tokens and u:
                prompt_tokens = int(u.get("prompt_tokens") or 0)
                out_tokens = int(u.get("completion_tokens") or 0)

    unittest_res = None
    if case.get("run_unittest"):
        py = default_python_bin()
        py_files = sorted(str(p.relative_to(workdir)) for p in workdir.rglob("*.py") if "__pycache__" not in p.parts)
        test_files = [f for f in py_files if "test" in Path(f).name.lower()]
        ucmd = [py, "-m", "unittest", "-v"] + (test_files if test_files else ["discover", "-v"])
        try:
            tr = subprocess.run(ucmd, cwd=str(workdir), stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
            unittest_res = {"returncode": tr.returncode, "stderr_tail": tr.stderr[-800:]}
        except subprocess.TimeoutExpired:
            unittest_res = {"returncode": -1, "stderr_tail": "timeout (30s)"}

    created_files = sorted(str(p.relative_to(workdir)).replace("\\", "/") for p in workdir.rglob("*") if p.is_file() and "__pycache__" not in p.parts)
    missing_skills = [s for s in case.get("expected_skills", []) if s not in skills_called]
    missing_tools = [t for t in case.get("expected_tools", []) if calls.get(t, 0) == 0]
    missing_files = [f for f in case.get("expected_files", []) if f not in created_files]
    unittest_ok = True if unittest_res is None else unittest_res["returncode"] == 0
    passed = (rc == 0 or stop_reason != "max_turns") and not timed_out and not missing_skills and not missing_tools and not missing_files and unittest_ok

    return {
        "name": name,
        "category": case["category"],
        "passed": passed,
        "elapsed_s": elapsed,
        "rc": rc,
        "timed_out": timed_out,
        "steps": steps,
        "stop_reason": stop_reason,
        "skills_called": skills_called,
        "missing_skills": missing_skills,
        "calls": dict(calls),
        "missing_tools": missing_tools,
        "fails": dict(fails),
        "prompt_tokens": prompt_tokens,
        "out_tokens": out_tokens,
        "cache_pct": round(cached_tokens / prompt_tokens * 100.0, 1) if prompt_tokens else 0.0,
        "reason_chars": reason_chars,
        "files": created_files,
        "missing_files": missing_files,
        "unittest": unittest_res,
        "stderr": stderr_text.strip(),
    }


def compare_reports(current: list[dict[str, Any]], baseline_path: Path) -> None:
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    by_name = {r["name"]: r for r in baseline if isinstance(r, dict) and "name" in r}
    print("\n=== COMPARISON VS BASELINE ===")
    print(f"{'Case':<22} {'Pass':<7} {'Elapsed(s)':<18} {'Steps':<12} {'Tokens(Prompt)':<18}")
    for r in current:
        b = by_name.get(r["name"])
        if b:
            dt = r["elapsed_s"] - b.get("elapsed_s", r["elapsed_s"])
            ds = r["steps"] - b.get("steps", r["steps"])
            print(
                f"{r['name']:<22} {str(r['passed']):<7} "
                f"{r['elapsed_s']:>6.1f} ({dt:+6.1f})   "
                f"{r['steps']:>3} ({ds:+3})    "
                f"{r['prompt_tokens']:>9} vs {b.get('prompt_tokens', 0)}"
            )
        else:
            print(f"{r['name']:<22} {str(r['passed']):<7} {r['elapsed_s']:>6.1f}            {r['steps']:>3}          {r['prompt_tokens']:>9}")


def main() -> int:
    ap = argparse.ArgumentParser(description="Zerith official live regression benchmark suite")
    ap.add_argument("--list", action="store_true", help="List available benchmark cases and exit")
    ap.add_argument("--cases", help="Comma-separated subset of case names (default: all)")
    ap.add_argument("--bin", dest="zerith_bin", default=default_zerith_bin(), help="Path to zerith binary")
    ap.add_argument("--model", default=DEFAULT_MODEL, help="Model route for all cases")
    ap.add_argument("--max-turns", type=int, default=None, help="Override max turns for every case")
    ap.add_argument("--timeout", type=int, default=240, help="Idle timeout per case in seconds")
    ap.add_argument("--out", help="Optional JSON output report path")
    ap.add_argument("--compare", help="Optional baseline JSON report path to compare deltas")
    ap.add_argument("--keep-workdir", action="store_true", help="Keep temporary generated workspaces")
    args = ap.parse_args()

    if args.list:
        for c in CASES:
            print(f"{c['name']:<22} [{c['category']}] skills={c['expected_skills']} tools={c['expected_tools']} max_turns={c['max_turns']}")
        return 0

    selected = CASES
    if args.cases:
        wanted = {x.strip() for x in args.cases.split(",") if x.strip()}
        selected = [c for c in CASES if c["name"] in wanted]
        if not selected:
            print(f"No matching cases for --cases={args.cases}", file=sys.stderr)
            return 2

    work_root = Path(tempfile.mkdtemp(prefix="zerith-bench-"))
    results: list[dict[str, Any]] = []
    try:
        for c in selected:
            res = run_case(c, args.zerith_bin, args.model, args.max_turns, work_root, args.timeout)
            results.append(res)
            print(json.dumps(res, ensure_ascii=False, indent=2), flush=True)
    finally:
        if not args.keep_workdir and work_root.exists():
            shutil.rmtree(work_root, ignore_errors=True)

    if args.out:
        out_p = Path(args.out)
        out_p.parent.mkdir(parents=True, exist_ok=True)
        out_p.write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")

    if args.compare:
        compare_reports(results, Path(args.compare))

    all_ok = all(r["passed"] for r in results)
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
