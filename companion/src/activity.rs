//! Conservative emulator game activity check.
//! An open game-list window is safe. A booted or uncertain game session is not.
//! Eglantine's boot logs do not provide a universally reliable game-stop marker,
//! so a session with a boot event remains blocked until the emulator exits.
use std::{fs,io::{Read,Seek,SeekFrom},path::PathBuf};

pub fn game_active_or_unknown() -> bool {
    let processes=crate::process_games();
    if processes.is_empty(){return false;}
    // A game supplied directly on the emulator command line is definitely active.
    if processes.iter().any(|(_,name)|name.is_some()){return true;}
    let home=PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let candidates=crate::log_candidates(&home);
    let mut verified_idle=false;
    for log in candidates {
        let Ok(mut file)=fs::File::open(&log) else {continue};
        let Ok(meta)=file.metadata() else {return true};
        // Never conclude that a game is idle from only the tail of a large log.
        if meta.len()>64*1024*1024 {return true;}
        let mut data=Vec::new();
        if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_end(&mut data).is_err(){
            return true;
        }
        let text=String::from_utf8_lossy(&data);
        if text.lines().any(|line| crate::parse_boot_event(line).is_some()){
            return true;
        }
        // A log with no boot event indicates a fresh emulator menu session.
        verified_idle=true;
    }
    !verified_idle
}
