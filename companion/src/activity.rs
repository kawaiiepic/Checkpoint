//! Conservative, process-first Eglantine activity guard.
//! /proc identifies the emulator and ROM launch arguments directly.
//! A GUI-launched game has no reliable stop signal through /proc alone:
//! never use an old BootGame log event or missing open file handle as proof
//! that a game has ended.
pub fn game_active_or_unknown()->bool {
    let processes=crate::process_games();
    if processes.is_empty(){return false;}
    // ROM launch arguments confirm an active/possibly active game session.
    if processes.iter().any(|(_,name)|name.is_some()){return true;}
    // A running GUI instance could be at its library OR running a game.
    // Without verified game-stop IPC, refuse to sync until the process exits.
    true
}
