#!/bin/sh
# Gemeinsames für die Build-Skripte (mit `. scripts/lib.sh` einbinden).

# Ordner dieser Maschine nicht in die Programme schreiben. Rust nennt in Fehlermeldungen (Panik)
# die Pfade der Quelltexte – ohne das stünden in jedem gebauten Programm der Benutzerordner
# dessen, der gebaut hat, und der Ort des Repositorys. Die Pfade werden auf neutrale Namen
# abgebildet: das Repository auf `luCode`, der Ordner von Cargo auf `cargo`.
#
#   remap_paths <wurzel des repositorys> [plain]
#
# Setzt CARGO_ENCODED_RUSTFLAGS (Schalter durch das Zeichen 0x1f getrennt – Pfade dürfen
# Leerzeichen enthalten). Schon gesetzte Schalter bleiben erhalten.
#
# Mit `plain` stattdessen RUSTFLAGS (durch Leerzeichen getrennt): Die Dioxus-CLI `dx` liest nur
# diese Variable und hängt ihre eigenen Schalter an. Pfade mit Leerzeichen lassen sich so nicht
# angeben – dann bleibt es bei einer Warnung.
remap_paths() {
  _root=$1
  _cargo=${CARGO_HOME:-$HOME/.cargo}
  _sep=$(printf '\037')
  _flags=""
  _add() { _flags="${_flags}${_flags:+$_sep}--remap-path-prefix=$1=$2"; }
  if command -v cygpath >/dev/null 2>&1; then
    # Unter Windows sieht der Compiler die Pfade mit \ – je nach Aufrufer auch mit /
    _add "$(cygpath -w "$_root")" luCode
    _add "$(cygpath -m "$_root")" luCode
    _add "$(cygpath -w "$_cargo")" cargo
    _add "$(cygpath -m "$_cargo")" cargo
  else
    _add "$_root" luCode
    _add "$_cargo" cargo
  fi
  if [ "${2:-}" = plain ]; then
    case "$_flags" in
      *" "*) echo "Hinweis: Pfade mit Leerzeichen – die Ordner dieser Maschine bleiben im WebAssembly sichtbar" >&2; return 0 ;;
    esac
    RUSTFLAGS="${RUSTFLAGS:-}${RUSTFLAGS:+ }$(printf '%s' "$_flags" | tr '\037' ' ')"
    export RUSTFLAGS
    unset CARGO_ENCODED_RUSTFLAGS
    return 0
  fi
  if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
    _flags="$CARGO_ENCODED_RUSTFLAGS$_sep$_flags"
  elif [ -n "${RUSTFLAGS:-}" ]; then
    for _f in $RUSTFLAGS; do _flags="$_f$_sep$_flags"; done
    unset RUSTFLAGS
  fi
  CARGO_ENCODED_RUSTFLAGS=$_flags
  export CARGO_ENCODED_RUSTFLAGS
}

# Vollen Pfad des Repositorys in gebündelten WebAssembly-Dateien unkenntlich machen. Die
# Asset-Einträge von Dioxus (`asset!`) nennen zu jeder Datei ihren vollen Pfad auf der Maschine,
# auf der gebaut wurde – gebraucht wird er nur beim Bündeln, zur Laufzeit zählt der gebündelte
# Name. Er wird durch gleich lange, neutrale Zeichen ersetzt (die Datei behält ihre Grösse).
#
#   scrub_paths <ordner mit dem bündel> <wurzel des repositorys>
scrub_paths() {
  command -v perl >/dev/null 2>&1 || { echo "Hinweis: perl fehlt – die Asset-Pfade bleiben im WebAssembly stehen" >&2; return 0; }
  _dir=$1
  _root=$2
  if command -v cygpath >/dev/null 2>&1; then
    _a=$(cygpath -w "$_root")
    _b=$(cygpath -m "$_root")
  else
    _a=$_root
    _b=""
  fi
  find "$_dir" -type f -name '*.wasm' | while IFS= read -r _f; do
    perl -e '
      my ($f, @forms) = @ARGV;
      open(my $in, "<:raw", $f) or die "$f: $!";
      local $/;
      my $d = <$in>;
      close $in;
      my $n = 0;
      for my $p (@forms) {
        next unless length $p;
        my $r = substr("luCode" . ("_" x length $p), 0, length $p);
        $n += ($d =~ s/\Q$p\E/$r/g);
      }
      if ($n) {
        open(my $out, ">:raw", $f) or die "$f: $!";
        print $out $d;
        close $out;
      }
    ' "$_f" "$_a" "$_b"
  done
}
