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
                    let ext=Path::new(path).extension().and_then(OsStr::to_str).unwrap_or("").to_lowercase();
                    if ["xci","nsp","nca","nro"].contains(&ext.as_str()){
                        game=Path::new(path).file_stem().map(|s|s.to_string_lossy().into_owned());break
                    }
                }
            }
            games.push((pid,game));
        }
    }
    games.sort_by_key(|g|g.0);games
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
    let mut roots:Vec<PathBuf>=Vec::new();let mut remote=None;let mut active=30u64;let mut idle=30u64;let mut desktop=false;let mut once=false;
    let mut args=env::args().skip(1);
    while let Some(a)=args.next(){
        match a.as_str(){
            "--save-dir"=>roots.push(PathBuf::from(args.next().expect("--save-dir requires a path"))),
            "--remote"=>remote=Some(args.next().expect("--remote requires a path")),
            "--active-seconds"=>active=args.next().expect("Missing seconds").parse().expect("Invalid seconds"),
            "--idle-seconds"=>idle=args.next().expect("Missing seconds").parse().expect("Invalid seconds"),
            "--notify"=>desktop=true,"--once"=>once=true,
            "-h"|"--help"=>{println!("checkpoint-companion [--save-dir PATH]... [--remote rclone:folder] [--active-seconds 30] [--idle-seconds 30] [--notify] [--once]");return},
            _=>{eprintln!("Unknown argument: {a}");std::process::exit(2)}
        }
    }
    if active==0||idle==0{eprintln!("Polling intervals must be greater than zero");std::process::exit(2)}
    if let Some(extra)=env::var_os("EGLANTINE_SAVE_DIRS"){roots.extend(env::split_paths(&extra))}
    if roots.is_empty(){
        let home=PathBuf::from(env::var_os("HOME").expect("HOME not set"));
        let xdg=env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or(home.join(".local/share"));
        for name in ["eglantine","Eglantine","eden","Eden"]{roots.push(xdg.join(name).join("nand/user/save"))}
    }
    println!("Checkpoint companion · read-only; no uploads or downloads");
    for p in &roots{println!("  watching {}",p.display())}
    let mut old_games:Option<Vec<(u32,Option<String>)>>=None;
    let mut old_files:Option<Files>=None;let mut old_remote:Option<Vec<u8>>=None;
    let mut pending:Option<(Instant,usize)>=None;
    loop{
        let games=process_games();let current=files(&roots);
        let session_changed=old_games.as_ref().map_or(true,|old|*old!=games);
        if session_changed{
            if games.is_empty(){if old_games.as_ref().is_some_and(|x|!x.is_empty()){notice("Eglantine exited",desktop)}}
            else{notice(&format!("Eglantine detected: {}", session_label(&games)),desktop)}
            pending=None;
        }else if let Some(old)=&old_files{
            let n=changed(old,&current);
            if n>0{pending=Some((Instant::now(),pending.map_or(n,|(_,v)|n.max(v))))}
            else if let Some((since,count))=pending{
                if since.elapsed()>=Duration::from_secs(6){
                    let game=session_label(&games);
                    notice(&format!("Local save updated ({count} changed files; {game}). No sync performed."),desktop);
                    pending=None
                }
            }
        }
        println!("[Checkpoint] {} Eglantine process(es), {} save files",games.len(),current.len());
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
    #[test]fn change_count(){let mut a=Files::new();a.insert(PathBuf::from("save"),(2,1));let mut b=a.clone();assert_eq!(changed(&a,&b),0);b.insert(PathBuf::from("next"),(2,1));assert_eq!(changed(&a,&b),1);}
}
