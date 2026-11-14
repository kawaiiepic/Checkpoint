//! Installed-game allowlist from Eglantine's Qt config.
//! Only verified base-game NUP metadata may enable a title for sync.
use crate::{discovery, nup};
use std::{collections::{BTreeMap,BTreeSet},env,fs,path::{Path,PathBuf}};

pub struct Library {
    pub titles:BTreeSet<String>,
    pub save_roots:Vec<PathBuf>,
    pub current_user: Option<usize>,
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
    let mut current_user=None;
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
        }else if section=="System" && key=="current_user"{
            current_user=value(val).parse::<usize>().ok().filter(|i|*i<8);
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
        if let Ok(root)=fs::canonicalize(nand) {
            if root.is_dir(){save_roots.push(root.join("user/save"));}
        }
    }
    Ok(Library{titles,save_roots,current_user})
}
pub fn load()->Result<Library,String>{
    for config in config_candidates(){
        if config.is_file(){return settings(&config);}
    }
    Err("Cannot locate Eglantine qt-config.ini; refusing sync without installed NUP allowlist. Set CHECKPOINT_QT_CONFIG if necessary".into())
}

/// Map the configured user index to its account save-directory ID.
fn selected_account_id(nand:&Path,index:usize)->Result<String,String>{
    if index>=8{return Err("Selected user index outside 0..8".into());}
    let base=nand.join("system/save/8000000000000010/su");
    let profiles=fs::read(base.join("avators/profiles.dat"))
        .map_err(|e|format!("Read profiles.dat: {e}"))?;
    let registry=fs::read(base.join("registry.dat"))
        .map_err(|e|format!("Read registry.dat: {e}"))?;
    const P:[u8;8]=[0xA4,0x87,0x1A,0xCD,0x01,0x60,0x61,0x50];
    const R:[u8;8]=[0x29,0x01,0x85,0xBC,0x01,0x60,0x11,0x60];
    if profiles.get(..8)!=Some(P.as_slice()) || registry.get(..8)!=Some(R.as_slice()) {
        return Err("Unknown Eglantine user profile format".into());
    }
    let offset=0x10+index*0xC8;
    let selected=profiles.get(offset..offset+16).ok_or("Truncated profiles.dat")?;
    let registered=registry.get(0x10+index*16..0x20+index*16).ok_or("Truncated registry.dat")?;
    if selected!=registered || selected.iter().all(|b|*b==0) {
        return Err("Selected profile and account registry disagree or user is empty".into());
    }
    let high=u64::from_le_bytes(selected[8..16].try_into().map_err(|_|"Bad UUID high half")?);
    let low=u64::from_le_bytes(selected[..8].try_into().map_err(|_|"Bad UUID low half")?);
    Ok(format!("{high:016X}{low:016X}"))
}
pub fn missing_account_directory(library:&Library,id:&str)->Result<PathBuf,String>{
    if !discovery::valid_title_id(id){return Err("Invalid title ID".into());}
    let index=library.current_user.ok_or("Missing [System] current_user in qt-config.ini")?;
    if library.save_roots.len()!=1 {
        return Err("Exactly one configured Eglantine NAND root is required".into());
    }
    let root=&library.save_roots[0];
    let nand=root.parent().and_then(Path::parent).ok_or("Invalid NAND root")?;
    let id_key=selected_account_id(nand,index)?;
    let account=root.join("account");
    let profile=account.join(id_key);
    let program=u64::from_str_radix(id,16).map_err(|e|e.to_string())? & !0xff;
    let game=profile.join(format!("{program:016X}"));
    let destination=game.join("0");
    for p in [root,&account,&profile,&game,&destination] {
        if let Ok(meta)=fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err("Unsafe save destination parent/symlink".into());
            }
        }
    }
    if destination.exists(){return Err("Local save directory already exists".into());}
    Ok(destination)
}
