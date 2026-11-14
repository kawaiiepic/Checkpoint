//! Global-config, automatic read-only-source backups (NOT two-way synchronization).
//! Cloud restore deliberately remains disabled pending safe merge/conflict logic.
use crate::discovery;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet}, env, fs, io,
    path::{Path, PathBuf}, process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config { pub backend: String, pub destination: String }
#[derive(Default, Serialize, Deserialize)]
struct State {
    uploads: BTreeMap<String, String>,
    #[serde(default)]
    backed_up_titles: BTreeMap<String, BTreeSet<String>>,
}

fn home() -> PathBuf { PathBuf::from(env::var_os("HOME").unwrap_or_default()) }
pub fn config_path() -> PathBuf {
    env::var_os("CHECKPOINT_CONFIG").map(PathBuf::from)
        .unwrap_or_else(|| env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config")).join("checkpoint/config.json"))
}
fn state_path() -> PathBuf {
    env::var_os("XDG_STATE_HOME").map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state")).join("checkpoint/uploaded.json")
}
fn validate(config: &Config) -> Result<(), String> {
    if !["rclone", "rsync"].contains(&config.backend.as_str())
        || config.destination.is_empty() || config.destination.starts_with('-')
        || config.destination.contains('\n') || config.destination.contains('\r') {
        return Err("Expected backend rclone/rsync and a nonempty destination".into());
    }
    if config.backend == "rsync" && config.destination.contains("://") {
        return Err("rsync destination must be a local path or SSH-style host:path".into());
    }
    Ok(())
}
fn save_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("No parent directory")?;
    fs::create_dir_all(parent).map_err(|e|e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(|e|e.to_string())?;
    fs::rename(temp, path).map_err(|e|e.to_string())
}
pub fn configure(backend: &str, destination: &str) -> Result<(), String> {
    let cfg = Config {backend: backend.into(),destination: destination.into()};
    validate(&cfg)?;
    save_atomic(&config_path(), &cfg)
}
pub fn load() -> Result<Option<Config>, String> {
    let path = config_path();
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let cfg: Config = serde_json::from_slice(&data).map_err(|e|e.to_string())?;
    validate(&cfg)?;
    Ok(Some(cfg))
}
fn entries(roots: &[PathBuf]) -> Vec<(String, discovery::SaveLocation)> {
    let mut ids = BTreeSet::new();
    for root in roots {
        if let Ok(profiles) = fs::read_dir(root.join("account")) {
            for profile in profiles.flatten() {
                if let Ok(titles) = fs::read_dir(profile.path()) {
                    for title in titles.flatten() {
                        let id=title.file_name().to_string_lossy().to_ascii_uppercase();
                        if discovery::valid_title_id(&id) {ids.insert(id);}
                    }
                }
            }
        }
        for bucket in ["device", "0000000000000000"] {
            let path=root.join(bucket);
            let directory = if bucket == "device" {true} else {false};
            if directory {
                if let Ok(titles)=fs::read_dir(&path) {
                    for title in titles.flatten() {
                        let id=title.file_name().to_string_lossy().to_ascii_uppercase();
                        if discovery::valid_title_id(&id) {ids.insert(id);}
                    }
                }
            } else if let Ok(profiles)=fs::read_dir(&path) {
                for profile in profiles.flatten() {
                    if let Ok(titles)=fs::read_dir(profile.path()) {
                        for title in titles.flatten() {
                            let id=title.file_name().to_string_lossy().to_ascii_uppercase();
                            if discovery::valid_title_id(&id) {ids.insert(id);}
                        }
                    }
                }
            }
        }
    }
    let mut saves = Vec::new();
    for id in ids {
        for save in discovery::discover(roots, &id) {
            saves.push((id.clone(), save));
        }
    }
    saves
}
fn device_id() -> String {
    // Namespaces are intentionally per installation to avoid collision between devices.
    // Stable Linux machine-id is hashed so it isn't exposed verbatim remotely.
    use sha2::{Digest, Sha256};
    let source = fs::read("/etc/machine-id").unwrap_or_else(|_|home().to_string_lossy().as_bytes().to_vec());
    let hash=Sha256::digest(source);
    format!("machine-{}",hash[..8].iter().map(|v|format!("{v:02x}")).collect::<String>())
}
fn part_name(name: &str) -> String {
    name.chars().filter(|c|c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(80).collect()
}
fn upload(cfg: &Config, source: &Path, target_suffix: &str) -> Result<(), String> {
    let target = format!("{}/{}",cfg.destination.trim_end_matches('/'),target_suffix);
    let (program, arguments): (&str, Vec<String>)=if cfg.backend=="rclone" {
        ("rclone",vec!["copy".into(),"--ignore-existing".into(),"--".into(),
            source.to_string_lossy().into_owned(),target])
    } else {
        ("rsync",vec!["-a".into(),"--ignore-existing".into(),"--mkpath".into(),"--".into(),
            format!("{}/",source.display()),format!("{target}/")])
    };
    let result=Command::new(program).args(&arguments).output()
        .map_err(|e|format!("Cannot run {program}: {e}"))?;
    if !result.status.success() {
        return Err(format!("{program} failed: {}",String::from_utf8_lossy(&result.stderr)));
    }
    Ok(())
}
fn destination_key(cfg: &Config) -> String {
    format!("{}|{}", cfg.backend, cfg.destination)
}
pub fn backup_status() -> Result<(bool, usize), String> {
    let Some(cfg) = load()? else {return Ok((false,0))};
    let state = fs::read(state_path()).ok()
        .and_then(|bytes|serde_json::from_slice::<State>(&bytes).ok())
        .unwrap_or_default();
    let count = state.backed_up_titles.get(&destination_key(&cfg)).map_or(0,BTreeSet::len);
    Ok((true,count))
}
pub fn backup_idle() -> Result<(usize,usize,usize), String> {
    let Some(cfg) = load()? else {
        return Err(format!(
            "No backup destination configured (missing {}). Usage: --configure-sync rclone REMOTE:Checkpoint or --configure-sync rsync HOST:/path/Checkpoint",
            config_path().display()
        ));
    };
    if !crate::process_games().is_empty() {
        return Err("Eglantine is running; refusing to back up active saves".into());
    }
    let roots=discovery::candidate_roots(&[],&[]);
    let path=state_path();
    let mut state=fs::read(&path).ok().and_then(|buf|serde_json::from_slice::<State>(&buf).ok())
        .unwrap_or_default();
    let device=device_id();
    let mut done=0usize;
    let mut failures=0usize;
    let destination=destination_key(&cfg);
    let mut dirty=false;
    for (id, save) in entries(&roots) {
        let profile=part_name(&save.profile);
        let profile=if profile.is_empty() {"unknown".into()} else {profile};
        let folder=format!("switch2/{id}/{}/{}/{}",save.layout,profile,device);
        let key=format!("{}|{}|{}",cfg.backend,cfg.destination,save.path);
        if state.uploads.get(&key).is_some_and(|old| old==&save.signature) {
            if state.backed_up_titles.entry(destination.clone()).or_default().insert(id.clone()) {
                dirty=true;
            }
            continue
        }
        // An immutable timestamped revision avoids overwriting existing remote data.
        // A revision is never pruned automatically. No delete or remote restore here.
        let millis=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let revision=format!("{folder}/snapshots/{millis}-{}",save.signature);
        match upload(&cfg,Path::new(&save.path),&revision) {
            Ok(()) => {
                state.uploads.insert(key,save.signature);
                state.backed_up_titles.entry(destination.clone()).or_default().insert(id.clone());
                save_atomic(&path,&state)?;
                dirty=false;
                done+=1;
            }
            Err(error) => {failures+=1;eprintln!("[Checkpoint] Backup failed for {id}: {error}");}
        }
    }
    if dirty {save_atomic(&path,&state)?;}
    let total=state.backed_up_titles.get(&destination).map_or(0,BTreeSet::len);
    Ok((done,failures,total))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn global_config_validation() {
        assert!(validate(&Config{backend:"rclone".into(),destination:"mywebdav:Checkpoint".into()}).is_ok());
        assert!(validate(&Config{backend:"rsync".into(),destination:"server:/backups/Checkpoint".into()}).is_ok());
        assert!(validate(&Config{backend:"rsync".into(),destination:"-bad".into()}).is_err());
    }
}
