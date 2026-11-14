//! Global-config, automatic read-only-source backups (NOT two-way synchronization).
//! Cloud restore deliberately remains disabled pending safe merge/conflict logic.
use crate::{discovery, webdav};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet}, env, fs, io,
    path::{Path, PathBuf}, process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub backend: String,
    pub destination: String,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub username: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub password: Option<String>,
}
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
    if !["rclone", "rsync", "webdav"].contains(&config.backend.as_str())
        || config.destination.is_empty() || config.destination.starts_with('-')
        || config.destination.contains('\n') || config.destination.contains('\r') {
        return Err("Expected backend webdav/rclone/rsync and a nonempty destination".into());
    }
    if config.backend == "rsync" && config.destination.contains("://") {
        return Err("rsync destination must be a local path or SSH-style host:path".into());
    }
    if config.backend == "webdav" {
        webdav::validate_url(&config.destination)?;
        if config.username.as_deref().is_none_or(str::is_empty)
            || config.password.as_deref().is_none_or(str::is_empty) {
            return Err("WebDAV requires a username and app password".into());
        }
    }
    Ok(())
}
fn save_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("No parent directory")?;
    fs::create_dir_all(parent).map_err(|e|e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?;
    let temp = path.with_extension("tmp");
    // Secrets may be stored in the config file; enforce owner-only permissions.
    use std::io::Write as _;
    use std::fs::OpenOptions;
    let mut options=OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output=options.open(&temp).map_err(|e|e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(0o600)).map_err(|e|e.to_string())?;
    }
    output.write_all(&bytes).map_err(|e|e.to_string())?;
    output.sync_all().map_err(|e|e.to_string())?;
    drop(output);
    fs::rename(temp, path).map_err(|e|e.to_string())
}
pub fn configure(backend: &str, destination: &str) -> Result<(), String> {
    let cfg = Config {backend: backend.into(),destination: destination.into(),username: None,password: None};
    validate(&cfg)?;
    save_atomic(&config_path(), &cfg)
}
pub fn configure_webdav(destination: &str, username: &str, password: &str) -> Result<(),String> {
    let cfg = Config {
        backend: "webdav".into(), destination: destination.into(),
        username: Some(username.into()),password: Some(password.into())
    };
    validate(&cfg)?;
    // Prevent silently storing credentials for an unreachable endpoint.
    webdav::test(&cfg)?;
    save_atomic(&config_path(),&cfg)
}
pub fn test_webdav() -> Result<(),String> {
    let cfg=load()?.ok_or("No backup destination configured")?;
    if cfg.backend != "webdav" {return Err("Configured backend is not native WebDAV".into());}
    webdav::test(&cfg)
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
    if cfg.backend == "webdav" { return webdav::upload(cfg,source,target_suffix); }
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
            "No backup destination configured (missing {}). Usage: --configure-webdav HTTPS_URL USER or --configure-sync rclone REMOTE:Checkpoint or --configure-sync rsync HOST:/path/Checkpoint",
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
            // Also prune old revisions on an unchanged save, for existing users.
            if cfg.backend=="webdav" {
                if let Err(error)=webdav::keep_one_snapshot(&cfg,&folder,None) {
                    failures+=1;
                    eprintln!("[Checkpoint] Cloud retention failed for {id}: {error}");
                }
            }
            continue
        }
        // Upload a complete replacement first, then prune obsolete revisions.
        // This preserves the previous snapshot if the new upload fails.
        let millis=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let revision=format!("{folder}/snapshots/{millis}-{}",save.signature);
        match upload(&cfg,Path::new(&save.path),&revision) {
            Ok(()) => {
                state.uploads.insert(key,save.signature);
                state.backed_up_titles.entry(destination.clone()).or_default().insert(id.clone());
                save_atomic(&path,&state)?;
                dirty=false;
                done+=1;
                if cfg.backend=="webdav" {
                    let revision_name=revision.rsplit('/').next().unwrap_or("");
                    match webdav::keep_one_snapshot(&cfg,&folder,Some(revision_name)) {
                        Ok(removed) if removed>0=>println!(
                            "[Checkpoint] Removed {removed} older cloud snapshot(s) for {id}"
                        ),
                        Ok(_)=>{},
                        Err(error)=>{
                            failures+=1;
                            eprintln!("[Checkpoint] Uploaded {id}, but remote cleanup failed: {error}");
                        }
                    }
                }
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
        assert!(validate(&Config{backend:"rclone".into(),destination:"mywebdav:Checkpoint".into(),username:None,password:None}).is_ok());
        assert!(validate(&Config{backend:"rsync".into(),destination:"server:/backups/Checkpoint".into(),username:None,password:None}).is_ok());
        assert!(validate(&Config{backend:"rsync".into(),destination:"-bad".into(),username:None,password:None}).is_err());
    }
}


pub fn list_cloud_backups(title: &str) -> Result<Vec<webdav::Snapshot>,String> {
    let cfg=load()?.ok_or("No backup destination configured")?;
    if cfg.backend!="webdav" {return Err("Cloud restore currently requires the native WebDAV backend".into());}
    webdav::list_snapshots(&cfg,title)
}

pub fn restore_snapshot(reference: &str, target: &Path, confirm: bool) -> Result<String,String> {
    if !confirm {return Err("Restore requires --confirm-restore and an explicit --to destination".into());}
    let cfg=load()?.ok_or("No backup destination configured")?;
    if cfg.backend!="webdav" {return Err("Cloud restore currently requires native WebDAV".into());}
    if !crate::process_games().is_empty() {
        return Err("Close Eglantine completely before restoring saves".into());
    }
    let segments:Vec<&str>=reference.split('/').collect();
    if segments.len()!=7 {return Err("Invalid snapshot reference".into());}
    let title=segments[1];
    if !discovery::valid_title_id(title) {return Err("Invalid title ID".into());}
    let expected_path=fs::canonicalize(target).map_err(|e|format!("Save directory must already exist: {e}"))?;
    if !expected_path.is_dir() {return Err("Restore target must be an existing save directory".into());}
    if fs::symlink_metadata(target).map_err(|e|e.to_string())?.file_type().is_symlink() {
        return Err("Refusing to restore through a symlink".into());
    }
    let roots=discovery::candidate_roots(&[],&[]);
    let matching=discovery::discover(&roots,title);
    if !matching.iter().any(|s|Path::new(&s.path)==expected_path) {
        return Err(format!("Restore target is not a discovered save directory for title {title}; use --save-dir or ensure Eglantine has created a local save first"));
    }
    // Keep staging in the save's parent, ensuring rename stays on one filesystem.
    let parent=expected_path.parent().ok_or("Restore destination has no parent")?;
    let name=expected_path.file_name().ok_or("Invalid restore target name")?.to_string_lossy();
    let nonce=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let prefix=format!(".checkpoint-restore-{name}-{}-{nonce}",std::process::id());
    let staging=parent.join(format!("{prefix}-staging"));
    let previous=parent.join(format!("{prefix}-previous"));
    fs::create_dir(&staging).map_err(|e|format!("Cannot create restore staging: {e}"))?;
    let downloaded=webdav::download_snapshot(&cfg,reference,&staging);
    let files=match downloaded {
        Ok(files)=>files,
        Err(error)=>{
            let _=fs::remove_dir_all(&staging);
            return Err(format!("Restore download failed; local save was not changed: {error}"));
        }
    };
    // Refuse to replace anything containing symlinks; retain the original unchanged.
    let mut pending=vec![expected_path.clone()];
    while let Some(folder)=pending.pop() {
        for entry in fs::read_dir(&folder).map_err(|e|e.to_string())? {
            let entry=entry.map_err(|e|e.to_string())?;
            let meta=fs::symlink_metadata(entry.path()).map_err(|e|e.to_string())?;
            if meta.file_type().is_symlink() {
                let _=fs::remove_dir_all(&staging);
                return Err("Original save contains symlinks; refusing restore".into());
            }
            if meta.is_dir(){pending.push(entry.path());}
        }
    }
    if let Err(error)=fs::rename(&expected_path,&previous) {
        let _=fs::remove_dir_all(&staging);
        return Err(format!("Cannot preserve existing save: {error}"));
    }
    if let Err(error)=fs::rename(&staging,&expected_path) {
        let rollback=fs::rename(&previous,&expected_path);
        return Err(format!("Cannot install restore: {error}; rollback: {rollback:?}; staging: {}",staging.display()));
    }
    Ok(format!("Restored {files} files to {}. Original save preserved at {}",
        expected_path.display(),previous.display()))
}
