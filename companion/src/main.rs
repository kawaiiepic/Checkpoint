mod sync;
mod webdav;
mod cross_sync;
mod discovery;
mod status;
mod titles;
mod nup;
use std::{collections::BTreeMap, env, ffi::OsStr, fs, path::{Path, PathBuf}, process::Command, thread, time::{Duration, Instant, UNIX_EPOCH}};
type Files = BTreeMap<PathBuf, (u64,u128)>;

fn is_eglantine_process(proc: &Path, args: &[String], executable: &str, actual: &str) -> bool {
    // AppImage launches can report AppRun or a bundled binary as argv[0].
    if executable.contains("eglantine") || actual.contains("eglantine")
        || executable == "eden-cli" || actual == "eden-cli" { return true; }
    let appimage_arg = args.iter().any(|arg| {
        let lower = arg.to_ascii_lowercase();
        lower.contains("eglantine") && (lower.ends_with(".appimage") || lower.contains("/eglantine"))
    });
    if appimage_arg { return true; }
    let Ok(data) = fs::read(proc.join("environ")) else { return false };
    data.split(|byte| *byte == 0).any(|entry| {
        entry.starts_with(b"APPIMAGE=")
            && String::from_utf8_lossy(entry).to_ascii_lowercase().contains("eglantine")
    })
}

fn game_from_launch_path(value: &str) -> Option<String> {
    let path = Path::new(value);
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or("").to_ascii_lowercase();
    if ext == "nup" {
        if let Ok(Some(info)) = nup::inspect(path) {
            let fallback = path.file_stem().map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unknown game".to_owned());
            return Some(format!("{} [{}]", info.title.unwrap_or(fallback), info.title_id));
        }
    }
    if ["xci", "nsp", "nca", "nro", "nup"].contains(&ext.as_str()) {
        return path.file_stem().map(|name| name.to_string_lossy().into_owned());
    }
    // Support extracted games passed as .../exefs or .../exefs/main.
    let exefs = if path.file_name() == Some(OsStr::new("exefs")) {
        Some(path)
    } else if path.file_name() == Some(OsStr::new("main"))
        && path.parent().and_then(Path::file_name) == Some(OsStr::new("exefs")) {
        path.parent()
    } else { None };
    let exefs = exefs?;
    if !exefs.join("main").is_file() { return None; }
    let container = exefs.parent()?;
    // Extracted Switch 2 layouts may have a numeric container such as 131072.
    let name_path = if container.file_name().and_then(OsStr::to_str)
        .is_some_and(|name| name.chars().all(|c| c.is_ascii_digit())) {
        container.parent().unwrap_or(container)
    } else { container };
    name_path.file_name().map(|name| name.to_string_lossy().into_owned())
}

fn process_games() -> Vec<(u32,Option<String>)> {
    let mut games=Vec::new();
    if let Ok(procs)=fs::read_dir("/proc") {
        for p in procs.flatten() {
            let Ok(pid)=p.file_name().to_string_lossy().parse::<u32>() else {continue};
            let Ok(raw)=fs::read(p.path().join("cmdline")) else {continue};
            let args:Vec<String>=raw.split(|b|*b==0).filter(|b|!b.is_empty()).map(|b|String::from_utf8_lossy(b).into_owned()).collect();
            let executable=args.first().and_then(|a|Path::new(a).file_name()).and_then(OsStr::to_str).unwrap_or("").to_lowercase();
            let actual=fs::read_link(p.path().join("exe")).ok().and_then(|a|a.file_name().map(|n|n.to_string_lossy().to_lowercase())).unwrap_or_default();
            if !is_eglantine_process(&p.path(), &args, &executable, &actual) {continue}
            let mut game=None;
            for (i,arg) in args.iter().enumerate() {
                let path=if arg=="--game"||arg=="-g" {args.get(i+1).map(String::as_str)}
                    else if let Some(value)=arg.strip_prefix("--game="){Some(value)}
                    else {Some(arg.as_str())};
                if let Some(path)=path {
                    if let Some(name) = game_from_launch_path(path) {
                        game = Some(name);
                        break;
                    }
                }
            }
            games.push((pid,game));
        }
    }
    games.sort_by_key(|g|g.0);games
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BootEvent {
    title_id: String,
    title: String,
}
fn parse_boot_event(line: &str) -> Option<BootEvent> {
    // Eglantine: BootGame: Booting game: 04007EF01F8AE000 | FEFW ROM (64-bit) |
    let data = line.split("BootGame: Booting game: ").nth(1)?;
    let mut fields = data.split('|').map(str::trim);
    let id = fields.next()?;
    let title = fields.next()?;
    if !titles::is_switch2_id(id) || title.is_empty() {
        return None;
    }
    Some(BootEvent { title_id: id.to_ascii_uppercase(), title: title.to_owned() })
}
fn most_recent_boot(log: &Path) -> Option<BootEvent> {
    // Read only the last 256 KiB to avoid loading large logs into memory.
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines().rev().find_map(parse_boot_event)
}
fn log_candidates(home: &Path) -> Vec<PathBuf> {
    let xdg = env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut paths = Vec::new();
    for dir in ["eglantine", "Eglantine", "eden", "Eden"] {
        for filename in ["eden_log.txt", "eglantine_log.txt"] {
            paths.push(xdg.join(dir).join("log").join(filename));
        }
    }
    paths
}

fn session_label(games: &[(u32, Option<String>)]) -> String {
    let mut titles: Vec<&str> = games.iter().filter_map(|(_, name)| name.as_deref()).collect();
    titles.sort_unstable();
    titles.dedup();
    match titles.as_slice() {
        [] => "game unknown (emulator active)".to_owned(),
        [title] => (*title).to_owned(),
        _ => "multiple distinct ROM arguments detected".to_owned(),
    }
}
fn files(roots:&[PathBuf])->Files{
    let mut found=Files::new();
    for root in roots{
        let mut stack=vec![root.clone()];
        while let Some(dir)=stack.pop(){
            if let Ok(entries)=fs::read_dir(dir){
                for e in entries.flatten(){
                    let path=e.path();
                    let Ok(m)=fs::symlink_metadata(&path) else {continue};
                    if m.file_type().is_symlink(){continue}
                    if m.is_dir(){stack.push(path)}
                    else if m.is_file(){
                        let modified=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
                        found.insert(path,(m.len(),modified));
                    }
                    if found.len()>=50_000{return found}
                }
            }
        }
    }
    found
}
fn changed(a:&Files,b:&Files)->usize{
    b.iter().filter(|(p,v)|a.get(*p)!=Some(*v)).count()+a.keys().filter(|p|!b.contains_key(*p)).count()
}
fn notice(message:&str,desktop:bool){
    println!("[Checkpoint] {message}");
    if desktop {let _=Command::new("notify-send").args(["Checkpoint",message]).status();}
}
fn main(){
    let mut roots:Vec<PathBuf>=Vec::new();let mut remote=None;let mut active=30u64;let mut idle=30u64;let mut desktop=false;let mut once=false;let mut log_override:Option<PathBuf>=None;let mut refresh_titles=false;let mut status_json=false;let mut sync_once=false;let mut configure_sync:Option<(String,String)>=None;let mut configure_webdav:Option<(String,String)>=None;let mut test_webdav=false;let mut list_backups:Option<String>=None;
    let mut restore_backup:Option<String>=None;let mut restore_target:Option<PathBuf>=None;
    let mut confirm_restore=false;let mut sync_adopt:Option<String>=None;
    let mut args=env::args().skip(1);
    while let Some(a)=args.next(){
        match a.as_str(){
            "--save-dir"=>roots.push(PathBuf::from(args.next().expect("--save-dir requires a path"))),
            "--remote"=>remote=Some(args.next().expect("--remote requires a path")),
            "--log-file"=>log_override=Some(PathBuf::from(args.next().expect("--log-file requires a path"))),
            "--active-seconds"=>active=args.next().expect("Missing seconds").parse().expect("Invalid seconds"),
            "--idle-seconds"=>idle=args.next().expect("Missing seconds").parse().expect("Invalid seconds"),
            "--notify"=>desktop=true,"--once"=>once=true,
            "--refresh-titles"=>refresh_titles=true,
            "--status-json"=>status_json=true,
            "--backup-once"|"--sync-once"=>sync_once=true,
            "--sync-adopt"=>sync_adopt=args.next(),
            "--test-webdav"=>test_webdav=true,
            "--list-backups"=>list_backups=args.next(),
            "--restore-backup"=>restore_backup=args.next(),
            "--to"=>restore_target=args.next().map(PathBuf::from),
            "--confirm-restore"=>confirm_restore=true,
            "--configure-webdav"=>{
                let url=args.next().filter(|s|!s.starts_with("--"));
                let user=args.next().filter(|s|!s.starts_with("--"));
                match (url,user) {
                    (Some(url),Some(user))=>configure_webdav=Some((url,user)),
                    _=>{
                        eprintln!("Usage: checkpoint-companion --configure-webdav 'https://cloud.example/remote.php/dav/files/USER/Checkpoint' USER");
                        std::process::exit(2);
                    }
                }
            },
            "--configure-sync"=>{
                let backend = args.next().filter(|v| !v.starts_with("--"));
                let destination = args.next().filter(|v| !v.starts_with("--"));
                match (backend, destination) {
                    (Some(backend), Some(destination)) => configure_sync=Some((backend,destination)),
                    _ => {
                        eprintln!("Missing arguments for --configure-sync.");
                        eprintln!("Usage:");
                        eprintln!("  checkpoint-companion --configure-sync rclone 'mywebdav:Checkpoint'");
                        eprintln!("  checkpoint-companion --configure-sync rsync 'server:/backups/Checkpoint'");
                        std::process::exit(2);
                    }
                }
            },
            "-h"|"--help"=>{println!("checkpoint-companion [--save-dir PATH]... [--remote rclone:folder] [--log-file PATH] [--active-seconds 30] [--idle-seconds 30] [--refresh-titles] [--status-json] [--configure-sync BACKEND DEST] [--configure-webdav HTTPS_URL USER] [--test-webdav] [--sync-once] [--sync-adopt TITLE_ID] [--backup-once] [--list-backups TITLE_ID] [--restore-backup SNAPSHOT_REF --to SAVE_DIR --confirm-restore] [--notify] [--once]");return},
            _=>{eprintln!("Unknown argument: {a}");std::process::exit(2)}
        }
    }
    if let Some(title)=list_backups {
        match sync::list_cloud_backups(&title) {
            Ok(snapshots)=>{
                if snapshots.is_empty(){println!("[Checkpoint] No complete snapshots found for {title}");}
                else {for snapshot in snapshots {println!("{} ({} top-level files)",snapshot.relative,snapshot.files);}}
            }
            Err(error)=>{eprintln!("[Checkpoint] Cloud listing failed: {error}");std::process::exit(1)}
        }
        return;
    }
    if let Some(reference)=restore_backup {
        let Some(target)=restore_target else {
            eprintln!("Usage: --restore-backup SNAPSHOT_REF --to EXISTING_SAVE_DIRECTORY --confirm-restore");
            std::process::exit(2);
        };
        match sync::restore_snapshot(&reference,&target,confirm_restore) {
            Ok(message)=>println!("[Checkpoint] {message}"),
            Err(error)=>{eprintln!("[Checkpoint] Restore failed: {error}");std::process::exit(1)}
        }
        return;
    }
    if let Some((url,user))=configure_webdav {
        let password=match rpassword::prompt_password("Nextcloud app password: ") {
            Ok(password)=>password,
            Err(error)=>{eprintln!("Cannot read app password: {error}");std::process::exit(2)}
        };
        if let Err(error)=sync::configure_webdav(&url,&user,&password) {
            eprintln!("WebDAV configuration failed: {error}");
            std::process::exit(2);
        }
        println!("[Checkpoint] Native WebDAV connected; global destination saved");
        return;
    }
    if test_webdav {
        match sync::test_webdav() {
            Ok(())=>println!("[Checkpoint] WebDAV connection OK"),
            Err(error)=>{eprintln!("[Checkpoint] WebDAV test failed: {error}");std::process::exit(1)}
        }
        return;
    }
    if let Some((backend,destination))=configure_sync {
        if let Err(e)=sync::configure(&backend,&destination) {eprintln!("Config error: {e}");std::process::exit(2)}
        println!("[Checkpoint] Global backup destination saved");return;
    }
    if let Some(title)=sync_adopt {
        match cross_sync::run(Some(&title)) {
            Ok(result)=>{
                println!("[Checkpoint] Sync adoption: {} downloads, {} conflicts, {} errors",result.downloaded,result.conflicts,result.errors);
                if result.conflicts>0||result.errors>0{std::process::exit(1);}
            }
            Err(error)=>{eprintln!("[Checkpoint] Sync adoption failed: {error}");std::process::exit(1)}
        }
        return;
    }
    if sync_once {
        match sync::backup_idle() {
            Ok((uploaded,failed,total))=>{println!("[Checkpoint] Sync completed: {uploaded} uploads, {failed} conflicts/errors; {total} local saves paired");if failed>0{std::process::exit(1)}},
            Err(e)=>{eprintln!("[Checkpoint] Backup: {e}");std::process::exit(1)},
        }
        return;
    }
    if active==0||idle==0{eprintln!("Polling intervals must be greater than zero");std::process::exit(2)}
    if let Some(extra)=env::var_os("EGLANTINE_SAVE_DIRS"){roots.extend(env::split_paths(&extra))}
    if roots.is_empty(){
        let home=PathBuf::from(env::var_os("HOME").expect("HOME not set"));
        let xdg=env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or(home.join(".local/share"));
        for name in ["eglantine","Eglantine","eden","Eden"]{roots.push(xdg.join(name).join("nand/user/save"))}
    }
    let home=PathBuf::from(env::var_os("HOME").expect("HOME not set"));
    let logs=log_override.as_ref().map(|p|vec![p.clone()]).unwrap_or_else(||log_candidates(&home));
    if status_json {
        let snapshot=status::snapshot(&roots, log_override.as_ref());
        println!("{}",serde_json::to_string(&snapshot).expect("serialize snapshot"));
        return;
    }
    let title_names=titles::load_or_refresh(refresh_titles);
    println!("Checkpoint companion · save monitoring and idle cloud sync");
    match sync::backup_status() {
        Ok((false,_)) => eprintln!("[Checkpoint] WARNING: No backup destination configured. Use --configure-webdav HTTPS_URL USER or --configure-sync rclone/rsync DEST"),
        Ok((true,total)) => println!("[Checkpoint] Previously backed up games: {total}"),
        Err(error) => eprintln!("[Checkpoint] Backup config error: {error}"),
    }
    for p in &roots{println!("  watching {}",p.display())}
    let mut old_games:Option<Vec<(u32,Option<String>)>>=None;
    let mut old_files:Option<Files>=None;let mut old_remote:Option<Vec<u8>>=None;
    let mut pending:Option<(Instant,usize)>=None;
    let mut last_boot:Option<BootEvent>=None;
    let mut last_log_version:Option<(PathBuf,u64,std::time::SystemTime)>=None;
    loop{
        let games=process_games();let current=files(&roots);
        // Avoid attributing a boot line from a previous session to a new one.
        // Process detection is poll-based; only recently modified logs qualify on first sight.
        let mut observed_boot = None;
        if !games.is_empty() {
            for log in &logs {
                let Ok(meta)=fs::metadata(log) else {continue};
                let Ok(modified)=meta.modified() else {continue};
                let age=std::time::SystemTime::now().duration_since(modified).unwrap_or_default();
                let version=(log.clone(), meta.len(),modified);
                let changed_log=last_log_version.as_ref()!=Some(&version);
                let recently_written=age < Duration::from_secs(120);
                if changed_log && recently_written {
                    observed_boot=most_recent_boot(log);
                }
                last_log_version=Some(version);
                if observed_boot.is_some() {break}
            }
        } else {
            last_boot=None;
        }
        let boot_changed=observed_boot.as_ref().is_some_and(|event|last_boot.as_ref()!=Some(event));
        if let Some(event)=observed_boot {last_boot=Some(event);}

        let session_changed=old_games.as_ref().map_or(true,|old|*old!=games);
        if session_changed{
            if games.is_empty(){if old_games.as_ref().is_some_and(|x|!x.is_empty()){notice("Eglantine exited",desktop)}}
            else{
                let game=last_boot.as_ref().map(|event|titles::display(&event.title_id,&event.title,&title_names))
                    .unwrap_or_else(||session_label(&games));
                notice(&format!("Eglantine detected: {game}"),desktop)
            }
            pending=None;
        }else if boot_changed {
            if let Some(event)=&last_boot { notice(&format!("Game boot detected: {}",titles::display(&event.title_id,&event.title,&title_names)),desktop); }
        }
        if !session_changed {
          if let Some(old)=&old_files{
            let n=changed(old,&current);
            if n>0{pending=Some((Instant::now(),pending.map_or(n,|(_,v)|n.max(v))))}
            else if let Some((since,count))=pending{
                if since.elapsed()>=Duration::from_secs(6){
                    let game=last_boot.as_ref().map(|event|format!("{} [{}]",event.title,event.title_id))
                        .unwrap_or_else(||session_label(&games));
                    notice(&format!("Local save updated ({count} changed files; {game}). No sync performed."),desktop);
                    pending=None
                }
            }
        }
        }
        println!("[Checkpoint] {} Eglantine process(es), {} save files",games.len(),current.len());
        if let Some(boot) = &last_boot {
            let found=discovery::discover(&discovery::candidate_roots(&roots,&games.iter().map(|(pid,_)|*pid).collect::<Vec<_>>()), &boot.title_id);
            for save in found {
                let timestamp = save.last_modified_ms
                    .map(|ms|format!("last modified {} (Unix ms)",ms))
                    .unwrap_or_else(||"last modified unknown".to_owned());
                println!("[Checkpoint] Save: {} ({}, profile {}, {} files, {})",save.path,save.layout,save.profile,save.file_count,timestamp);
            }
        }
        old_games=Some(games.clone());old_files=Some(current);
        if let Some(path)=&remote{
            match Command::new("rclone").args(["lsf","--recursive","--files-only","--format","pst",path]).output(){
                Ok(out) if out.status.success()=>{
                    if old_remote.as_ref().is_some_and(|previous|*previous!=out.stdout){
                        notice("Remote listing changed; review before restoring saves",desktop);
                    }
                    old_remote=Some(out.stdout);
                },
                Ok(out)=>eprintln!("[Checkpoint] rclone error: {}",String::from_utf8_lossy(&out.stderr)),
                Err(e)=>eprintln!("[Checkpoint] could not run rclone: {e}"),
            }
        }
        if games.is_empty() {
            match sync::backup_idle() {
                Ok((uploaded,failed,total)) if uploaded>0 || failed>0 => {
                    println!("[Checkpoint] Automatic backup: {uploaded} new snapshots, {failed} failed; {total} games backed up");
                }
                Err(err) if !err.starts_with("No backup destination configured") =>
                    eprintln!("[Checkpoint] Automatic backup: {err}"),
                _ => {}
            }
        }
        if once{break}
        thread::sleep(Duration::from_secs(if games.is_empty(){idle}else{active}));
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn appimage_processes_are_not_games(){
        assert_eq!(session_label(&[(1,None),(2,None)]), "game unknown (emulator active)");
        assert_eq!(session_label(&[(1,None),(2,Some("Example".into()))]), "Example");
        assert_eq!(session_label(&[(1,Some("Example".into())),(2,Some("Example".into()))]), "Example");
    }
    #[test]fn parses_eglantine_boot_log(){
        let line="[   4.375378] Frontend <Info> yuzu/main_window.cpp:2160:BootGame: Booting game: 04007EF01F8AE000 | FEFW ROM (64-bit) |";
        assert_eq!(parse_boot_event(line),Some(BootEvent{
            title_id:"04007EF01F8AE000".into(),title:"FEFW ROM (64-bit)".into()
        }));
        assert_eq!(parse_boot_event("BootGame: Eglantine starting..."),None);
    }
    #[test]fn nup_rom_detection(){
        assert_eq!(game_from_launch_path("/games/Fire Emblem.nup").as_deref(), Some("Fire Emblem"));
        assert_eq!(game_from_launch_path("/games/Game.xci").as_deref(), Some("Game"));
    }
    #[test]fn change_count(){let mut a=Files::new();a.insert(PathBuf::from("save"),(2,1));let mut b=a.clone();assert_eq!(changed(&a,&b),0);b.insert(PathBuf::from("next"),(2,1));assert_eq!(changed(&a,&b),1);}
}
