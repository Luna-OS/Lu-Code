//! Dateien des Umzugs: Ordner kopieren (mit Fortschritt), die Verweise der Git-Worktrees auf den
//! neuen Ort stellen und die Sitzungen von Claude Code unter den neuen Arbeitsordnern ablegen,
//! damit sich übernommene Chats nahtlos fortsetzen lassen.
//!
//! Alles hier blockiert (läuft in `spawn_blocking`).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

/// Zähler für den Fortschritt (Dateien, Bytes).
#[derive(Default)]
pub struct Progress {
    pub files: AtomicI64,
    pub bytes: AtomicI64,
}

/// Ordnernamen, die nur Zwischenstände eines Builds enthalten und beim nächsten Build neu
/// entstehen. Dazu zählt jeder Ordner mit `CACHEDIR.TAG` (z. B. `target` von Cargo).
const BUILD_DIRS: &[&str] =
    &["node_modules", ".venv", "__pycache__", ".gradle", ".next", ".turbo", ".parcel-cache", ".pytest_cache", ".mypy_cache", ".ruff_cache"];

fn is_build_dir(path: &Path, name: &str) -> bool {
    BUILD_DIRS.contains(&name) || path.join("CACHEDIR.TAG").is_file()
}

/// Was beim Kopieren ausgelassen wurde.
#[derive(Default, Debug)]
pub struct CopyReport {
    /// Übersprungene Build-Ordner
    pub skipped_dirs: i64,
    /// Verweise, die sich nicht nachbauen liessen
    pub skipped_links: i64,
    /// Einträge, die sich nicht kopieren liessen (Pfad: Grund) – höchstens einige
    pub failed: Vec<String>,
    pub failed_count: i64,
}

impl CopyReport {
    fn fail(&mut self, path: &Path, e: &io::Error) {
        self.failed_count += 1;
        if self.failed.len() < 5 {
            self.failed.push(format!("{}: {e}", path.display()));
        }
    }
}

/// Ordner samt Inhalt kopieren. Vorhandene, gleich grosse und nicht ältere Dateien bleiben (so
/// setzt ein wiederholter Import fort, statt neu zu beginnen). `skip_build`: Build-Ordner
/// auslassen – nie innerhalb von `.git`.
pub fn copy_tree(src: &Path, dst: &Path, skip_build: bool, progress: &Progress, report: &mut CopyReport) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    copy_dir(src, dst, skip_build, false, progress, report);
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path, skip_build: bool, in_git: bool, progress: &Progress, report: &mut CopyReport) {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) => return report.fail(src, &e),
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let name = entry.file_name();
        let to = dst.join(&name);
        let meta = match std::fs::symlink_metadata(&from) {
            Ok(m) => m,
            Err(e) => {
                report.fail(&from, &e);
                continue;
            }
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            // Verweise (unter Windows auch Abzweigungen wie die im Benutzerprofil) lassen sich nicht
            // immer nachbauen – sie zeigen ohnehin auf etwas ausserhalb und fehlen niemandem
            if copy_link(&from, &to).is_err() {
                report.skipped_links += 1;
            }
        } else if kind.is_dir() {
            let name = name.to_string_lossy();
            let git = in_git || name == ".git";
            if skip_build && !git && is_build_dir(&from, &name) {
                report.skipped_dirs += 1;
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&to) {
                report.fail(&to, &e);
                continue;
            }
            copy_dir(&from, &to, skip_build, git, progress, report);
        } else if kind.is_file() {
            match copy_file(&from, &to, &meta) {
                Ok(n) => {
                    progress.files.fetch_add(1, Ordering::Relaxed);
                    progress.bytes.fetch_add(n as i64, Ordering::Relaxed);
                }
                Err(e) => report.fail(&from, &e),
            }
        }
    }
}

/// Datei kopieren; gibt die Grösse zurück. Schreibgeschützte Ziele (Git-Objekte) werden ersetzt.
fn copy_file(from: &Path, to: &Path, meta: &std::fs::Metadata) -> io::Result<u64> {
    if let Ok(have) = std::fs::metadata(to) {
        let newer = match (have.modified(), meta.modified()) {
            (Ok(a), Ok(b)) => a >= b,
            _ => false,
        };
        if have.is_file() && have.len() == meta.len() && newer {
            return Ok(meta.len());
        }
        let mut perms = have.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(to, perms);
        }
        let _ = std::fs::remove_file(to);
    }
    let n = std::fs::copy(from, to)?;
    // Unter Unix bleibt die Änderungszeit sonst die des Kopierens – git hielte jede Datei für geändert
    #[cfg(not(windows))]
    if let Ok(mtime) = meta.modified() {
        if let Ok(f) = std::fs::File::options().write(true).open(to) {
            let _ = f.set_modified(mtime);
        }
    }
    Ok(n)
}

#[cfg(unix)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    let target = std::fs::read_link(from)?;
    let _ = std::fs::remove_file(to);
    std::os::unix::fs::symlink(target, to)
}

#[cfg(windows)]
fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    let target = std::fs::read_link(from)?;
    let _ = std::fs::remove_file(to);
    let _ = std::fs::remove_dir(to);
    // Verweise brauchen unter Windows ein Recht (Entwicklermodus); ohne es bleibt der Eintrag aus
    if std::fs::metadata(from).map(|m| m.is_dir()).unwrap_or(false) {
        std::os::windows::fs::symlink_dir(target, to)
    } else {
        std::os::windows::fs::symlink_file(target, to)
    }
}

#[cfg(not(any(unix, windows)))]
fn copy_link(_from: &Path, _to: &Path) -> io::Result<()> {
    Ok(())
}

/// Pfad für Git-Dateien: immer mit `/` (so schreibt es git auch unter Windows).
fn git_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Verweise eines kopierten Worktrees auf den neuen Ort stellen: `<worktree>/.git` zeigt auf
/// `<repo>/.git/worktrees/<name>`, dessen `gitdir` zurück auf `<worktree>/.git`. Ohne das zeigten
/// die Kopien weiter auf das Repository in nrzCode. Gibt `false` zurück, wenn der Ordner kein
/// Worktree dieses Repositorys ist.
pub fn relink_worktree(repo: &Path, worktree: &Path) -> io::Result<bool> {
    let dot_git = worktree.join(".git");
    if !dot_git.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&dot_git)?;
    let Some(old) = text.lines().find_map(|l| l.strip_prefix("gitdir:")).map(str::trim) else {
        return Ok(false);
    };
    // Name des Verwaltungsordners = letzter Teil des alten Verweises
    let Some(name) = old.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty()) else {
        return Ok(false);
    };
    let admin = repo.join(".git").join("worktrees").join(name);
    if !admin.is_dir() {
        return Ok(false);
    }
    std::fs::write(&dot_git, format!("gitdir: {}\n", git_path(&admin)))?;
    std::fs::write(admin.join("gitdir"), format!("{}\n", git_path(&dot_git)))?;
    Ok(true)
}

/// Ordnername, unter dem Claude Code die Sitzungen eines Arbeitsordners ablegt
/// (`<home>/.claude/projects/<name>`): der Pfad, jedes Zeichen ausser Buchstaben und Ziffern als `-`.
pub fn claude_slug(path: &Path) -> String {
    path.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Zuordnung alter zu neuen Nummern für die Ordner unter dem Datenordner.
#[derive(Default, Clone)]
pub struct IdMaps {
    pub projects: HashMap<i64, i64>,
    pub threads: HashMap<i64, i64>,
    pub bots: HashMap<i64, i64>,
}

/// Führende Zahl eines Rests wie `12-repo` → (12, `-repo`).
fn leading_number(s: &str) -> Option<(i64, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// Rest eines Sitzungs-Ordners (nach dem Datenordner) auf die neuen Nummern umschreiben, z. B.
/// `workspaces-6-worktrees-7` → `workspaces-9-worktrees-31`. `None`: gehört zu etwas, das nicht
/// übernommen wurde.
fn map_rest(rest: &str, maps: &IdMaps) -> Option<String> {
    if let Some(r) = rest.strip_prefix("workspaces-") {
        let (pid, after) = leading_number(r)?;
        let new_pid = maps.projects.get(&pid)?;
        if let Some(w) = after.strip_prefix("-worktrees-") {
            let (tid, tail) = leading_number(w)?;
            let new_tid = maps.threads.get(&tid)?;
            return Some(format!("workspaces-{new_pid}-worktrees-{new_tid}{tail}"));
        }
        return Some(format!("workspaces-{new_pid}{after}"));
    }
    if let Some(r) = rest.strip_prefix("scratch-") {
        let (tid, tail) = leading_number(r)?;
        return Some(format!("scratch-{}{tail}", maps.threads.get(&tid)?));
    }
    if let Some(r) = rest.strip_prefix("bots-") {
        let (bid, tail) = leading_number(r)?;
        return Some(format!("bots-{}{tail}", maps.bots.get(&bid)?));
    }
    None
}

/// Sitzungen von Claude Code in einem kopierten Home umziehen: Ordner, die zu Arbeitsordnern
/// unter dem alten Datenordner gehören, bekommen den Namen des neuen Arbeitsordners. Gibt die
/// Zahl der umbenannten Ordner zurück.
pub fn move_claude_sessions(home: &Path, src_root: &Path, dst_root: &Path, maps: &IdMaps) -> usize {
    let projects = home.join(".claude").join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else { return 0 };
    let old_prefix = format!("{}-", claude_slug(src_root)).to_lowercase();
    let new_prefix = format!("{}-", claude_slug(dst_root));
    let mut moved = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Windows unterscheidet Gross und Klein im Pfad nicht
        if name.len() < old_prefix.len() || name[..old_prefix.len()].to_lowercase() != old_prefix {
            continue;
        }
        let Some(rest) = map_rest(&name[old_prefix.len()..], maps) else { continue };
        let target = projects.join(format!("{new_prefix}{rest}"));
        if target.exists() {
            continue;
        }
        if std::fs::rename(entry.path(), &target).is_ok() {
            moved += 1;
        }
    }
    moved
}

/// Liegt die Sitzung `session` von Claude Code für den Arbeitsordner `workdir` im Home?
pub fn claude_session_exists(home: &Path, workdir: &Path, session: &str) -> bool {
    !session.is_empty() && home.join(".claude").join("projects").join(claude_slug(workdir)).join(format!("{session}.jsonl")).is_file()
}

/// Absoluter Pfad ohne Auflösen von Verweisen (so, wie ihn Prozesse als Arbeitsordner sehen).
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lucode-import-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slug_like_claude_code() {
        assert_eq!(claude_slug(Path::new(r"C:\Users\Ada\AppData\Roaming\nrzCode\workspaces\12\repo")), "C--Users-Ada-AppData-Roaming-nrzCode-workspaces-12-repo");
        assert_eq!(claude_slug(Path::new("/data/workspaces/3/worktrees/9")), "-data-workspaces-3-worktrees-9");
    }

    #[test]
    fn rest_follows_new_ids() {
        let mut maps = IdMaps::default();
        maps.projects.insert(6, 9);
        maps.threads.insert(7, 31);
        maps.bots.insert(2, 5);
        assert_eq!(map_rest("workspaces-6-repo", &maps).as_deref(), Some("workspaces-9-repo"));
        assert_eq!(map_rest("workspaces-6-repo-build-run", &maps).as_deref(), Some("workspaces-9-repo-build-run"));
        assert_eq!(map_rest("workspaces-6-worktrees-7", &maps).as_deref(), Some("workspaces-9-worktrees-31"));
        assert_eq!(map_rest("scratch-7", &maps).as_deref(), Some("scratch-31"));
        assert_eq!(map_rest("bots-2", &maps).as_deref(), Some("bots-5"));
        assert_eq!(map_rest("workspaces-8-repo", &maps), None);
        assert_eq!(map_rest("tmp-runs-13", &maps), None);
    }

    #[test]
    fn copies_and_skips_build_dirs() {
        let src = tmp("src");
        let dst = tmp("dst");
        std::fs::create_dir_all(src.join("app/node_modules/x")).unwrap();
        std::fs::create_dir_all(src.join("app/target")).unwrap();
        std::fs::create_dir_all(src.join("app/.git/node_modules")).unwrap();
        std::fs::write(src.join("app/main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("app/node_modules/x/index.js"), "x").unwrap();
        std::fs::write(src.join("app/target/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        std::fs::write(src.join("app/.git/node_modules/keep"), "k").unwrap();
        let progress = Progress::default();
        let mut report = CopyReport::default();
        copy_tree(&src, &dst, true, &progress, &mut report).unwrap();
        assert!(dst.join("app/main.rs").is_file());
        assert!(!dst.join("app/node_modules").exists());
        assert!(!dst.join("app/target").exists());
        assert!(dst.join("app/.git/node_modules/keep").is_file(), "in .git wird nichts ausgelassen");
        assert_eq!(report.skipped_dirs, 2);
        assert_eq!(progress.files.load(Ordering::Relaxed), 2);
        // Ohne Auslassen kommt alles mit; schon Kopiertes bleibt
        let mut report = CopyReport::default();
        copy_tree(&src, &dst, false, &progress, &mut report).unwrap();
        assert!(dst.join("app/node_modules/x/index.js").is_file());
        assert_eq!(report.failed_count, 0);
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(dst);
    }

    #[test]
    fn worktree_links_point_to_the_new_place() {
        let root = tmp("wt");
        let repo = root.join("workspaces/4/repo");
        let wt = root.join("workspaces/4/worktrees/21");
        std::fs::create_dir_all(repo.join(".git/worktrees/7")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(repo.join(".git/worktrees/7/gitdir"), "C:/old/nrzCode/workspaces/4/worktrees/7/.git\n").unwrap();
        std::fs::write(wt.join(".git"), "gitdir: C:/old/nrzCode/workspaces/4/repo/.git/worktrees/7\n").unwrap();
        assert!(relink_worktree(&repo, &wt).unwrap());
        let link = std::fs::read_to_string(wt.join(".git")).unwrap();
        assert_eq!(link.trim(), format!("gitdir: {}", git_path(&repo.join(".git/worktrees/7"))));
        let back = std::fs::read_to_string(repo.join(".git/worktrees/7/gitdir")).unwrap();
        assert_eq!(back.trim(), git_path(&wt.join(".git")));
        // Kein Worktree: nichts zu tun
        assert!(!relink_worktree(&repo, &root).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_sessions_move_with_the_workdir() {
        let home = tmp("home");
        let projects = home.join(".claude/projects");
        let src_root = Path::new(r"C:\Users\Ada\AppData\Roaming\nrzCode");
        let dst_root = Path::new(r"C:\Users\Ada\AppData\Roaming\luCode");
        for name in ["C--Users-Ada-AppData-Roaming-nrzCode-workspaces-6-worktrees-7", "C--Users-Ada-AppData-Roaming-nrzCode-scratch-17", "C--Users-Ada-AppData-Roaming-nrzCode-tmp-runs-13", "C--Other-place"] {
            std::fs::create_dir_all(projects.join(name)).unwrap();
        }
        std::fs::write(projects.join("C--Users-Ada-AppData-Roaming-nrzCode-workspaces-6-worktrees-7/abc.jsonl"), "{}").unwrap();
        let mut maps = IdMaps::default();
        maps.projects.insert(6, 6);
        maps.threads.insert(7, 40);
        maps.threads.insert(17, 17);
        assert_eq!(move_claude_sessions(&home, src_root, dst_root, &maps), 2);
        assert!(projects.join("C--Users-Ada-AppData-Roaming-luCode-workspaces-6-worktrees-40/abc.jsonl").is_file());
        assert!(projects.join("C--Users-Ada-AppData-Roaming-luCode-scratch-17").is_dir());
        assert!(projects.join("C--Users-Ada-AppData-Roaming-nrzCode-tmp-runs-13").is_dir(), "Unbekanntes bleibt liegen");
        assert!(claude_session_exists(&home, &dst_root.join("workspaces").join("6").join("worktrees").join("40"), "abc"));
        assert!(!claude_session_exists(&home, &dst_root.join("workspaces").join("6").join("repo"), "abc"));
        let _ = std::fs::remove_dir_all(home);
    }
}
