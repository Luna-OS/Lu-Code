# luCode: Umzug von nrzCode und Extras

Was luCode über nrzCode hinaus anbietet. Alle Pfade unter `/api/v1`, JSON in camelCase,
Zeitangaben in Unix-Sekunden. Formen: `shared/src/import.rs`, `shared/src/extras.rs`.

## Umzug von nrzCode (`src/import/`)

Nur für Administratoren.

| Methode | Pfad | |
|---|---|---|
| GET | `/import/nrzcode?path=` | → `ImportSource` – Quelle finden (ohne `path`: `LUCODE_IMPORT_DIR`, `%APPDATA%\nrzCode`, `~/Library/Application Support/nrzCode`, `~/.local/share/nrzCode`, Ordner neben dem eigenen Datenordner), zählen (`counts`) und sagen, was davon noch fehlt (`pending`) |
| POST | `/import/nrzcode` | `ImportOptions` → `ImportStatus` – startet den Import im Hintergrund; `409`, wenn schon einer läuft |
| GET | `/import/nrzcode/status` | → `ImportStatus` – `state` (`idle`, `running`, `done`, `error`), Schritt, kopierte Dateien und Bytes, Hinweise, was neu dazukam |

`ImportOptions`: `path`, `accounts`, `connections`, `workspaces`, `extras`, `tools`,
`skipBuildDirs` – ohne Angaben alles (`{}` genügt).

### Ablauf

1. **Datenbank, eine Transaktion.** Die Quelle (`<ordner>/nrzcode.db`) wird nur gelesen – in einem
   Lesevorgang, damit alle Tabellen denselben Stand haben, auch wenn nrzCode läuft. Aus jeder
   Tabelle kommen die Spalten, die es hier auch gibt (`rows.rs`); neue Spalten einer neueren
   nrzCode-Version stören nicht. Jede Zeile bekommt hier eine Nummer – die alte, wenn sie frei ist –
   und alle Verweise folgen ihr:

   | Was | Besonderheit |
   |---|---|
   | Benutzer | gleicher Name → derselbe Benutzer (Sprache, Name, Profilbild werden ergänzt); hat die Quelle nur einen, ist es der Importierende; sonst wird das Konto mit dem Passwort aus nrzCode angelegt |
   | Git-Verbindungen | je Benutzer und Plattform eine; eine hier eingerichtete bleibt |
   | Agent-Konten | Vorgabe-Konto nur, wenn es hier für die Art noch keines gibt |
   | Projekte | bis ihre Dateien kopiert sind `cloning` („Wird aus nrzCode übernommen …“) |
   | Threads | Eltern-Thread, Projekt, Bot und Konto mit neuen Nummern; `working`/`waiting` → `idle`, keine Wiederaufnahme, leere Warteschlange |
   | Läufe, Nutzung | laufende gelten als abgebrochen; je Lauf genau eine Zeile in `usage_log` (die Zeilen des Triggers an `runs` werden durch die der Quelle ersetzt), `thread_key` bekommt die Kennung der Quelle davor |
   | Nachrichten | `data.children` der Subagenten-Nachrichten |
   | Canvas | `threadId` und `id` (`t<thread>`) der Fenster, `closed`; Terminals bleiben weg |
   | Einstellungen | `chains:<benutzer>` und `voice:<benutzer>` mit den neuen Nummern der Konten; die Kopplung mit dem Handy (`mobile:…`) zieht nicht um |
   | Team | eingeschaltet/Antworten; den Stand holt der nächste Abgleich aus dem Repository |

   `import_map (source, tbl, old_id, new_id, files_done)` merkt sich die Zuordnung. `source` ist
   eine Kennung der nrzCode-Instanz (aus ihrem Cookie-Schlüssel). Beim nächsten Import werden
   zugeordnete Zeilen übersprungen – nur Neues kommt dazu. Was in luCode gelöscht wurde, kommt
   nicht zurück.

2. **Dateien.** Für alles mit `files_done = 0` (auch aus einem abgebrochenen Import):
   `accounts/<konto>` (Homes der CLIs mit den Anmeldungen), `workspaces/<projekt>` (Checkout,
   Worktrees – Build-Ordner auf Wunsch ausgelassen: `node_modules`, `.venv`, `.gradle`, `.next` …
   und jeder Ordner mit `CACHEDIR.TAG`, nie innerhalb von `.git`), `scratch/<thread>`,
   `bots/<bot>`, `avatars/<benutzer>`, `docker/<benutzer>`, `attachments/<uuid>`, `tools/` (nur
   wenn hier noch keine installiert sind), `models/`. Worktrees tragen danach die neue Nummer
   ihres Threads, und ihre Verweise (`<worktree>/.git` ↔ `<repo>/.git/worktrees/<name>/gitdir`)
   zeigen auf das Repository hier.

3. **Sitzungen.** Claude Code legt Sitzungen je Arbeitsordner ab
   (`<home>/.claude/projects/<pfad mit - statt Sonderzeichen>`); diese Ordner bekommen den Namen
   des neuen Arbeitsordners. Ein Thread behält seine Sitzung nur, wenn das CLI sie hier findet
   (Claude Code: die Datei liegt da; Codex: sein Home wurde kopiert) – sonst beginnt der nächste
   Lauf eine neue und bekommt den bisherigen Verlauf mit.

`import_runs` protokolliert jeden Lauf (Quelle, Pfad, Ergebnis).

## Extras (`src/extras/`)

| Methode | Pfad | |
|---|---|---|
| GET | `/search?q=&limit=` | → `SearchResults` – eigene Chats, deren Titel passt (`threads`), und Nachrichten von dir und den Agenten mit dem Begriff (`messages`, mit Ausschnitt); ab zwei Zeichen |
| GET, PUT | `/instructions` | `Instructions { text }` – eigene Anweisungen des Benutzers (höchstens 6000 Zeichen) |
| GET, PUT | `/projects/<id>/instructions` | dasselbe je Projekt (nur der Besitzer, sonst `404`) |
| GET | `/threads/<id>/export` | der Chat als Markdown-Datei (`Content-Disposition: attachment`) |
| GET | `/backups` | → `BackupList` (Administratoren) |
| POST | `/backups` | jetzt sichern → `BackupList` |
| PUT | `/backups/settings` | `BackupSettings { auto }` – tägliche Sicherung ein/aus |
| DELETE | `/backups/<name>` | Sicherung löschen |
| GET | `/backups/<name>/download` | Sicherung herunterladen |

**Anweisungen** stehen bei jedem Lauf am Ende des Systemhinweises (`extras::instructions_hint`,
eingehängt in `agents/runs.rs`): erst die des Benutzers, in einem Projekt zusätzlich die des
Projekts – für jede Agent-Art gleich, auch für Subagenten und Bots.

**Sicherungen** sind Abbilder der Datenbank (`VACUUM INTO`, in sich stimmig auch im Betrieb) unter
`<data>/backups`: `lucode-<unix>.db` (von Hand) und `lucode-auto-<unix>.db` (täglich, die letzten
sieben bleiben; abschaltbar). Zurückspielen: luCode beenden, die Datei als `lucode.db` in den
Datenordner legen (`lucode.db-wal` und `lucode.db-shm` löschen), starten.

## Oberfläche

- **Befehlspalette** (`ui/src/pages/workspace/palette.rs`): Strg+K / ⌘K oder die Lupe oben rechts.
  Chats und Projekte kommen sofort aus dem Zustand der Oberfläche, Nachrichten über `/search`.
- **Erscheinungsbild** (`ui/tailwind.css`, `ui/src/state.rs`): Themen Nacht (`dark`, Vorgabe) und
  Tag (`light`), „wie das System“; `mondnacht` aus nrzCode gilt als Nacht. Akzentfarbe
  (`html[data-accent]`, `localStorage` `lu-accent`), Sternenhimmel (`html.lu-no-stars`, `lu-stars`),
  Milchglas der App (`html.lu-glass`, `lu-glass`, ab Werk aus).

## Veröffentlichen

Ein Release ist eine frische Instanz: Im Quelltext, in den Bildern der Doku und in den gebauten
Programmen steht nichts von einer bestimmten Installation – keine Konten, keine Verbindungen, keine
Adressen privater Git-Server, keine Tokens. Was eine Installation ausmacht, liegt allein in ihrem
Datenordner.

- Die Build-Skripte (`scripts/ui.sh`, `scripts/windows.sh`, `scripts/macos.sh`) blenden die Ordner
  der Maschine aus, auf der gebaut wird (`scripts/lib.sh`, `--remap-path-prefix`): Rust nennt in
  Fehlermeldungen die Pfade der Quelltexte, sonst stünde in jedem Programm der Benutzerordner
  dessen, der gebaut hat. Das Repository erscheint als `luCode/…`, Cargos Ordner als `cargo/…`.
- `scripts/check-clean.sh [dateien…]` prüft Quelltext und gebaute Dateien auf Adressen aus dem
  Tailscale-/CGNAT-Bereich (100.64.x.x bis 100.127.x.x), auf die Hosts aus `NRZGIT_URL`/`LUGIT_URL` der Umgebung
  und auf die Werte der Tokens der Umgebung, gebaute Dateien zusätzlich auf Benutzerordner der
  Build-Maschine. `scripts/windows.sh` ruft es für die gebauten Programme
  auf, `scripts/release.sh` vor jedem Hochladen.
- `scripts/release.sh github|lugit|nrzgit|all` legt die Dateien aus `dist-windows/` und
  `dist-macos/` als Release `v<version>` ab – auf GitHub (CLI `gh` oder die REST-API) und über den
  MCP-Server der Plattform in luGit und nrzGit. Adressen, Tokens und Repositories kommen aus der
  Umgebung (`GH_TOKEN`, `LUGIT_URL`/`LUGIT_TOKEN`/`LUGIT_REPO`, `NRZGIT_URL`/`NRZGIT_TOKEN`/
  `NRZGIT_REPO`); `DRY_RUN=1` zeigt nur, was geschähe.
- Die Apps suchen Updates in den GitHub-Releases von `Luna-OS/Lu-Code`; `LUCODE_UPDATE_REPO`
  (API-Adresse eines Repositorys, auch luGit/nrzGit) lenkt sie um. Noch keine Releases (`404`) gilt
  als „aktuell“.
- luCodeMobile kennt kein eingebautes Repository: `LUMOBILE_UPDATE_REPO=besitzer/name`.

