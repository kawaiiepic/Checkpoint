//! Conflict-safe sync of a single cloud "current" save per game.
//! Local recovery copies survive replaces; old per-device snapshot folders are untouched.
use crate::{discovery, installed, sync, webdav};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet}, env, fs,
    io::{Read, Write}, path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, Serialize, Deserialize)]
struct SyncState { bases: BTreeMap<String, Base> }
#[derive(Clone, Serialize, Deserialize)]
struct Base { generation: String, digest: String }
#[derive(Default)]
pub struct Summary {pub uploaded:usize,pub downloaded:usize,pub conflicts:usize,pub errors:usize,pub games:usize}

fn state_path()->PathBuf {
    let home=PathBuf::from(env::var_os("HOME").unwrap_or_default());
    env::var_os("XDG_STATE_HOME").map(PathBuf::from)
        .unwrap_or_else(||home.join(".local/state")).join("checkpoint/sync.json")
}
fn read_state()->Result<SyncState,String>{
    match fs::read(state_path()) {
        Ok(buf)=>serde_json::from_slice(&buf).map_err(|e|format!("Invalid sync state: {e}")),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(SyncState::default()),
        Err(e)=>Err(e.to_string()),
    }
}
fn store_state(state:&SyncState)->Result<(),String>{
    let path=state_path();
    let folder=path.parent().ok_or("No sync state parent")?;
    fs::create_dir_all(folder).map_err(|e|e.to_string())?;
    let temporary=path.with_extension("sync-tmp");
    let mut options=fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut file=options.open(&temporary).map_err(|e|e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(state).map_err(|e|e.to_string())?)
        .map_err(|e|e.to_string())?;
    file.sync_all().map_err(|e|e.to_string())?;
    drop(file);
    fs::rename(temporary,path).map_err(|e|e.to_string())
}
fn digest_file(path:&Path)->Result<(u64,String),String>{
    let mut file=fs::File::open(path).map_err(|e|format!("Read {}: {e}",path.display()))?;
    let mut hasher=Sha256::new();
    let mut size=0u64;let mut chunk=[0u8;65536];
    loop {
        let n=file.read(&mut chunk).map_err(|e|e.to_string())?;
        if n==0 {break;}
        size=size.saturating_add(n as u64);
        hasher.update(&chunk[..n]);
    }
    Ok((size,format!("{:x}",hasher.finalize())))
}
fn collect(source:&Path)->Result<(Vec<webdav::CloudFile>,String),String>{
    let mut files=Vec::new();
    let mut stack=vec![PathBuf::new()];
    while let Some(relative)=stack.pop(){
        for item in fs::read_dir(source.join(&relative)).map_err(|e|e.to_string())? {
            let item=item.map_err(|e|e.to_string())?;
            let name=item.file_name().into_string().map_err(|_|"Non UTF-8 filename")?;
            if name.is_empty()||name=="."||name==".."||name.contains('%')||name.contains('\\') {
                return Err("Unsupported save filename".into());
            }
            let path=relative.join(&name);
            let meta=fs::symlink_metadata(item.path()).map_err(|e|e.to_string())?;
            if meta.file_type().is_symlink() {return Err("Refusing symlink in save".into());}
            if meta.is_dir() {
                stack.push(path);
            }else if meta.is_file(){
                let (size,sha256)=digest_file(&item.path())?;
                files.push(webdav::CloudFile{
                    path:path.to_string_lossy().replace('\\',"/"),size,sha256
                });
                if files.len()>50000 {return Err("Too many save files".into());}
            }else {return Err("Unsupported special save file".into());}
        }
    }
    files.sort_by(|a,b|a.path.cmp(&b.path));
    if files.is_empty(){return Err("Empty save not eligible for cloud sync".into());}
    let mut hasher=Sha256::new();
    for f in &files {
        hasher.update(f.path.as_bytes());hasher.update([0]);
        hasher.update(f.size.to_le_bytes());hasher.update([0]);
        hasher.update(f.sha256.as_bytes());hasher.update([0]);
    }
    Ok((files,format!("{:x}",hasher.finalize())))
}
fn unique_generation()->String {
    let time=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{time}-{}",std::process::id())
}
fn host_id()->String {
    let source=fs::read("/etc/machine-id").unwrap_or_else(|_|{
        env::var("HOME").unwrap_or_default().into_bytes()
    });
    let hash=Sha256::digest(source);
    format!("{:x}",hash)[..16].to_string()
}
fn install(cfg:&sync::Config,current:&webdav::CurrentResponse,source:&Path,
    id:&str,expected_local:&str)->Result<(),String>{
    if crate::activity::game_active_or_unknown(){return Err("Emulator started; refusing restore".into());}
    let parent=source.parent().ok_or("Invalid save parent")?;
    let stem=source.file_name().ok_or("Invalid save name")?.to_string_lossy();
    let name=format!(".checkpoint-sync-{stem}-{}",unique_generation());
    let staged=parent.join(format!("{name}-download"));
    let recovery=parent.join(format!("{name}-recovery"));
    fs::create_dir(&staged).map_err(|e|e.to_string())?;
    if let Err(error)=webdav::download_current(cfg,&current.manifest,&staged) {
        let _=fs::remove_dir_all(&staged);
        return Err(format!("Cloud download failed; local untouched: {error}"));
    }
    let (_,hash)=collect(&staged)?;
    if hash!=current.manifest.digest {
        let _=fs::remove_dir_all(&staged);
        return Err("Cloud manifest checksum mismatch; local untouched".into());
    }
    // The cloud pointer must not change while downloading.
    if webdav::get_current(cfg,id)?.as_ref().map(|x|x.etag.as_str())
       !=Some(current.etag.as_str()) {
        let _=fs::remove_dir_all(&staged);
        return Err("Cloud changed during download; local save untouched".into());
    }
    if crate::activity::game_active_or_unknown(){let _=fs::remove_dir_all(&staged);return Err("Emulator started during download".into());}
    if collect(source)?.1!=expected_local {
        let _=fs::remove_dir_all(&staged);
        return Err("Local save changed during download; refusing to replace it".into());
    }
    fs::rename(source,&recovery).map_err(|e|format!("Could not preserve original save: {e}"))?;
    if let Err(e)=fs::rename(&staged,source) {
        let rollback=fs::rename(&recovery,source);
        return Err(format!("Could not install new save: {e}; original rollback: {rollback:?}"));
    }
    println!("[Checkpoint] Synced {id} from cloud; previous local save at {}",recovery.display());
    Ok(())
}

/// Stage and validate a cloud save for an installed title with no local save yet.
/// A single verified account profile is required; no game data is cached otherwise.
fn install_missing(cfg:&sync::Config,current:&webdav::CurrentResponse,
    destination:&Path,id:&str)->Result<(),String>{
    if current.manifest.save_layout.as_deref()!=Some("account") {
        return Err("Cloud save predates account-layout metadata. Run the updated Checkpoint --sync-once on the original PC with its matching local account save, then retry here".into());
    }
    if destination.exists() {return Err("Local destination appeared during sync".into());}
    let parent=destination.parent().ok_or("Missing save parent")?;
    // Parent is created only after verifying that the target belongs to one known profile.
    fs::create_dir_all(parent).map_err(|e|e.to_string())?;
    let staged=parent.join(format!(".checkpoint-new-{}",unique_generation()));
    fs::create_dir(&staged).map_err(|e|e.to_string())?;
    let outcome=(||{
        webdav::download_current(cfg,&current.manifest,&staged)?;
        let (_,digest)=collect(&staged)?;
        if digest!=current.manifest.digest{return Err("Cloud content hash mismatch".into());}
        if crate::activity::game_active_or_unknown(){return Err("Emulator started during sync".into());}
        if webdav::get_current(cfg,id)?.as_ref().map(|x|x.etag.as_str())!=Some(current.etag.as_str()) {
            return Err("Cloud changed during download".into());
        }
        if destination.exists(){return Err("Save destination appeared during download".into());}
        fs::rename(&staged,destination).map_err(|e|format!("Install missing save: {e}"))?;
        Ok(())
    })();
    if outcome.is_err(){let _=fs::remove_dir_all(&staged);}
    outcome
}

fn save_key(cfg:&sync::Config,id:&str,save:&discovery::SaveLocation)->String{
    format!("{}|{}|{}",cfg.destination,id,save.path)
}
/// Upgrade a legacy current.json that predates save_layout. Only the machine
/// with a matching local account save can attest to its layout. Updating the
/// pointer conditionally preserves the immutable cloud data generation.
fn attest_legacy_layout(
    cfg:&sync::Config,id:&str,save:&discovery::SaveLocation,local_digest:&str,
    current:webdav::CurrentResponse,
)->Result<webdav::CurrentResponse,String>{
    if current.manifest.save_layout.is_some() || save.layout!="account" ||
       current.manifest.digest!=local_digest {
        return Ok(current);
    }
    let mut updated=current.manifest.clone();
    updated.save_layout=Some("account".into());
    webdav::publish_current(cfg,&updated,Some(&current.etag))?;
    println!("[Checkpoint] Verified and upgraded legacy account-save metadata for {id}");
    let published=webdav::get_current(cfg,id)?
        .ok_or("Cloud pointer disappeared after metadata update")?;
    if published.manifest.generation!=updated.generation ||
       published.manifest.digest!=updated.digest ||
       published.manifest.save_layout.as_deref()!=Some("account"){
        return Err("Cloud pointer changed after metadata update; retry sync".into());
    }
    Ok(published)
}

fn one(cfg:&sync::Config,id:&str,save:&discovery::SaveLocation,
    state:&mut SyncState,adopt:bool)->Result<&'static str,String>{
    let source=Path::new(&save.path);
    let (files,local)=collect(source)?;
    let key=save_key(cfg,id,save);
    let previous=state.bases.get(&key).cloned();
    let remote=webdav::get_current(cfg,id)?;
    let remote=match remote {
        Some(current)=>Some(attest_legacy_layout(cfg,id,save,&local,current)?),
        None=>None,
    };
    match (previous,remote) {
        (None,None)=>{
            // First machine seeds the empty cloud. Conditional creation protects races.
            let manifest=webdav::CloudCurrent{
                version:1,title:id.into(),generation:unique_generation(),digest:local.clone(),
                writer:host_id(),save_layout:Some(save.layout.clone()),files
            };
            webdav::upload_generation(cfg,source,&manifest)?;
            if collect(source)?.1!=local{return Err("Save changed during upload; cloud pointer not published".into());}
            webdav::publish_current(cfg,&manifest,None)?;
            state.bases.insert(key,Base{generation:manifest.generation,digest:local});
            store_state(state)?;
            Ok("uploaded")
        }
        (None,Some(current))=>{
            if current.manifest.digest==local {
                state.bases.insert(key,Base{generation:current.manifest.generation,digest:local});
                store_state(state)?;
                return Ok("unchanged");
            }
            if !adopt {
                return Err(format!("CONFLICT {id}: local and cloud saves differ; run --sync-adopt {id} once on this device to replace its local save (local recovery copy kept)"));
            }
            install(cfg,&current,source,id,&local)?;
            state.bases.insert(key,Base{
                generation:current.manifest.generation,digest:current.manifest.digest
            });
            store_state(state)?;
            Ok("downloaded")
        }
        (Some(base),Some(current))=>{
            let local_changed=local!=base.digest;
            let remote_changed=current.manifest.generation!=base.generation ||
                current.manifest.digest!=base.digest;
            match (local_changed,remote_changed) {
                (false,false)=>Ok("unchanged"),
                (false,true)=>{
                    install(cfg,&current,source,id,&local)?;
                    state.bases.insert(key,Base{generation:current.manifest.generation,
                        digest:current.manifest.digest});
                    store_state(state)?;
                    Ok("downloaded")
                }
                (true,false)=>{
                    let manifest=webdav::CloudCurrent{
                        version:1,title:id.into(),generation:unique_generation(),
                        digest:local.clone(),writer:host_id(),save_layout:Some(save.layout.clone()),files
                    };
                    webdav::upload_generation(cfg,source,&manifest)?;
                    if collect(source)?.1!=local{return Err("Local save changed during upload; cloud pointer unchanged".into());}
                    webdav::publish_current(cfg,&manifest,Some(&current.etag))?;
                    state.bases.insert(key,Base{generation:manifest.generation.clone(),digest:local});
                    store_state(state)?;
                    // One referenced cloud generation. Deletion is safe after conditional pointer update.
                    if let Err(e)=webdav::delete_generation(cfg,id,&current.manifest.generation) {
                        eprintln!("[Checkpoint] Warning: old cloud data cleanup failed for {id}: {e}");
                    }
                    Ok("uploaded")
                }
                (true,true) if adopt=>{
                    // Explicit user choice: cloud wins, with original save retained locally.
                    install(cfg,&current,source,id,&local)?;
                    state.bases.insert(key,Base{generation:current.manifest.generation,
                        digest:current.manifest.digest});
                    store_state(state)?;
                    Ok("downloaded")
                }
                (true,true)=>Err(format!("CONFLICT {id}: both local and cloud saves changed; no overwrite performed. To preserve local progress, stop here; to choose cloud and keep a local recovery copy, run --sync-adopt {id}"))
            }
        }
        (Some(_),None)=>Err(format!("CONFLICT {id}: cloud save missing after previous sync; refusing to overwrite or recreate it automatically. If you intentionally deleted the cloud copy, run --sync-reseed {id}"))
    }
}
struct Guard{path:PathBuf}
impl Drop for Guard{fn drop(&mut self){let _=fs::remove_file(&self.path);}}
fn lock()->Result<Guard,String>{
    let path=state_path().with_extension("lock");
    fs::create_dir_all(path.parent().ok_or("Invalid lock path")?).map_err(|e|e.to_string())?;
    fs::OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e|format!("Another Checkpoint sync is running (or stale lock at {}): {e}",path.display()))?;
    Ok(Guard{path})
}
pub fn count()->usize{
    read_state().map(|s|s.bases.len()).unwrap_or(0)
}
pub fn run(adopt:Option<&str>)->Result<Summary,String>{
    let cfg=sync::load()?.ok_or("No cloud destination configured")?;
    if cfg.backend!="webdav" {return Err("Cross-device sync requires native WebDAV".into());}
    if crate::activity::game_active_or_unknown(){return Err("Eglantine is running; sync only runs while closed".into());}
    let _guard=lock()?;
    let mut state=read_state()?;
    // This allowlist is mandatory, even for existing saves. Extracted games
    // and uninstalled games must never be silently uploaded or downloaded.
    let library=installed::load()?;
    let mut groups:BTreeMap<String,Vec<discovery::SaveLocation>>=BTreeMap::new();
    for (id,save) in sync::local_entries() {
        if library.titles.contains(&id){groups.entry(id).or_default().push(save);}
    }
    let mut summary=Summary::default();
    if let Some(title)=adopt {
        if !discovery::valid_title_id(title) {return Err("Invalid title ID".into());}
        if !groups.contains_key(title) {return Err("No local save found for that title. Create an in-game save first".into());}
    }
    // Only installed NUP titles are considered. Never download saves for
    // cloud-only or uninstalled games.
    for title in &library.titles {
        if groups.contains_key(title){continue;}
        if adopt.is_some()&&adopt!=Some(title.as_str()){continue;}
        match webdav::get_current(&cfg,title) {
            Ok(Some(current))=>{
                match installed::missing_account_directory(&library,title)
                    .and_then(|path|{
                        install_missing(&cfg,&current,&path,title)?;
                        let key=format!("{}|{}|{}",cfg.destination,title,path.display());
                        state.bases.insert(key,Base{
                            generation:current.manifest.generation.clone(),
                            digest:current.manifest.digest.clone()
                        });
                        store_state(&state)?;
                        Ok(())
                    }){
                    Ok(())=>{
                        summary.downloaded+=1;
                        println!("[Checkpoint] Installed missing save from cloud for {title}");
                    },
                    Err(e)=>{
                        summary.conflicts+=1;
                        eprintln!("[Checkpoint] CONFLICT {title}: {e}");
                    }
                }
            }
            Ok(None)=>{},
            Err(e)=>{summary.errors+=1;eprintln!("[Checkpoint] Cloud check failed for {title}: {e}");}
        }
    }
    for (title,saves) in groups {
        if adopt.is_some()&&adopt!=Some(title.as_str()){continue;}
        if saves.len()!=1 {
            summary.conflicts+=1;
            eprintln!("[Checkpoint] CONFLICT {title}: multiple local profiles; not safe to choose one");
            continue;
        }
        match one(&cfg,&title,&saves[0],&mut state,adopt==Some(title.as_str())) {
            Ok("uploaded")=>{summary.uploaded+=1;println!("[Checkpoint] Published current cloud save for {title}");},
            Ok("downloaded")=>summary.downloaded+=1,
            Ok(_)=>{},
            Err(e) if e.starts_with("CONFLICT")=>{summary.conflicts+=1;eprintln!("[Checkpoint] {e}");},
            Err(e)=>{summary.errors+=1;eprintln!("[Checkpoint] Sync error for {title}: {e}");},
        }
    }
    summary.games=state.bases.keys().count();
    Ok(summary)
}

/// Explicit recovery when the user intentionally removed the remote Checkpoint folder.
/// Does not clear the on-disk sync base until a fresh cloud save has been published.
pub fn reseed(title: &str) -> Result<(), String> {
    if !discovery::valid_title_id(title) {return Err("Invalid Switch 2 title ID".into());}
    if crate::activity::game_active_or_unknown() {
        return Err("Close Eglantine before recreating the cloud save".into());
    }
    let cfg=sync::load()?.ok_or("No cloud destination configured")?;
    if cfg.backend!="webdav" {
        return Err("Cloud reseeding requires native WebDAV".into());
    }
    let _guard=lock()?;
    let mut state=read_state()?;
    let library=installed::load()?;
    if !library.titles.contains(title) {
        return Err("Game is not installed as a verified NUP; reseed is disabled".into());
    }
    let saves=sync::local_entries().into_iter()
        .filter(|(id,_)|id==title).map(|(_,save)|save).collect::<Vec<_>>();
    if saves.len()!=1 {
        return Err(format!("Expected exactly one existing local save for {title}, found {}",saves.len()));
    }
    let save=&saves[0];
    let key=save_key(&cfg,title,save);
    if !state.bases.contains_key(&key) {
        return Err("This game has no previous sync association; use --sync-once instead".into());
    }
    if webdav::get_current(&cfg,title)?.is_some() {
        return Err("A cloud current save already exists; refusing to replace it. Resolve the conflict instead".into());
    }
    // In-memory only: if publication fails, the durable sync association is unchanged.
    let previous=state.bases.remove(&key);
    match one(&cfg,title,save,&mut state,false) {
        Ok("uploaded") => {
            println!("[Checkpoint] Recreated cloud current save from local {title}; local save unchanged");
            Ok(())
        }
        Ok(_) => Err("Unexpected reseed result".into()),
        Err(error) => {
            if let Some(base)=previous { state.bases.insert(key,base); }
            Err(format!("Cloud reseed failed; previous association preserved: {error}"))
        }
    }
}
