//! Native HTTPS WebDAV transport for immutable Checkpoint backup snapshots.
//! No dependencies on rclone or rsync.
use reqwest::{
    blocking::{Body, Client},
    header::{HeaderValue, IF_NONE_MATCH},
    Method, StatusCode, Url,
};
use std::{
    fs::{self, File},
    io,
    path::Path,
    time::Duration,
};

use crate::sync::Config;

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
    let response=authorize(client.request(method(b"PROPFIND"),url.clone())
        .header("Depth","0"),cfg).send().map_err(|err|format!("WebDAV connection: {err}"))?;
    match response.status() {
        StatusCode::MULTI_STATUS => Ok(()),
        StatusCode::NOT_FOUND => {
            // Only create the final configured collection, never parent account paths.
            // For Nextcloud, /remote.php/dav/files/USER/ must already exist.
            ensure_folder(&client,cfg,url)
        }
        status => Err(error_status("connection test",status))
    }
}

pub fn upload(cfg: &Config, source: &Path, revision: &str) -> Result<(),String> {
    // The configured Checkpoint folder may have been deleted since setup.
    test(cfg)?;
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

// Manual WebDAV restore is deliberately separate from automatic upload.
use std::io::Read as _;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub relative: String,
    pub files: usize,
}

#[derive(Debug)]
struct RemoteItem {
    name: String,
    is_dir: bool,
    size: Option<u64>,
}

fn list_collection(client: &Client, cfg: &Config, base: &Url, parts: &[&str]) -> Result<Vec<RemoteItem>,String> {
    let url=target(base,parts,true)?;
    let response=authorize(client.request(method(b"PROPFIND"),url.clone())
        .header("Depth","1")
        .header("Content-Type","application/xml; charset=utf-8")
        .body(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/></d:prop></d:propfind>"#),cfg)
        .send().map_err(|e|format!("WebDAV listing: {e}"))?;
    if response.status() == StatusCode::NOT_FOUND {return Ok(vec![]);}
    if response.status()!=StatusCode::MULTI_STATUS {return Err(error_status("list",response.status()));}
    let body=response.bytes().map_err(|e|format!("WebDAV listing body: {e}"))?;
    if body.len()>8*1024*1024 {return Err("WebDAV listing too large".into());}
    let xml=roxmltree::Document::parse(std::str::from_utf8(&body).map_err(|e|e.to_string())?)
        .map_err(|e|format!("WebDAV XML: {e}"))?;
    let mut found=Vec::new();
    let parent=url.path().trim_end_matches('/');
    for entry in xml.descendants().filter(|n|n.is_element() && n.tag_name().name()=="response") {
        let Some(href)=entry.children().find(|n|n.is_element()&&n.tag_name().name()=="href")
            .and_then(|n|n.text()) else {continue};
        // URL::join handles both absolute path and absolute HTTP URL responses.
        let resolved=url.join(href).map_err(|e|format!("Invalid WebDAV href: {e}"))?;
        if resolved.scheme()!=url.scheme()||resolved.host_str()!=url.host_str()
           ||resolved.port_or_known_default()!=url.port_or_known_default() {
            return Err("WebDAV listing returned another origin".into());
        }
        let decoded:Vec<String>=resolved.path_segments().ok_or("Invalid WebDAV href path")?
            .filter(|s|!s.is_empty())
            .map(|s|percent_encoding::percent_decode_str(s).decode_utf8().map(|v|v.into_owned()))
            .collect::<Result<_,_>>().map_err(|_|"Invalid filename encoding")?;
        let url_components:Vec<String>=url.path_segments().ok_or("Invalid WebDAV folder")?
            .filter(|s|!s.is_empty())
            .map(|s|percent_encoding::percent_decode_str(s).decode_utf8().map(|v|v.into_owned()))
            .collect::<Result<_,_>>().map_err(|_|"Invalid folder encoding")?;
        if decoded==url_components {continue;}
        if decoded.len()!=url_components.len()+1 || !decoded.starts_with(&url_components) {
            return Err(format!("Unexpected WebDAV listing path under {parent}"));
        }
        let name=decoded.last().cloned().ok_or("Empty remote filename")?;
        if name.is_empty() || name=="." || name==".." || name.contains('/') || name.contains('\\') {
            return Err("Unsafe remote filename".into());
        }
        let ok_prop=entry.descendants().find(|n|n.is_element()&&n.tag_name().name()=="propstat"
            && n.children().any(|child|child.is_element()&&child.tag_name().name()=="status"
                && child.text().unwrap_or("").contains(" 200 ")));
        let Some(props)=ok_prop else {return Err("WebDAV listing missing successful properties".into());};
        let is_dir=props.descendants().any(|n|n.is_element()&&n.tag_name().name()=="collection");
        let size=props.descendants().find(|n|n.is_element()&&n.tag_name().name()=="getcontentlength")
            .and_then(|n|n.text()).and_then(|v|v.parse::<u64>().ok());
        found.push(RemoteItem{name,is_dir,size});
    }
    found.sort_by(|a,b|a.name.cmp(&b.name));
    Ok(found)
}

fn snapshot_parts(reference: &str) -> Result<Vec<&str>,String> {
    let components:Vec<&str>=reference.split('/').collect();
    if components.len()!=7 || components[0]!="switch2"
        || !crate::discovery::valid_title_id(components[1])
        || !["account","device","legacy-account"].contains(&components[2])
        || components[5]!="snapshots"
        || components.iter().any(|p|p.is_empty()||*p=="."||*p==".."||p.contains('\\')||p.contains('%')) {
        return Err("Expected snapshot path switch2/TITLE_ID/LAYOUT/PROFILE/DEVICE/snapshots/REVISION".into());
    }
    Ok(components)
}

fn list_subfolders(client: &Client,cfg: &Config,base: &Url,prefix: Vec<String>,remaining: usize,
    output: &mut Vec<Snapshot>) -> Result<(),String> {
    if output.len()>10000 {return Err("Too many cloud snapshots".into());}
    let parts:Vec<&str>=prefix.iter().map(String::as_str).collect();
    if remaining==0 {
        let contents=list_collection(client,cfg,base,&parts)?;
        if contents.iter().any(|item|item.name==".checkpoint-complete"&&!item.is_dir) {
            output.push(Snapshot{relative:prefix.join("/"),files:contents.iter()
                .filter(|item|!item.is_dir&&item.name!=".checkpoint-complete").count()});
        }
        return Ok(());
    }
    for entry in list_collection(client,cfg,base,&parts)? {
        if !entry.is_dir {continue;}
        let mut child=prefix.clone();
        child.push(entry.name);
        if child.len()==6 && child[5]!="snapshots" {continue;}
        list_subfolders(client,cfg,base,child,remaining-1,output)?;
    }
    Ok(())
}

pub fn list_snapshots(cfg: &Config,title: &str) -> Result<Vec<Snapshot>,String> {
    if !crate::discovery::valid_title_id(title) {return Err("Invalid Switch 2 title ID".into());}
    let client=client()?;
    let base=root(cfg)?;
    let mut snapshots=vec![];
    // Layout, profile, device, snapshots directory, revision = 5 layers.
    // Constrain the snapshots directory explicitly when descending.
    list_subfolders(&client,cfg,&base,vec!["switch2".into(),title.to_ascii_uppercase()],5,&mut snapshots)?;
    snapshots.sort_by(|a,b|b.relative.cmp(&a.relative));
    Ok(snapshots)
}

fn download_dir(client:&Client,cfg:&Config,base:&Url,parts:&[String],local:&Path,
    count:&mut usize,depth:usize) -> Result<(),String> {
    if depth>20 || *count>50000 {return Err("Cloud snapshot exceeds restore limits".into());}
    let names:Vec<&str>=parts.iter().map(String::as_str).collect();
    for entry in list_collection(client,cfg,base,&names)? {
        if entry.name==".checkpoint-complete" {continue;}
        let mut path=parts.to_vec();
        path.push(entry.name.clone());
        let destination=local.join(&entry.name);
        if entry.is_dir {
            fs::create_dir(&destination).map_err(|e|e.to_string())?;
            download_dir(client,cfg,base,&path,&destination,count,depth+1)?;
        } else {
            let names:Vec<&str>=path.iter().map(String::as_str).collect();
            let url=target(base,&names,false)?;
            let mut response=authorize(client.get(url),cfg).send()
                .map_err(|e|format!("WebDAV download: {e}"))?;
            if !response.status().is_success() {return Err(error_status("download",response.status()));}
            let mut output=File::create(&destination).map_err(|e|e.to_string())?;
            let received=io::copy(&mut response,&mut output).map_err(|e|format!("Save download: {e}"))?;
            if let Some(size)=entry.size {
                if size!=received {return Err(format!("Download size mismatch for {}",entry.name));}
            }
            output.sync_all().map_err(|e|e.to_string())?;
            *count+=1;
        }
    }
    Ok(())
}

pub fn download_snapshot(cfg:&Config,reference:&str,staging:&Path) -> Result<usize,String> {
    let parts=snapshot_parts(reference)?;
    let client=client()?;
    let base=root(cfg)?;
    let entries=list_collection(&client,cfg,&base,&parts)?;
    let marker=entries.iter().find(|e|e.name==".checkpoint-complete"&&!e.is_dir)
        .ok_or("Remote snapshot has no completion marker; refusing incomplete backup")?;
    if marker.size.is_some_and(|size|size>1024) {return Err("Invalid snapshot marker".into());}
    let mut marker_parts=parts.clone();
    marker_parts.push(".checkpoint-complete");
    let mut marker_response=authorize(client.get(target(&base,&marker_parts,false)?),cfg)
        .send().map_err(|e|e.to_string())?;
    if !marker_response.status().is_success() {
        return Err(error_status("read snapshot marker",marker_response.status()));
    }
    let mut marker_data=Vec::new();
    io::Read::take(&mut marker_response,1025).read_to_end(&mut marker_data)
        .map_err(|e|e.to_string())?;
    let expected=std::str::from_utf8(&marker_data).map_err(|e|e.to_string())?
        .trim().strip_prefix("files=").ok_or("Invalid snapshot completion marker")?
        .parse::<usize>().map_err(|_|"Invalid snapshot file count")?;
    if expected==0 || expected>50000 {return Err("Snapshot file count outside limits".into());}
    let mut count=0;
    let names=parts.iter().map(|s|s.to_string()).collect::<Vec<_>>();
    download_dir(&client,cfg,&base,&names,staging,&mut count,0)?;
    if count!=expected {
        return Err(format!("Snapshot incomplete: expected {expected} files, downloaded {count}"));
    }
    Ok(count)
}


/// Retain one completed remote snapshot for this exact game/profile/device.
/// Never delete anything unless the snapshot being kept has a completion marker.
pub fn keep_one_snapshot(cfg: &Config, device_folder: &str, preferred: Option<&str>) -> Result<usize,String> {
    let client=client()?;
    let base=root(cfg)?;
    let folder:Vec<&str>=device_folder.split('/').collect();
    if folder.len()!=5 || folder[0]!="switch2"
        || !crate::discovery::valid_title_id(folder[1])
        || !["account","device","legacy-account"].contains(&folder[2])
        || folder.iter().any(|item|item.is_empty()||*item=="."||*item==".."
            || item.contains('%')||item.contains('\\')) {
        return Err("Invalid backup directory for retention".into());
    }
    let mut snapshots=folder.clone();
    snapshots.push("snapshots");
    let items=list_collection(&client,cfg,&base,&snapshots)?;
    let mut candidates:Vec<String>=items.into_iter().filter(|item|item.is_dir)
        .map(|item|item.name).collect();
    candidates.sort();
    if candidates.is_empty(){return Ok(0);}
    let mut complete=Vec::new();
    for name in &candidates {
        let mut parts=snapshots.clone();
        parts.push(name);
        let entries=list_collection(&client,cfg,&base,&parts)?;
        if entries.iter().any(|entry|entry.name==".checkpoint-complete" && !entry.is_dir) {
            complete.push(name.clone());
        }
    }
    let keep=match preferred {
        Some(revision) => {
            if !complete.iter().any(|item|item==revision) {
                return Err("New snapshot is not complete; retaining all previous backups".into());
            }
            revision.to_owned()
        }
        None => match complete.last() {
            Some(name)=>name.clone(),
            None=>return Err("No completed snapshot found; refusing retention cleanup".into())
        }
    };
    let mut removed=0;
    for name in candidates {
        if name==keep {continue;}
        let mut parts=snapshots.clone();
        parts.push(&name);
        let url=target(&base,&parts,true)?;
        let response=authorize(client.request(method(b"DELETE"),url),cfg)
            .send().map_err(|e|format!("WebDAV cleanup: {e}"))?;
        if !response.status().is_success() && response.status()!=StatusCode::NOT_FOUND {
            return Err(error_status("remove older snapshot",response.status()));
        }
        removed+=1;
    }
    Ok(removed)
}


// Canonical cross-device sync protocol: one pointer, one active generation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CloudFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CloudCurrent {
    pub version: u32,
    pub title: String,
    pub generation: String,
    pub digest: String,
    pub writer: String,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub save_layout: Option<String>,
    pub files: Vec<CloudFile>,
}
pub struct CurrentResponse {
    pub manifest: CloudCurrent,
    pub etag: String,
}
fn current_path(title: &str) -> [&str; 3] { ["switch2", title, "current.json"] }

pub fn get_current(cfg: &Config, title: &str) -> Result<Option<CurrentResponse>, String> {
    if !crate::discovery::valid_title_id(title) {return Err("Invalid Switch 2 title".into());}
    let client=client()?;
    let base=root(cfg)?;
    let response=authorize(client.get(target(&base,&current_path(title),false)?)
        .header("Cache-Control","no-cache"),cfg).send()
        .map_err(|e|format!("WebDAV current save: {e}"))?;
    if response.status()==StatusCode::NOT_FOUND {return Ok(None);}
    if !response.status().is_success() {return Err(error_status("read current save",response.status()));}
    let etag=response.headers().get(reqwest::header::ETAG)
        .ok_or("Nextcloud returned no ETag; safe concurrent sync unavailable")?
        .to_str().map_err(|e|e.to_string())?.to_owned();
    let bytes=response.bytes().map_err(|e|e.to_string())?;
    if bytes.len()>8*1024*1024 {return Err("Cloud manifest too large".into());}
    let manifest:CloudCurrent=serde_json::from_slice(&bytes).map_err(|e|format!("Invalid cloud manifest: {e}"))?;
    validate_current(&manifest,title)?;
    Ok(Some(CurrentResponse{manifest,etag}))
}
fn validate_current(current: &CloudCurrent,title: &str) -> Result<(),String> {
    if current.version!=1 || current.title!=title || current.files.is_empty()
       || current.files.len()>50000 || current.generation.is_empty()
       || !current.generation.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-')
       || current.digest.len()!=64 || !current.digest.bytes().all(|c|c.is_ascii_hexdigit()) {
        return Err("Invalid cloud save manifest".into());
    }
    let mut paths=std::collections::BTreeSet::new();
    for f in &current.files {
        if f.sha256.len()!=64 || !f.sha256.bytes().all(|c|c.is_ascii_hexdigit())
          || f.path.is_empty() || !paths.insert(f.path.clone())
          || f.path.split('/').any(|p|p.is_empty()||p=="."||p==".."||p.contains('\\')||p.contains('%'))
          || f.path.starts_with('/') {
            return Err("Invalid cloud file path or hash".into());
        }
    }
    Ok(())
}
pub fn upload_generation(cfg:&Config, source:&Path, current:&CloudCurrent) -> Result<(),String> {
    validate_current(current,&current.title)?;
    // WebDAV MKCOL requires the parent collection to exist (otherwise HTTP 409).
    // In particular, a user may have deleted the configured /Checkpoint folder.
    test(cfg)?;
    let client=client()?;
    let base=root(cfg)?;
    let prefix=["switch2",current.title.as_str(),"data",current.generation.as_str()];
    for count in 1..=prefix.len() {
        ensure_folder(&client,cfg,target(&base,&prefix[..count],true)?)?;
    }
    let mut created=std::collections::BTreeSet::new();
    for entry in &current.files {
        let names:Vec<&str>=entry.path.split('/').collect();
        for n in 1..names.len() {
            let mut segments=prefix.to_vec();segments.extend_from_slice(&names[..n]);
            if created.insert(segments.join("/")) {
                ensure_folder(&client,cfg,target(&base,&segments,true)?)?;
            }
        }
        let mut path=prefix.to_vec();path.extend_from_slice(&names);
        put_file(&client,cfg,target(&base,&path,false)?,&source.join(&entry.path))?;
    }
    Ok(())
}
pub fn publish_current(cfg:&Config,manifest:&CloudCurrent,previous:Option<&str>) -> Result<(),String> {
    validate_current(manifest,&manifest.title)?;
    test(cfg)?;
    let client=client()?;
    let root=root(cfg)?;
    let parent=["switch2",manifest.title.as_str()];
    ensure_folder(&client,cfg,target(&root,&parent,true)?)?;
    let mut request=authorize(client.put(target(&root,&current_path(&manifest.title),false)?),cfg);
    request=if let Some(etag)=previous {
        request.header(reqwest::header::IF_MATCH,etag)
    }else{request.header(IF_NONE_MATCH,"*")};
    let payload=serde_json::to_vec(manifest).map_err(|e|e.to_string())?;
    let response=request.header(reqwest::header::CONTENT_TYPE,"application/json")
        .body(payload).send().map_err(|e|format!("Publish cloud current: {e}"))?;
    if response.status()==StatusCode::PRECONDITION_FAILED ||
       response.status()==StatusCode::CONFLICT {
        return Err("CONFLICT: Cloud save changed on another device. No data overwritten".into());
    }
    if !response.status().is_success() {return Err(error_status("publish current",response.status()));}
    Ok(())
}
pub fn delete_generation(cfg:&Config,title:&str,generation:&str) -> Result<(),String> {
    if !crate::discovery::valid_title_id(title) || generation.is_empty()
        || !generation.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-') {
        return Err("Unsafe cloud generation path".into());
    }
    // Never delete the active cloud version.
    if get_current(cfg,title)?.is_some_and(|c|c.manifest.generation==generation) {
        return Err("Refusing to delete current cloud save".into());
    }
    let client=client()?;
    let base=root(cfg)?;
    let response=authorize(client.request(method(b"DELETE"),
        target(&base,&["switch2",title,"data",generation],true)?),cfg)
        .send().map_err(|e|format!("WebDAV cleanup: {e}"))?;
    if response.status().is_success() || response.status()==StatusCode::NOT_FOUND {Ok(())}
    else {Err(error_status("delete old cloud generation",response.status()))}
}
pub fn download_current(cfg:&Config, current:&CloudCurrent, directory:&Path) -> Result<(),String> {
    validate_current(current,&current.title)?;
    let client=client()?;
    let base=root(cfg)?;
    for entry in &current.files {
        let pieces:Vec<&str>=entry.path.split('/').collect();
        let mut parts=vec!["switch2",current.title.as_str(),"data",current.generation.as_str()];
        parts.extend_from_slice(&pieces);
        let destination=directory.join(&entry.path);
        fs::create_dir_all(destination.parent().ok_or("Invalid destination path")?).map_err(|e|e.to_string())?;
        let mut response=authorize(client.get(target(&base,&parts,false)?),cfg)
            .send().map_err(|e|format!("Cloud file download: {e}"))?;
        if !response.status().is_success() {return Err(error_status("download current file",response.status()));}
        let mut file=File::create(&destination).map_err(|e|e.to_string())?;
        let mut hasher=sha2::Sha256::new();
        use sha2::Digest;
        let mut total=0u64;
        let mut buffer=[0u8;64*1024];
        loop {
            let n=std::io::Read::read(&mut response,&mut buffer).map_err(|e|e.to_string())?;
            if n==0 {break;}
            total=total.saturating_add(n as u64);
            if total>entry.size {return Err(format!("Cloud file larger than expected: {}",entry.path));}
            std::io::Write::write_all(&mut file,&buffer[..n]).map_err(|e|e.to_string())?;
            hasher.update(&buffer[..n]);
        }
        if total!=entry.size || format!("{:x}",hasher.finalize())!=entry.sha256 {
            return Err(format!("Cloud file checksum mismatch: {}",entry.path));
        }
        file.sync_all().map_err(|e|e.to_string())?;
    }
    Ok(())
}
