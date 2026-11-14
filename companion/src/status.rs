//! Machine-readable one-shot status, consumed by Decky and the CLI.
use crate::discovery::{self, SaveLocation};
use serde::Serialize;
use std::{path::PathBuf, time::{Duration, SystemTime}};
#[derive(Serialize)]
pub struct Snapshot {
    pub running: bool,
    pub process_count: usize,
    pub game_running: bool,
    pub game: Option<String>,
    pub title_id: Option<String>,
    pub save_roots: Vec<String>,
    pub saves: Vec<SaveLocation>,
    pub watched_files: usize,
    pub available_saves: usize,
    pub available_titles: usize,
    pub last_modified_ms: Option<u64>,
    pub backup_configured: bool,
    pub backed_up_games: usize,
    pub backup_error: Option<String>,
}
pub fn snapshot(extra: &[PathBuf], log_override: Option<&PathBuf>) -> Snapshot {
    let processes = crate::process_games();
    let pids: Vec<u32> = processes.iter().map(|(pid,_)| *pid).collect();
    let roots = discovery::candidate_roots(extra, &pids);
    let logs = log_override.map(|p| vec![p.clone()])
        .unwrap_or_else(|| crate::log_candidates(&PathBuf::from(
            std::env::var_os("HOME").unwrap_or_default())));
    // Steam ROM Manager launches Eglantine with a ROM argument, which we can
    // identify from /proc without consulting logs. No file handles required.
    let launched = processes.iter().filter_map(|(_,name)|name.as_deref()).filter_map(|name|{
        let (title,raw)=name.rsplit_once(" [")?;
        let id=raw.strip_suffix(']')?;
        if !discovery::valid_title_id(id){return None;}
        Some(crate::BootEvent{title_id:id.to_owned(),title:title.to_owned()})
    }).collect::<Vec<_>>();
    let mut boot = if launched.len()==1 {launched.into_iter().next()} else {None};
    // A recent boot log can identify a GUI-launched game for display, but
    // cannot prove the game is still active.
    if boot.is_none() && !processes.is_empty() {
        for path in logs {
            let Ok(metadata) = std::fs::metadata(&path) else { continue };
            let Ok(mtime) = metadata.modified() else { continue };
            if SystemTime::now().duration_since(mtime).unwrap_or(Duration::MAX) > Duration::from_secs(120) {
                continue;
            }
            if let Some(event) = crate::most_recent_boot(&path) {
                boot = Some(event);
                break;
            }
        }
    }
    let title_id = boot.as_ref().map(|event|event.title_id.clone());
    let game = boot.as_ref().map(|event|event.title.clone()).or_else(|| {
        if processes.is_empty() { None } else { Some(crate::session_label(&processes)) }
    });
    let saves = title_id.as_ref().map(|id|discovery::discover(&roots, id)).unwrap_or_default();
    let watched_files = saves.iter().map(|item|item.file_count).sum();
    // Idle inventory is independent of the last booted game. It is read-only
    // and does not establish that a title is installed or eligible for sync.
    let inventory = crate::sync::entries(&roots);
    let available_saves = inventory.len();
    let available_titles = inventory.iter().map(|(id,_)|id).collect::<std::collections::BTreeSet<_>>().len();
    let last_modified_ms = saves.iter().filter_map(|item|item.last_modified_ms).max();
    let (backup_configured, backed_up_games, backup_error) = match crate::sync::backup_status() {
        Ok((configured,count)) => (configured,count,None),
        Err(error) => (false,0,Some(error)),
    };
    Snapshot {
        running: !processes.is_empty(), process_count: processes.len(),
        game_running: crate::activity::game_active_or_unknown(), game, title_id,
        save_roots: roots.iter().map(|r|r.to_string_lossy().into_owned()).collect(),
        saves, watched_files, available_saves, available_titles, last_modified_ms,
        backup_configured, backed_up_games, backup_error
    }
}
