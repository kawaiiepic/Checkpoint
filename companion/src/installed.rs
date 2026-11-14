//! Installed-game allowlist from Eglantine's Qt config.
//! Only verified base-game NUP metadata may enable a title for sync.
use crate::{discovery, nup};
use std::{collections::{BTreeMap,BTreeSet},env,fs,path::{Path,PathBuf}};

pub struct Library {
    pub titles:BTreeSet<String>,
    pub save_roots:Vec<PathBuf>,
}
fn home()->PathBuf {PathBuf::from(env::var_os("HOME").unwrap_or_default())}
fn config_candidates()->Vec<PathBuf>{
    if let Some(path)=env::var_os("CHECKPOINT_QT_CONFIG"){return vec![PathBuf::from(path)];}
    let root=env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(||home().join(".config"));
    ["eglantine","Eglantine","eden","Eden"].iter().map(|p|root.join(p).join("qt-config.ini")).collect()
}
fn value(raw:&str)->String {
    let value=raw.trim();
    // Qt's INI string literals may be enclosed in quotes.
    if value.len()>=2 && value.starts_with('"')&&value.ends_with('"'){
        value[1..value.len()-1].to_string()
    }else{value.to_string()}
}
fn settings(path:&Path)->Result<Library,String>{
    let content=fs::read_to_string(path).map_err(|e|format!("Read {}: {e}",path.display()))?;
    let mut section="";
    let mut directories:BTreeMap<usize,String>=BTreeMap::new();
    let mut deep:BTreeMap<usize,bool>=BTreeMap::new();
    let mut nand=None;
    for line in content.lines() {
        let line=line.trim();
        if line.starts_with('[')&&line.ends_with(']'){section=&line[1..line.len()-1];continue;}
        let Some((key,val))=line.split_once('=') else {continue};
        if key.ends_with(r"\default") {continue;}
        if section=="UI" {
            if let Some(rest)=key.strip_prefix(r"Paths\gamedirs\") {
                if let Some((index,field))=rest.split_once('\\'){
                    if let Ok(index)=index.parse::<usize>(){
                        if (1..=512).contains(&index){
                            if field=="path" {directories.insert(index,value(val));}
                            if field=="deep_scan" {deep.insert(index,val.trim()=="true");}
                        }
                    }
                }
            }
        }else if section=="Data%20Storage" && key=="nand_directory"{
            nand=Some(PathBuf::from(value(val)));
        }
    }
    let mut titles=BTreeSet::new();
    for (index,dir) in directories {
        // Skip virtual Qt directory labels and non-existing paths.
        let path=PathBuf::from(dir);
        if !path.is_absolute() || !path.is_dir(){continue;}
        let canonical=match fs::canonicalize(&path){Ok(p)=>p,Err(_)=>continue};
        let mut stack=vec![(canonical,0usize)];
        let recursively=deep.get(&index).copied().unwrap_or(false);
        let mut seen=BTreeSet::new();
        while let Some((folder,depth))=stack.pop(){
            if !seen.insert(folder.clone()){continue;}
            let Ok(read)=fs::read_dir(&folder) else {continue};
            for entry in read.flatten(){
                let Ok(kind)=entry.file_type() else {continue};
                if kind.is_symlink(){continue;}
                if kind.is_dir(){
                    if recursively && depth<16 {
                        stack.push((entry.path(),depth+1));
                    }
                    continue;
                }
                if !kind.is_file() || !entry.path().extension().and_then(|e|e.to_str()).is_some_and(|e|e.eq_ignore_ascii_case("nup")){continue;}
                if let Ok(Some(info))=nup::inspect(&entry.path()){
                    if info.base_game && discovery::valid_title_id(&info.title_id){
                        titles.insert(info.title_id);
                    }
                }
            }
        }
    }
    let mut save_roots=Vec::new();
    if let Some(nand)=nand {
        let save=nand.join("user/save");
        if let Ok(root)=fs::canonicalize(save) {
            if root.is_dir(){save_roots.push(root);}
        }
    }
    Ok(Library{titles,save_roots})
}
pub fn load()->Result<Library,String>{
    for config in config_candidates(){
        if config.is_file(){return settings(&config);}
    }
    Err("Cannot locate Eglantine qt-config.ini; refusing sync without installed NUP allowlist. Set CHECKPOINT_QT_CONFIG if necessary".into())
}

/// Find a deterministic destination for an account save that has never existed.
/// Never infer a profile from a default index or use a path that may belong to a different user.
pub fn missing_account_directory(roots:&[PathBuf],id:&str)->Result<PathBuf,String>{
    let mut candidates=BTreeSet::new();
    let program=u64::from_str_radix(id,16).map_err(|e|e.to_string())? & !0xff;
    for root in roots {
        let account=root.join("account");
        if !fs::symlink_metadata(&account).is_ok_and(|m|m.is_dir() && !m.file_type().is_symlink()) {continue;}
        if let Ok(profiles)=fs::read_dir(account){
            for profile in profiles.flatten(){
                let Ok(ty)=profile.file_type() else {continue};
                if !ty.is_dir()||ty.is_symlink(){continue;}
                let name=profile.file_name().to_string_lossy().into_owned();
                // Profile directory names must resemble the emulator's UUID identifiers.
                let normalized=name.replace('-',"");
                if normalized.len()!=32 || !normalized.bytes().all(|b|b.is_ascii_hexdigit()){continue;}
                candidates.insert(profile.path().join(format!("{program:016X}")).join("0"));
            }
        }
    }
    if candidates.len()!=1 {
        return Err(format!("Cannot safely create save: expected exactly one known account profile, found {}. Create a local save first",candidates.len()));
    }
    let candidate=candidates.into_iter().next().ok_or("No profile")?;
    if candidate.exists() {
        return Err("Missing-save destination already exists; refusing to overwrite it".into());
    }
    Ok(candidate)
}
