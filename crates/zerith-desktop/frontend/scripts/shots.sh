#!/usr/bin/env bash
# Photographs every screen of the window over the fake core (src/preview).
# Needs `npm run dev` running on :5183. Usage:
#   scripts/shots.sh [outdir] [scene[:tab] ...]
# Defaults to every scene, dark and light.
set -u
here="$(cd "$(dirname "$0")/.." && pwd -W 2>/dev/null || pwd)"
out="${1:-$here/../../../target/shots/desktop}"
shift || true
mkdir -p "$out" "$here/../../../target/tmp/edge-${SHOTS_ID:-main}"
edge="/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
scenes=("$@")
if [ ${#scenes[@]} -eq 0 ]; then
  scenes=(empty chat approval question
    settings:Geral settings:Provedor settings:Contas "settings:Delegação" settings:Uso
    "settings:Memória" settings:Skills settings:Agentes "settings:Mídia" "settings:Histórico")
fi
for theme in ${THEMES:-dark light}; do
  for s in "${scenes[@]}"; do
    scene="${s%%:*}"
    tab=""
    [ "$scene" != "$s" ] && tab="${s#*:}"
    name="$theme-$scene${tab:+-$tab}"
    name="$(echo "$name" | iconv -f utf-8 -t ascii//TRANSLIT 2>/dev/null || echo "$name")"
    url="http://localhost:5183/preview.html?scene=$scene&theme=$theme"
    [ -n "$tab" ] && url="$url&tab=$(python -c "import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1]))" "$tab" 2>/dev/null || echo "$tab")"
    "$edge" --headless=new --disable-gpu --hide-scrollbars --no-first-run \
      --user-data-dir="$(cygpath -w "$here/../../../target/tmp/edge-${SHOTS_ID:-main}" 2>/dev/null || echo "$here/../../../target/tmp/edge-${SHOTS_ID:-main}")" \
      --window-size=${SHOT_W:-1280},${SHOT_H:-820} --virtual-time-budget=6000 \
      --screenshot="$(cygpath -w "$out/$name.png" 2>/dev/null || echo "$out/$name.png")" "$url" >/dev/null 2>&1
    echo "$out/$name.png"
  done
done
