#!/bin/sh
# Release veröffentlichen: die Dateien aus dist-windows/ und dist-macos/ als Release `v<version>` –
# auf GitHub, luGit und/oder nrzGit. Die installierten Apps finden es von selbst (Update-Hinweis).
#
#   scripts/release.sh github              # nach GitHub
#   scripts/release.sh lugit nrzgit        # in die privaten Git-Plattformen
#   scripts/release.sh all                 # überall, wofür in der Umgebung ein Token steht
#   DRY_RUN=1 scripts/release.sh all       # nur prüfen und zeigen, was geschähe
#
# Im Skript steht keine Adresse eines Servers: Adressen und Tokens kommen aus der Umgebung
# (luCode gibt sie seinen Agenten für die verbundenen Plattformen mit).
#
#   GitHub   GH_TOKEN oder GITHUB_TOKEN; GITHUB_REPO=besitzer/name (Vorgabe: aus `git remote`)
#   luGit    LUGIT_URL, LUGIT_TOKEN;     LUGIT_REPO=besitzer/name  (Vorgabe: <LUGIT_USERNAME>/<name von origin>)
#   nrzGit   NRZGIT_URL, NRZGIT_TOKEN;   NRZGIT_REPO=besitzer/name (Vorgabe: <NRZGIT_USERNAME>/<name von origin>)
#
# Weitere: NOTES=datei.md (Text des Releases), RELEASE_TARGET (Branch für den Tag, Vorgabe: der
# aktuelle – er muss dort schon liegen). Vorher scripts/windows.sh bzw. scripts/macos.sh ausführen.
# Braucht curl und jq (für GitHub genügt auch das CLI `gh`).
#
# Vor dem Hochladen prüft scripts/check-clean.sh Quelltext und Dateien: Ein Release ist eine
# frische Instanz – ohne Adressen privater Server und ohne Tokens.
#
# Apps, die ihre Updates nicht von GitHub holen sollen, bekommen die Adresse über
# LUCODE_UPDATE_REPO (z. B. https://<lugit>/api/v1/repos/<besitzer>/<name>).
set -eu
cd "$(dirname "$0")/.."

[ $# -gt 0 ] || { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 1; }
dry=${DRY_RUN:-}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tag="v$version"
target=${RELEASE_TARGET:-$(git rev-parse --abbrev-ref HEAD)}
origin=$(git remote get-url origin 2>/dev/null || true)
origin_name=$(printf '%s' "$origin" | sed -e 's|\.git$||' -e 's|.*/||')
[ -n "$origin_name" ] || origin_name=Lu-Code

# ------------------------------------------------------------------ Dateien
files=""
for d in dist-windows dist-macos; do
  for f in "$d"/luCode-"$version"-*; do
    [ -f "$f" ] && files="$files $f"
  done
done
[ -n "$files" ] || { echo "Keine Dateien für $version in dist-windows/ oder dist-macos/ – zuerst scripts/windows.sh bzw. scripts/macos.sh" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
: > "$work/SHA256SUMS"
for d in dist-windows dist-macos; do
  [ -f "$d/SHA256SUMS" ] && cat "$d/SHA256SUMS" >> "$work/SHA256SUMS"
done

# shellcheck disable=SC2086
sh scripts/check-clean.sh $files

if [ -n "${NOTES:-}" ]; then
  cp "$NOTES" "$work/notes.md"
else
  cat > "$work/notes.md" <<MD
# luCode $version

**Windows:** **luCode-$version-Setup.exe** ausführen (Installer) – oder die ZIP entpacken und **luCode.exe** starten.

**macOS:** DMG öffnen und **luCode** in **Programme** ziehen. Beim ersten Start: Rechtsklick auf die App → **Öffnen** (nicht von Apple notarisiert).

Jede Installation beginnt als frische Instanz. Wer von nrzCode kommt: *Einstellungen → Umzug von nrzCode*.
Installierte Apps finden neue Versionen selbst und zeigen einen Hinweis mit Knopf **Aktualisieren**.
MD
fi

# ------------------------------------------------------------------ Ziele
targets=""
for t in "$@"; do
  case "$t" in
    all)
      [ -n "${GH_TOKEN:-${GITHUB_TOKEN:-}}" ] && targets="$targets github"
      [ -n "${LUGIT_TOKEN:-}" ] && [ -n "${LUGIT_URL:-}" ] && targets="$targets lugit"
      [ -n "${NRZGIT_TOKEN:-}" ] && [ -n "${NRZGIT_URL:-}" ] && targets="$targets nrzgit"
      ;;
    github|lugit|nrzgit) targets="$targets $t" ;;
    *) echo "Unbekanntes Ziel: $t (github, lugit, nrzgit oder all)" >&2; exit 1 ;;
  esac
done
[ -n "$targets" ] || { echo "Kein Ziel: Für keine Plattform steht ein Token in der Umgebung." >&2; exit 1; }

echo "luCode $version (Tag $tag auf $target):"
for f in $files; do echo "  $(basename "$f")  $(du -h "$f" | cut -f1)"; done

# ------------------------------------------------------------------ GitHub
publish_github() {
  repo=${GITHUB_REPO:-}
  if [ -z "$repo" ]; then
    case "$origin" in
      *github.com[:/]*) repo=$(printf '%s' "$origin" | sed -e 's|\.git$||' -e 's|.*github\.com[:/]||') ;;
    esac
  fi
  [ -n "$repo" ] || { echo "GitHub: GITHUB_REPO=besitzer/name fehlt (origin zeigt nicht auf GitHub)" >&2; return 1; }
  token=${GH_TOKEN:-${GITHUB_TOKEN:-}}
  echo "GitHub: $repo"
  [ -z "$dry" ] || return 0
  if command -v gh >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    GH_TOKEN=$token gh release create "$tag" $files "$work/SHA256SUMS" --repo "$repo" --target "$target" --title "luCode $version" --notes-file "$work/notes.md"
    return
  fi
  [ -n "$token" ] || { echo "GitHub: GH_TOKEN fehlt" >&2; return 1; }
  api=https://api.github.com/repos/$repo
  created=$(jq -n --arg t "$tag" --arg g "$target" --arg n "luCode $version" --rawfile b "$work/notes.md" '{tag_name:$t,target_commitish:$g,name:$n,body:$b,draft:true}' |
    curl -sS --fail -X POST "$api/releases" -H "Authorization: Bearer $token" -H 'Accept: application/vnd.github+json' --data-binary @-)
  id=$(printf '%s' "$created" | jq -r .id)
  for f in $files "$work/SHA256SUMS"; do
    echo "  Hochladen: $(basename "$f") …"
    curl -sS --fail -X POST "https://uploads.github.com/repos/$repo/releases/$id/assets?name=$(basename "$f")" \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/octet-stream' --data-binary @"$f" >/dev/null
  done
  curl -sS --fail -X PATCH "$api/releases/$id" -H "Authorization: Bearer $token" -H 'Accept: application/vnd.github+json' -d '{"draft":false}' >/dev/null
  echo "  Fertig: https://github.com/$repo/releases/tag/$tag"
}

# ------------------------------------------------------------------ luGit und nrzGit (MCP)
# Aufruf eines Werkzeugs der Plattform (JSON-RPC über HTTP); gibt den Text des Ergebnisses aus
mcp() { # Adresse, Token, Werkzeug, Argumente (JSON)
  jq -n --arg tool "$3" --argjson args "$4" '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$tool,arguments:$args}}' |
    curl -sS --fail -X POST "$1/mcp" -H "Authorization: Bearer $2" \
      -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data-binary @- |
    sed -n 's/^data: //p; /^{/p' | head -1 |
    jq -er 'if .error then error(.error.message) elif .result.isError then error(.result.content[0].text) else .result.content[0].text end'
}

publish_forge() { # Name, Adresse, Token, Repository
  label=$1
  url=${2%/}
  token=$3
  repo=$4
  [ -n "$url" ] && [ -n "$token" ] || { echo "$label: Adresse oder Token fehlt in der Umgebung" >&2; return 1; }
  echo "$label: $repo"
  [ -z "$dry" ] || return 0
  # Kennt die Plattform die Werkzeuge für Releases? (luGit hat sie ab 1.1.0)
  tools=$(printf '%s' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' |
    curl -sS --fail -X POST "$url/mcp" -H "Authorization: Bearer $token" \
      -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data-binary @- |
    sed -n 's/^data: //p; /^{/p' | head -1 | jq -r '.result.tools[].name')
  for need in create_release upload_release_asset publish_release; do
    printf '%s\n' "$tools" | grep -qx "$need" || {
      echo "$label: Das Werkzeug $need fehlt. Vorhanden für Releases: $(printf '%s\n' "$tools" | grep -i release | tr '\n' ' ')" >&2
      return 1
    }
  done
  base=$(jq -n --arg o "${repo%%/*}" --arg r "${repo#*/}" '{owner:$o,repo:$r}')
  existing=$(mcp "$url" "$token" get_release_by_tag "$(printf '%s' "$base" | jq --arg t "$tag" '. + {tag:$t}')" 2>/dev/null || true)
  [ -z "$existing" ] || { echo "$label: Release $tag gibt es schon – zuerst löschen oder die Version erhöhen." >&2; return 1; }
  created=$(mcp "$url" "$token" create_release "$(printf '%s' "$base" | jq --arg t "$tag" --rawfile n "$work/notes.md" --arg v "luCode $version" --arg g "$target" \
    '. + {tag_name:$t, target:$g, title:$v, body:$n, is_draft:true}')")
  id=$(printf '%s' "$created" | jq -r .id)
  for f in $files "$work/SHA256SUMS"; do
    echo "  Hochladen: $(basename "$f") …"
    base64 < "$f" | tr -d '\n' > "$work/b64"
    printf '%s' "$base" | jq --argjson id "$id" --arg n "$(basename "$f")" --rawfile c "$work/b64" \
      '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:"upload_release_asset",arguments:(. + {id:$id,name:$n,content_base64:$c})}}' > "$work/req.json"
    curl -sS --fail -X POST "$url/mcp" -H "Authorization: Bearer $token" \
      -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data-binary @"$work/req.json" |
      sed -n 's/^data: //p; /^{/p' | head -1 |
      jq -er 'if .error then error(.error.message) elif .result.isError then error(.result.content[0].text) else "ok" end' >/dev/null
  done
  mcp "$url" "$token" publish_release "$(printf '%s' "$base" | jq --argjson id "$id" '. + {id:$id}')" >/dev/null
  echo "  Fertig: $url/$repo/releases/tag/$tag"
}

failed=0
for t in $targets; do
  case "$t" in
    github) publish_github || failed=1 ;;
    lugit) publish_forge luGit "${LUGIT_URL:-}" "${LUGIT_TOKEN:-}" "${LUGIT_REPO:-${LUGIT_USERNAME:-}/$origin_name}" || failed=1 ;;
    nrzgit) publish_forge nrzGit "${NRZGIT_URL:-}" "${NRZGIT_TOKEN:-}" "${NRZGIT_REPO:-${NRZGIT_USERNAME:-}/$origin_name}" || failed=1 ;;
  esac
done
[ -z "$dry" ] || echo "(Probelauf – nichts veröffentlicht)"
exit $failed
