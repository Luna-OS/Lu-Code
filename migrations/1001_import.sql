-- luCode – Umzug von nrzCode. Eigene Migrationen von luCode beginnen bei 1001: 0001–0019 sind
-- unverändert die von nrzCode (gleiche Prüfsummen, gleiches Schema – darauf baut der Import).
--
-- import_map merkt sich, welche Zeile der Quelle (source = Kennung der nrzCode-Instanz) zu welcher
-- Zeile hier wurde. So lässt sich der Import wiederholen, ohne etwas doppelt anzulegen, und
-- abgebrochenes Kopieren von Dateien (files_done = 0) wird beim nächsten Mal nachgeholt.
CREATE TABLE import_map (
    source TEXT NOT NULL,
    tbl TEXT NOT NULL,
    old_id INTEGER NOT NULL,
    new_id INTEGER NOT NULL,
    files_done INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (source, tbl, old_id)
);
CREATE INDEX import_map_new ON import_map (tbl, new_id);

-- Protokoll der Importe (je Lauf eine Zeile; summary = ImportCounts als JSON)
CREATE TABLE import_runs (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    path TEXT NOT NULL,
    user_id INTEGER,
    summary TEXT NOT NULL DEFAULT '{}',
    error TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX import_runs_source ON import_runs (source, id);
