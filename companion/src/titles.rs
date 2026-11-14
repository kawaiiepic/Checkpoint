//! Read-only Switch 2 title lookup; never query Switch 1 title databases.
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::{
    collections::HashMap,
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const GAMES_URL: &str = "https://switchbrew.org/wiki/Switch_2:_Title_list/Games";
const CACHE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn is_switch2_id(id: &str) -> bool {
    id.len() == 16
        && id.starts_with("0400")
        && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn cache_path() -> PathBuf {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("checkpoint/switch2-titles.tsv")
}

fn parse_document(document: &str) -> HashMap<String, String> {
    let html = Html::parse_document(document);
    let row_selector = Selector::parse("tr").unwrap();
    let cell_selector = Selector::parse("td").unwrap();
    let mut result = HashMap::new();
    for row in html.select(&row_selector) {
        let cells: Vec<String> = row.select(&cell_selector)
            .take(2)
            .map(|cell| cell.text().collect::<Vec<_>>().join(" ").trim().to_owned())
            .collect();
        if cells.len() < 2 { continue; }
        let Some(id) = cells[0].split_whitespace()
            .find(|part| is_switch2_id(part)) else { continue };
        let title = cells[1].trim();
        if title.is_empty() { continue; }
        result.entry(id.to_ascii_uppercase()).or_insert_with(|| title.to_owned());
    }
    result
}

fn load_cache(path: &Path) -> io::Result<HashMap<String, String>> {
    let mut titles = HashMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let Some((id, title)) = line.split_once('\t') else { continue };
        if is_switch2_id(id) && !title.is_empty() {
            titles.insert(id.to_ascii_uppercase(), title.to_owned());
        }
    }
    Ok(titles)
}

fn refresh(path: &Path) -> Result<HashMap<String, String>, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Checkpoint/0.1 (+https://github.com/kawaiiepic/Checkpoint)")
        .build().map_err(|e| e.to_string())?;
    let html = client.get(GAMES_URL).send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|e| e.to_string())?;
    let titles = parse_document(&html);
    // Avoid poisoning a working cache with a blocked page or changed layout.
    if titles.len() < 20 {
        return Err(format!("Unexpected Switchbrew response ({} entries)", titles.len()));
    }
    let parent = path.parent().ok_or("Cache has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut sorted: Vec<_> = titles.iter().collect();
    sorted.sort_unstable_by_key(|(id, _)| *id);
    let output = sorted.into_iter()
        .map(|(id, title)| format!("{}\t{}\n", id, title.replace(['\t', '\n', '\r'], " ")))
        .collect::<String>();
    let temp = path.with_extension("tsv.tmp");
    fs::write(&temp, output).map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())?;
    Ok(titles)
}

pub fn load_or_refresh(force: bool) -> HashMap<String, String> {
    let path = cache_path();
    let existing = load_cache(&path).unwrap_or_default();
    let fresh = fs::metadata(&path).and_then(|meta| meta.modified())
        .ok().and_then(|time| SystemTime::now().duration_since(time).ok())
        .is_some_and(|age| age < CACHE_AGE);
    if !force && fresh && !existing.is_empty() {
        println!("[Checkpoint] Switch 2 title database: {} cached names", existing.len());
        return existing;
    }
    match refresh(&path) {
        Ok(titles) => {
            println!("[Checkpoint] Switch 2 title database updated: {} names", titles.len());
            titles
        }
        Err(error) => {
            eprintln!("[Checkpoint] Switch 2 database refresh unavailable: {error}");
            existing
        }
    }
}

pub fn display(id: &str, fallback: &str, names: &HashMap<String, String>) -> String {
    if is_switch2_id(id) {
        let resolved = names.get(&id.to_ascii_uppercase())
            .map(String::as_str).unwrap_or(fallback);
        format!("{resolved} [{id}]")
    } else {
        fallback.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_switch2_ids() {
        assert!(is_switch2_id("04007EF01F8AE000"));
        assert!(!is_switch2_id("0100000000010000"));
        assert!(!is_switch2_id("04007EF01F8AE00X"));
    }
    #[test]
    fn parses_game_rows() {
        let input = "<table><tr><td>04007EF01F8AE000</td><td>Example Game</td></tr>\
                     <tr><td>0100000000010000</td><td>Switch 1</td></tr></table>";
        let entries = parse_document(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get("04007EF01F8AE000").map(String::as_str), Some("Example Game"));
    }
}
