#!/bin/sh
# Prüft, dass eine Veröffentlichung „frisch“ ist: Im Quelltext und in den gebauten Dateien steht
# nichts von einer bestimmten Instanz – keine Adressen privater Server (Tailscale-Netz, die
# verbundenen Git-Plattformen dieser Umgebung), keine Zugriffstokens und keine Ordner der
# Maschine, auf der gebaut wurde.
#
#   scripts/check-clean.sh                      # Quelltext (alles, was git kennt bzw. nicht ignoriert)
#   scripts/check-clean.sh dist-windows/*.exe   # zusätzlich diese Dateien (auch Programme)
#
# Geprüft wird auf:
#   * Adressen aus dem Tailscale-/CGNAT-Bereich (100.64.x.x bis 100.127.x.x)
#   * die Hosts aus NRZGIT_URL und LUGIT_URL dieser Umgebung
#   * die Werte von NRZGIT_TOKEN, LUGIT_TOKEN, GITHUB_TOKEN und GH_TOKEN dieser Umgebung
#   * in gebauten Dateien: Benutzerordner der Build-Maschine (unter Windows der Ordner unter
#     Users, sonst /Users/<name>/ bzw. /home/<name>/) – die Build-Skripte blenden sie aus
#     (scripts/lib.sh)
# Endet mit 1, sobald etwas gefunden wird (mit Datei und Fundstelle; Tokens werden nie ausgegeben).
set -eu
cd "$(dirname "$0")/.."

tailnet='(^|[^0-9.])100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}([^0-9]|$)'
homes='([A-Za-z]:[\\/]+Users[\\/]+[A-Za-z0-9_.-]+[\\/])|(/Users/[A-Za-z0-9_.-]+/)|(/home/[A-Za-z0-9_.-]+/)'
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
: > "$work/found"

# Host (ohne Schema und Port) einer Adresse; öffentliche Dienste zählen nicht
host_of() {
  h=$(printf '%s' "$1" | sed -e 's|^[a-zA-Z]*://||' -e 's|[:/].*$||')
  case "$h" in ''|localhost|127.0.0.1|github.com|*.github.com) ;; *) printf '%s' "$h" ;; esac
}
hosts=""
for u in "${NRZGIT_URL:-}" "${LUGIT_URL:-}"; do
  h=$(host_of "$u")
  [ -n "$h" ] && hosts="$hosts $h"
done

report() { # Art, Fundstellen (Zeilen)
  [ -n "$2" ] || return 0
  echo x >> "$work/found"
  echo "NICHT SAUBER – $1:" >&2
  printf '%s\n' "$2" | head -8 | cut -c1-170 | sed 's/^/    /' >&2
}

# ------------------------------------------------------------------ Quelltext
# Alles, was git kennt oder nicht ignoriert (keine Build-Ordner) – in einem Durchlauf je Muster.
# Dieses Skript nennt die Muster selbst und zählt nicht.
git ls-files -co --exclude-standard -z | tr '\0' '\n' | grep -v -x 'scripts/check-clean.sh' > "$work/files"
count=$(wc -l < "$work/files" | tr -d ' ')
search() { # weitere Schalter für grep …
  tr '\n' '\0' < "$work/files" | xargs -0 grep -a -I -H "$@" 2>/dev/null || true
}
report "Adresse aus einem privaten Netz" "$(search -n -E "$tailnet")"
for h in $hosts; do
  report "Adresse einer verbundenen Git-Plattform ($h)" "$(search -n -F "$h")"
done
for name in NRZGIT_TOKEN LUGIT_TOKEN GITHUB_TOKEN GH_TOKEN; do
  eval "v=\${$name:-}"
  # Kurze Werte wären keine Tokens – und träfen alles Mögliche
  [ "${#v}" -ge 16 ] || continue
  report "Zugriffstoken ($name) in" "$(tr '\n' '\0' < "$work/files" | xargs -0 grep -a -l -F -- "$v" 2>/dev/null || true)"
done

# ------------------------------------------------------------------ Gebaute Dateien
# Als Argumente; hier zählt nur die Fundstelle selbst (keine Binärzeilen).
built=0
for f in "$@"; do
  [ -f "$f" ] || continue
  built=$((built + 1))
  report "Adresse aus einem privaten Netz in $f" "$(grep -a -o -E "$tailnet" "$f" 2>/dev/null | sort -u || true)"
  for h in $hosts; do
    report "Adresse einer verbundenen Git-Plattform in $f" "$(grep -a -o -F "$h" "$f" 2>/dev/null | sort -u || true)"
  done
  for name in NRZGIT_TOKEN LUGIT_TOKEN GITHUB_TOKEN GH_TOKEN; do
    eval "v=\${$name:-}"
    [ "${#v}" -ge 16 ] || continue
    if grep -a -q -F -- "$v" "$f" 2>/dev/null; then
      report "Zugriffstoken ($name)" "$f"
    fi
  done
  report "Ordner der Build-Maschine in $f (ohne scripts/lib.sh gebaut?)" "$(grep -a -o -E "$homes" "$f" 2>/dev/null | sort | uniq -c | sort -rn | head -5 || true)"
done

if [ -s "$work/found" ]; then
  echo "Bitte entfernen – eine Veröffentlichung darf nichts von einer bestimmten Instanz enthalten." >&2
  exit 1
fi
if [ "$built" -gt 0 ]; then
  echo "Sauber: keine privaten Adressen, Tokens oder Ordner dieser Maschine ($count Dateien im Quelltext, $built gebaute)."
else
  echo "Sauber: keine privaten Adressen und keine Tokens ($count Dateien im Quelltext)."
fi
