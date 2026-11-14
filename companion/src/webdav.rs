//! Native HTTPS WebDAV transport for immutable Checkpoint backup snapshots.
//! No dependencies on rclone or rsync.
use reqwest::{
    blocking::{Body, Client},
    header::{HeaderValue, IF_NONE_MATCH},
    Method, StatusCode, Url,
};
use std::{
    fs::{self, File},
    path::Path,
    time::Duration,
};

use super::Config;

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        // Never forward credentials through an HTTP redirect.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Checkpoint/0.1 (+https://github.com/kawaiiepic/Checkpoint)")
        .build()
        .map_err(|err| format!("WebDAV client: {err}"))
}

pub fn validate_url(url: &str) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|err| format!("Invalid WebDAV URL: {err}"))?;
    if parsed.scheme() != "https" || parsed.host_str().is_none()
        || !parsed.username().is_empty() || parsed.password().is_some()
        || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("WebDAV URL must be HTTPS with a host, without embedded credentials, query or fragment".into());
    }
    Ok(())
}

fn root(cfg: &Config) -> Result<Url, String> {
    validate_url(&cfg.destination)?;
    let mut url = Url::parse(&cfg.destination).map_err(|err|err.to_string())?;
    // The configured URL is a *directory*, even if it lacks a trailing slash.
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn target(root: &Url, segments: &[&str], directory: bool) -> Result<Url, String> {
    let mut url=root.clone();
    {
        let mut target=url.path_segments_mut().map_err(|_|"Invalid WebDAV directory URL")?;
        target.pop_if_empty();
        for segment in segments {
            if segment.is_empty() || *segment == "." || *segment == ".."
                || segment.contains('/') || segment.contains('\\') {
                return Err("Unsafe WebDAV path component".into());
            }
            target.push(segment);
        }
        if directory { target.push(""); }
    }
    Ok(url)
}

fn method(name: &[u8]) -> Method {
    Method::from_bytes(name).expect("Known WebDAV method")
}
fn error_status(action: &str, code: StatusCode) -> String {
    format!("WebDAV {action} failed: HTTP {code}")
}
fn authorize(request: reqwest::blocking::RequestBuilder, cfg: &Config) -> reqwest::blocking::RequestBuilder {
    request.basic_auth(cfg.username.as_deref().unwrap_or(""), cfg.password.as_deref())
}

fn ensure_folder(client: &Client, cfg: &Config, url: Url) -> Result<(), String> {
    let response=authorize(client.request(method(b"MKCOL"),url),cfg)
        .send().map_err(|err|format!("WebDAV MKCOL: {err}"))?;
    if response.status().is_success() || response.status() == StatusCode::METHOD_NOT_ALLOWED {
        // 405 generally means the collection already exists. Require a successful
        // PROPFIND in this case so a non-directory resource cannot masquerade as one.
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            let url=response.url().clone();
            let check=authorize(client.request(method(b"PROPFIND"),url)
                .header("Depth","0"),cfg).send().map_err(|err|format!("WebDAV folder check: {err}"))?;
            if check.status() != StatusCode::MULTI_STATUS {
                return Err(error_status("folder check",check.status()));
            }
        }
        Ok(())
    } else {
        Err(error_status("create directory",response.status()))
    }
}

fn walk(source: &Path, relative: &Path, found: &mut Vec<(Vec<String>,std::path::PathBuf)>)
    -> Result<(), String> {
    let directory = source.join(relative);
    for item in fs::read_dir(&directory).map_err(|err|format!("Cannot read {}: {err}",directory.display()))? {
        let item=item.map_err(|err|err.to_string())?;
        let metadata=fs::symlink_metadata(item.path()).map_err(|err|err.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Refusing to copy symlink {}",item.path().display()));
        }
        let name=item.file_name().into_string()
            .map_err(|_|"WebDAV backup requires UTF-8 save filenames".to_owned())?;
        let child=relative.join(&name);
        if metadata.is_dir() {
            walk(source,&child,found)?;
        } else if metadata.is_file() {
            let parts=child.iter().map(|x|x.to_string_lossy().into_owned()).collect();
            found.push((parts,item.path()));
        } else {
            return Err(format!("Unsupported save file type: {}",item.path().display()));
        }
    }
    Ok(())
}

fn put_file(client: &Client, cfg: &Config, url: Url, path: &Path) -> Result<(),String> {
    let file=File::open(path).map_err(|err|format!("Cannot read {}: {err}",path.display()))?;
    let size=file.metadata().map_err(|err|err.to_string())?.len();
    let body=Body::sized(file,size);
    let response=authorize(client.put(url).header(IF_NONE_MATCH,HeaderValue::from_static("*"))
        .body(body),cfg).send().map_err(|err|format!("WebDAV PUT: {err}"))?;
    if response.status().is_success() {Ok(())}
    else {Err(error_status("upload",response.status()))}
}

pub fn test(cfg: &Config) -> Result<(),String> {
    let client=client()?;
    let url=root(cfg)?;
    let response=authorize(client.request(method(b"PROPFIND"),url)
        .header("Depth","0"),cfg).send().map_err(|err|format!("WebDAV connection: {err}"))?;
    if response.status() == StatusCode::MULTI_STATUS {Ok(())}
    else {Err(error_status("connection test",response.status()))}
}

pub fn upload(cfg: &Config, source: &Path, revision: &str) -> Result<(),String> {
    let client=client()?;
    let base=root(cfg)?;
    let segments:Vec<&str>=revision.split('/').collect();
    if segments.is_empty() {return Err("Empty snapshot path".into());}
    // Validate and create all intermediate collections.
    for count in 1..=segments.len() {
        ensure_folder(&client,cfg,target(&base,&segments[..count],true)?)?;
    }
    let mut found=Vec::new();
    walk(source,Path::new(""),&mut found)?;
    if found.is_empty() {return Err("No regular files found to back up".into());}
    found.sort_by(|a,b|a.0.cmp(&b.0));
    let mut created=std::collections::BTreeSet::new();
    for (parts,path) in &found {
        for count in 1..parts.len() {
            let mut combined=segments.clone();
            combined.extend(parts[..count].iter().map(String::as_str));
            if created.insert(combined.join("/")) {
                ensure_folder(&client,cfg,target(&base,&combined,true)?)?;
            }
        }
        let mut combined=segments.clone();
        combined.extend(parts.iter().map(String::as_str));
        put_file(&client,cfg,target(&base,&combined,false)?,path)?;
    }
    // A marker is written last: an interrupted snapshot lacks this file.
    let marker=target(&base,&[segments.clone(),vec![".checkpoint-complete"]].concat(),false)?;
    let response=authorize(client.put(marker).header(IF_NONE_MATCH,"*")
        .body(format!("files={}\n",found.len())),cfg)
        .send().map_err(|err|format!("WebDAV commit marker: {err}"))?;
    if !response.status().is_success() {
        return Err(error_status("commit marker",response.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn requires_https_and_no_password_in_url() {
        assert!(validate_url("https://cloud.example/remote.php/dav/files/user").is_ok());
        assert!(validate_url("http://cloud.example/").is_err());
        assert!(validate_url("https://user:secret@cloud.example/").is_err());
    }
    #[test] fn path_encodes_spaces() {
        let url=Url::parse("https://cloud.example/remote.php/dav/files/user/Checkpoint/").unwrap();
        let output=target(&url,&["switch2","save with spaces"],true).unwrap();
        assert!(output.as_str().contains("save%20with%20spaces"));
    }
}
