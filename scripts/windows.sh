#!/bin/sh
# luCode als Windows-App (x64) bauen – Querbau von macOS/Linux mit mingw-w64:
#   brew install mingw-w64   (bzw. apt install mingw-w64)
#   rustup target add x86_64-pc-windows-gnu
#
#   scripts/windows.sh            # Oberfläche + Programm
#   SKIP_UI=1 scripts/windows.sh  # Oberfläche nicht neu bauen
#
# Unter Windows selbst geht es auch (Git Bash): mingw-w64 (z. B. WinLibs, Variante msvcrt) und
# NSIS in den PATH legen, `rustup target add x86_64-pc-windows-gnu`, Dioxus-CLI `dx` 0.7.
#
# Ergebnis in dist-windows/:
#   luCode-<version>-Setup.exe  Installer (Startmenü, Deinstallieren; braucht makensis: brew install makensis)
#   luCode-<version>-x64.zip    ohne Installer (luCode.exe, entpacken und starten) – auch für die
#                               automatischen Updates der installierten App
#   SHA256SUMS
# Bewusst keine einzelne luCode-<version>-x64.exe mehr: Apps bis 1.1.1 nähmen sie fürs Update und
# setzten statt des Programms JSON ein (siehe macos/src/updater.rs). Ohne sie melden diese Apps nur,
# dass keine passende Datei da ist.
# Veröffentlichen: scripts/release.sh github|lugit|nrzgit|all (prüft vorher mit scripts/check-clean.sh)
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)
PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
export PATH

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app_version=$(sed -n 's/^version = "\(.*\)"/\1/p' macos/Cargo.toml | head -1)
if [ "$version" != "$app_version" ]; then
  echo "Version in Cargo.toml ($version) und macos/Cargo.toml ($app_version) ist verschieden" >&2
  exit 1
fi
command -v x86_64-w64-mingw32-gcc >/dev/null || { echo "mingw-w64 fehlt (brew install mingw-w64)" >&2; exit 1; }

if [ "${SKIP_UI:-}" != "1" ] || [ ! -f ui/dist/public/index.html ]; then
  scripts/ui.sh
fi

target=x86_64-pc-windows-gnu
# Keine Ordner dieser Maschine im Programm (siehe scripts/lib.sh)
. scripts/lib.sh
remap_paths "$root"
(cd macos && cargo build --release --locked --target $target)

out=dist-windows
rm -rf "$out"
mkdir -p "$out/luCode"
exe="${CARGO_TARGET_DIR:-macos/target}/$target/release/lucode-macos.exe"
# WebView2Loader.dll steckt im Programm (macos/src/webview2_loader.rs) und darf nicht in der
# Importtabelle stehen – sonst startet luCode.exe ohne die DLL daneben gar nicht erst.
objdump=$(command -v x86_64-w64-mingw32-objdump || command -v objdump) || { echo "objdump fehlt" >&2; exit 1; }
imports=$("$objdump" -p "$exe" | sed -n 's/^[[:space:]]*DLL Name: //p')
[ -n "$imports" ] || { echo "Importtabelle von $exe nicht lesbar" >&2; exit 1; }
if echo "$imports" | grep -qi webview2loader; then
  echo "WebView2Loader.dll steht in der Importtabelle von $exe – fehlt eine Funktion in webview2_loader.rs?" >&2
  exit 1
fi
cp "$exe" "$out/luCode/luCode.exe"
cat > "$out/luCode/LIESMICH.txt" <<TXT
luCode $version für Windows

luCode.exe starten (einzelne Datei, keine weiteren DLLs nötig).
Daten: %APPDATA%\\luCode   Protokoll: %LOCALAPPDATA%\\luCode\\Logs
Updates installiert die App selbst.

Beim ersten Start fragt Windows SmartScreen evtl. nach (die App ist nicht signiert):
"Weitere Informationen" → "Trotzdem ausführen".
TXT
if command -v zip >/dev/null; then
  (cd "$out" && zip -qr "luCode-$version-x64.zip" luCode)
else
  # Windows ohne zip: bsdtar des Systems schreibt ZIP mit /-Pfaden
  (cd "$out" && "${SYSTEMROOT:-C:/Windows}/System32/tar.exe" -a -c -f "luCode-$version-x64.zip" luCode)
fi
# Frische Instanz: Im Programm darf nichts von dieser Umgebung stehen (private Adressen, Tokens)
sh scripts/check-clean.sh "$out/luCode/luCode.exe"
# Installer (NSIS). Der Name enthält bewusst kein „x64“: Apps bis 1.0.28 nehmen fürs Update die
# erste EXE mit „x64“ im Namen – das darf nie der Installer sein.
if command -v makensis >/dev/null; then
  abs=$(pwd)/$out
  # Unter Windows erwartet makensis Windows-Pfade und „/D“ statt „-D“ stört Git Bash nicht
  if command -v cygpath >/dev/null; then abs=$(cygpath -w "$abs"); sep='\'; else sep=/; fi
  makensis -V2 "-DVERSION=$version" "-DEXE=$abs${sep}luCode${sep}luCode.exe" "-DOUT=$abs${sep}luCode-$version-Setup.exe" macos/resources/installer.nsi
else
  echo "makensis fehlt – kein Installer (brew install makensis)" >&2
fi
rm -rf "$out/luCode"
if command -v shasum >/dev/null; then sum="shasum -a 256"; else sum=sha256sum; fi
(cd "$out" && $sum luCode-*.zip $(ls luCode-*-Setup.exe 2>/dev/null) | sed 's/ \*/  /' > SHA256SUMS)
echo "Fertig: $out/luCode-$version-x64.zip ($(du -h "$out/luCode-$version-x64.zip" | cut -f1))"
[ -f "$out/luCode-$version-Setup.exe" ] && echo "        $out/luCode-$version-Setup.exe"
true
