"""Regenerates DEPENDENCY_LICENSES.md from `cargo metadata`.

Usage (from the zerith/ workspace): python scripts/dependency-licenses.py
Lists every package in the resolved graph for all targets, so platform-only
crates (Windows, macOS, Linux) appear too.
"""

import json
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
meta = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=root, check=True, capture_output=True, text=True,
    ).stdout
)
members = set(meta["workspace_members"])
rows = sorted(
    {
        (p["name"], p["version"], p.get("license") or p.get("license_file") or "DESCONHECIDA")
        for p in meta["packages"]
        if p["id"] not in members
    }
)
counts = {}
for _, _, lic in rows:
    counts[lic] = counts.get(lic, 0) + 1

out = [
    "# Licenças das dependências",
    "",
    "Gerado por `python scripts/dependency-licenses.py` a partir de",
    "`cargo metadata --format-version 1` (grafo resolvido, todos os alvos).",
    "Não edite à mão: rode o script de novo depois de mudar `Cargo.lock`.",
    "",
    f"Total: {len(rows)} pacotes de terceiros.",
    "",
    "## Resumo por licença",
    "",
    "| Licença | Pacotes |",
    "| --- | --- |",
]
out += [f"| `{lic}` | {n} |" for lic, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))]
out += ["", "## Pacotes", "", "| Nome | Versão | Licença |", "| --- | --- | --- |"]
out += [f"| {n} | {v} | `{lic}` |" for n, v, lic in rows]
(root / "DEPENDENCY_LICENSES.md").write_text("\n".join(out) + "\n", encoding="utf-8")
print(f"{len(rows)} packages", file=sys.stderr)
