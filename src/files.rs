//! Host inbox file transfer. Names are sanitized into a dedicated directory.

use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_FILE: usize = 2 * 1024 * 1024 * 1024usize;
pub const HTTP_PUT_MAX: usize = 8 * 1024 * 1024;
pub const MAX_CHUNK: usize = 24 * 1024;
pub const MAX_NAME: usize = 128;

fn b64_engine() -> &'static base64::engine::GeneralPurpose {
    &base64::engine::general_purpose::STANDARD
}

pub fn decode_b64(s: &str) -> Result<Vec<u8>, String> {
    b64_engine()
        .decode(s.trim())
        .map_err(|_| "invalid base64".into())
}

pub fn encode_b64(data: &[u8]) -> String {
    b64_engine().encode(data)
}

pub fn sanitize_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    if name.starts_with('.') || name == "." {
        return None;
    }
    if name.contains("..") {
        return None;
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control())
    {
        return None;
    }
    Some(name.to_string())
}

pub fn normalize_root(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "inbox" => Some("inbox"),
        "home" => Some("home"),
        "desktop" => Some("desktop"),
        "documents" => Some("documents"),
        "downloads" => Some("downloads"),
        _ => None,
    }
}

pub fn sanitize_rel(rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Some(String::new());
    }
    if rel.len() > 512 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for p in rel.split(['/', '\\']) {
        if p.is_empty() || p == "." {
            continue;
        }
        if p == ".." {
            return None;
        }
        parts.push(sanitize_name(p)?);
        if parts.len() > 8 {
            return None;
        }
    }
    Some(parts.join("/"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

impl FileEntry {
    pub fn file(name: String, size: u64) -> Self {
        Self {
            name,
            size,
            dir: false,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "size": self.size, "dir": self.dir})
    }
}

struct Incoming {
    name: String,
    written: usize,
    size: usize,
    root: String,
    rel: String,
}

pub struct Inbox {
    pub dir: PathBuf,
    home: Option<PathBuf>,
    incoming: Mutex<HashMap<String, Incoming>>,
}

impl Inbox {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            home: None,
            incoming: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone().or_else(|| {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
    }

    pub fn root_dir(&self, root: &str) -> Result<PathBuf, String> {
        let root = normalize_root(root).ok_or_else(|| "unknown folder".to_string())?;
        match root {
            "inbox" => Ok(self.dir.clone()),
            "home" => self.home_dir().ok_or_else(|| "no home directory".into()),
            "desktop" => self
                .home_dir()
                .map(|h| h.join("Desktop"))
                .ok_or_else(|| "no home directory".into()),
            "documents" => self
                .home_dir()
                .map(|h| h.join("Documents"))
                .ok_or_else(|| "no home directory".into()),
            "downloads" => self
                .home_dir()
                .map(|h| h.join("Downloads"))
                .ok_or_else(|| "no home directory".into()),
            _ => Err("unknown folder".into()),
        }
    }

    pub fn join_under(&self, root: &str, rel: &str, name: &str) -> Result<PathBuf, String> {
        let base = self.root_dir(root)?;
        let rel = sanitize_rel(rel).ok_or_else(|| "invalid path".to_string())?;
        if !base.exists() {
            if rel.is_empty() && name.is_empty() {
                return Ok(base);
            }
            return Err("file not found".into());
        }
        let root_can = base.canonicalize().map_err(|e| e.to_string())?;
        let mut path = root_can.clone();
        if !rel.is_empty() {
            path.push(&rel);
        }
        if !name.is_empty() {
            let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
            path.push(name);
        }
        let check = if path.exists() {
            path.canonicalize().map_err(|e| e.to_string())?
        } else if let Some(parent) = path.parent() {
            if !parent.exists() {
                return Err("file not found".into());
            }
            parent
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join(
                    path.file_name()
                        .ok_or_else(|| "invalid file name".to_string())?,
                )
        } else {
            return Err("invalid path".into());
        };
        if !check.starts_with(&root_can) {
            return Err("invalid path".into());
        }
        Ok(check)
    }

    pub fn ensure_dir(&self) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())
    }

    fn dest(&self, name: &str) -> Result<PathBuf, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let path = self.dir.join(&name);
        if path.parent() != Some(self.dir.as_path()) && path.parent() != Some(Path::new("")) {
            return Err("invalid file name".into());
        }
        Ok(path)
    }

    fn part_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.part"))
    }

    fn part_path_at(&self, root: &str, rel: &str, name: &str) -> Result<PathBuf, String> {
        let dest = self.join_under(root, rel, name)?;
        let fname = dest
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file name".to_string())?;
        Ok(dest.with_file_name(format!("{fname}.part")))
    }

    fn ensure_root(&self, root: &str) -> Result<(), String> {
        if normalize_root(root) == Some("inbox") {
            return self.ensure_dir();
        }
        let base = self.root_dir(root)?;
        if !base.exists() {
            fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<FileEntry> {
        self.list_at("inbox", "")
            .map(|(_, _, files)| files)
            .unwrap_or_default()
    }

    pub fn list_at(&self, root: &str, rel: &str) -> Result<(String, String, Vec<FileEntry>), String> {
        let root = normalize_root(root)
            .ok_or_else(|| "unknown folder".to_string())?
            .to_string();
        let rel = sanitize_rel(rel).ok_or_else(|| "invalid path".to_string())?;
        let dir = self.join_under(&root, &rel, "")?;
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => return Ok((root, rel, out)),
        };
        for ent in rd.flatten() {
            if out.len() >= 400 {
                break;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if sanitize_name(&name).is_none() {
                continue;
            }
            if name.ends_with(".part") {
                continue;
            }
            let meta = match ent.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                out.push(FileEntry {
                    name,
                    size: 0,
                    dir: true,
                });
            } else if meta.is_file() {
                out.push(FileEntry::file(name, meta.len()));
            }
        }
        out.sort_by(|a, b| a.dir.cmp(&b.dir).reverse().then(a.name.cmp(&b.name)));
        Ok((root, rel, out))
    }

    pub fn put_bytes(&self, name: &str, data: &[u8]) -> Result<FileEntry, String> {
        self.put_bytes_at("inbox", "", name, data)
    }

    pub fn put_bytes_at(
        &self,
        root: &str,
        rel: &str,
        name: &str,
        data: &[u8],
    ) -> Result<FileEntry, String> {
        if data.len() > MAX_FILE {
            return Err("file too large".into());
        }
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        self.ensure_root(root)?;
        let path = self.join_under(root, rel, &name)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_file_name(format!("{name}.part"));
        fs::write(&tmp, data).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(FileEntry::file(name, data.len() as u64))
    }

    /// Copy a host-side file into the inbox without loading it into RAM.
    pub fn import_path(&self, src: &Path) -> Result<FileEntry, String> {
        if !src.is_file() {
            return Err("not a file".into());
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file name".to_string())?;
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let len = fs::metadata(src).map_err(|e| e.to_string())?.len();
        if len > MAX_FILE as u64 {
            return Err("file too large".into());
        }
        self.ensure_dir()?;
        let dest = self.dest(&name)?;
        if src.canonicalize().ok() == dest.canonicalize().ok() {
            return Ok(FileEntry::file(name, len));
        }
        let tmp = self.part_path(&format!("import-{name}"));
        fs::copy(src, &tmp).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &dest).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(FileEntry::file(name, len))
    }

    pub fn list_json(&self) -> String {
        self.list_at_json("inbox", "")
    }

    pub fn list_at_json(&self, root: &str, rel: &str) -> String {
        match self.list_at(root, rel) {
            Ok((root, path, files)) => json!({
                "type": "file",
                "action": "list",
                "root": root,
                "path": path,
                "files": files.iter().map(|e| e.to_json()).collect::<Vec<_>>()
            })
            .to_string(),
            Err(e) => err_json(&e),
        }
    }

    pub fn mkdir_at(&self, root: &str, rel: &str, name: &str) -> Result<FileEntry, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        self.ensure_root(root)?;
        let path = self.join_under(root, rel, &name)?;
        if path.exists() {
            return Err("already exists".into());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(FileEntry {
            name,
            size: 0,
            dir: true,
        })
    }

    pub fn remove(&self, name: &str) -> Result<FileEntry, String> {
        self.remove_at("inbox", "", name)
    }

    pub fn remove_at(&self, root: &str, rel: &str, name: &str) -> Result<FileEntry, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let path = self.join_under(root, rel, &name)?;
        if path.is_dir() {
            return Err("not a file".into());
        }
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        if normalize_root(root) == Some("inbox") && sanitize_rel(rel).unwrap_or_default().is_empty()
        {
            let _ = fs::remove_file(self.part_path(&name));
            self.incoming.lock().retain(|_, inc| inc.name != name);
        }
        Ok(FileEntry::file(name, meta.len()))
    }

    pub fn get_bytes(&self, name: &str) -> Result<Vec<u8>, String> {
        let path = self.dest(name)?;
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        if meta.len() as usize > MAX_FILE {
            return Err("file too large".into());
        }
        let data = fs::read(&path).map_err(|_| "file not found".to_string())?;
        Ok(data)
    }

    pub fn readable_path(&self, name: &str) -> Result<(PathBuf, u64), String> {
        self.readable_path_at("inbox", "", name)
    }

    pub fn readable_path_at(
        &self,
        root: &str,
        rel: &str,
        name: &str,
    ) -> Result<(PathBuf, u64), String> {
        let path = self.join_under(root, rel, name)?;
        if path.is_dir() {
            return Err("not a file".into());
        }
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        if meta.len() > MAX_FILE as u64 {
            return Err("file too large".into());
        }
        Ok((path, meta.len()))
    }

    /// Send a stored file as blob / blob-begin+chunk+end without holding it in RAM.
    pub fn emit_blob<F: FnMut(String)>(&self, name: &str, emit: F) -> Result<(), String> {
        self.emit_blob_at("inbox", "", name, emit)
    }

    pub fn emit_blob_at<F: FnMut(String)>(
        &self,
        root: &str,
        rel: &str,
        name: &str,
        mut emit: F,
    ) -> Result<(), String> {
        let path = self.join_under(root, rel, name)?;
        if path.is_dir() {
            return Err("not a file".into());
        }
        let shown = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file name".to_string())?;
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        if meta.len() > MAX_FILE as u64 {
            return Err("file too large".into());
        }
        let size = meta.len() as usize;
        if size <= MAX_CHUNK {
            let bytes = fs::read(&path).map_err(|_| "file not found".to_string())?;
            emit(json!({
                "type": "file",
                "action": "blob",
                "name": shown,
                "size": bytes.len(),
                "data": encode_b64(&bytes)
            })
            .to_string());
            return Ok(());
        }
        emit(json!({
            "type": "file",
            "action": "blob-begin",
            "name": shown,
            "size": size
        })
        .to_string());
        let mut f = fs::File::open(&path).map_err(|_| "file not found".to_string())?;
        let mut buf = vec![0u8; MAX_CHUNK];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            emit(json!({
                "type": "file",
                "action": "blob-chunk",
                "name": shown,
                "data": encode_b64(&buf[..n])
            })
            .to_string());
        }
        emit(json!({"type":"file","action":"blob-end","name":shown}).to_string());
        Ok(())
    }

    pub fn begin(&self, id: &str, name: &str, size: usize) -> Result<usize, String> {
        self.begin_at(id, name, size, "inbox", "")
    }

    pub fn begin_at(
        &self,
        id: &str,
        name: &str,
        size: usize,
        root: &str,
        rel: &str,
    ) -> Result<usize, String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let root = normalize_root(root)
            .ok_or_else(|| "unknown folder".to_string())?
            .to_string();
        let rel = sanitize_rel(rel).ok_or_else(|| "invalid path".to_string())?;
        if size == 0 || size > MAX_FILE {
            return Err("invalid file size".into());
        }
        self.ensure_root(&root)?;
        let part = self.part_path_at(&root, &rel, &name)?;
        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut written = 0usize;
        if let Ok(meta) = fs::metadata(&part) {
            let n = meta.len() as usize;
            if n < size && n <= MAX_FILE {
                written = n;
            } else {
                let _ = fs::remove_file(&part);
            }
        }
        let mut g = self.incoming.lock();
        g.retain(|k, inc| {
            k == &id || !(inc.name == name && inc.root == root && inc.rel == rel)
        });
        if g.len() >= 4 && !g.contains_key(&id) {
            return Err("too many transfers".into());
        }
        g.insert(
            id,
            Incoming {
                name,
                written,
                size,
                root,
                rel,
            },
        );
        Ok(written)
    }

    pub fn chunk(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        if data.is_empty() || data.len() > MAX_CHUNK * 2 {
            return Err("invalid chunk".into());
        }
        let mut g = self.incoming.lock();
        let inc = g.get_mut(&id).ok_or_else(|| "unknown transfer".to_string())?;
        let next = inc.written.saturating_add(data.len());
        if next > inc.size || next > MAX_FILE {
            return Err("file too large".into());
        }
        let part = self.part_path_at(&inc.root, &inc.rel, &inc.name)?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| e.to_string())?;
        inc.written = next;
        Ok(())
    }

    pub fn end(&self, id: &str) -> Result<(FileEntry, String, String), String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        let inc = self
            .incoming
            .lock()
            .remove(&id)
            .ok_or_else(|| "unknown transfer".to_string())?;
        let part = self.part_path_at(&inc.root, &inc.rel, &inc.name)?;
        let on_disk = fs::metadata(&part).map(|m| m.len() as usize).unwrap_or(0);
        if inc.written != inc.size || on_disk != inc.size {
            return Err("incomplete file".into());
        }
        let dest = self.join_under(&inc.root, &inc.rel, &inc.name)?;
        fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        Ok((FileEntry::file(inc.name, inc.size as u64), inc.root, inc.rel))
    }

    pub fn handle_message(&self, v: &Value) -> Vec<String> {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let root = v.get("root").and_then(|a| a.as_str()).unwrap_or("inbox");
        let rel = v.get("path").and_then(|a| a.as_str()).unwrap_or("");
        match action {
            "list" => vec![self.list_at_json(root, rel)],
            "put" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                match decode_b64(data).and_then(|b| self.put_bytes_at(root, rel, name, &b)) {
                    Ok(ent) => vec![ok_json_at(&ent, root, rel)],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "begin" => {
                let id = v.get("id").and_then(|n| n.as_str()).unwrap_or("");
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let size = v.get("size").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                match self.begin_at(id, name, size, root, rel) {
                    Ok(offset) => vec![json!({
                        "type":"file",
                        "action":"accept",
                        "id":id,
                        "offset":offset,
                        "size":size
                    })
                    .to_string()],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "chunk" => {
                let id = v.get("id").and_then(|n| n.as_str()).unwrap_or("");
                let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                match decode_b64(data).and_then(|b| self.chunk(id, &b)) {
                    Ok(()) => Vec::new(),
                    Err(e) => vec![err_json(&e)],
                }
            }
            "end" => {
                let id = v.get("id").and_then(|n| n.as_str()).unwrap_or("");
                match self.end(id) {
                    Ok((ent, root, rel)) => vec![ok_json_at(&ent, &root, &rel)],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "get" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let mut out = Vec::new();
                match self.emit_blob_at(root, rel, name, |m| out.push(m)) {
                    Ok(()) => out,
                    Err(e) => vec![err_json(&e)],
                }
            }
            "mkdir" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                match self.mkdir_at(root, rel, name) {
                    Ok(ent) => vec![
                        json!({
                            "type": "file",
                            "action": "mkdir",
                            "name": ent.name,
                            "dir": true,
                            "root": normalize_root(root).unwrap_or("inbox"),
                            "path": sanitize_rel(rel).unwrap_or_default()
                        })
                        .to_string(),
                        self.list_at_json(root, rel),
                    ],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "delete" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                match self.remove_at(root, rel, name) {
                    Ok(ent) => vec![
                        json!({
                            "type": "file",
                            "action": "deleted",
                            "name": ent.name,
                            "size": ent.size,
                            "root": normalize_root(root).unwrap_or("inbox"),
                            "path": sanitize_rel(rel).unwrap_or_default()
                        })
                        .to_string(),
                        self.list_at_json(root, rel),
                    ],
                    Err(e) => vec![err_json(&e)],
                }
            }
            _ => vec![err_json("unknown file action")],
        }
    }
}

/// Unique path in `dir` for `name` (`a.txt`, then `a-1.txt`, …).
pub fn unique_dest(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = sanitize_name(name)?;
    let dest = dir.join(&name);
    if !dest.exists() {
        return Some(dest);
    }
    let path = Path::new(&name);
    let stem = path.file_stem()?.to_str()?.to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    for i in 1..100 {
        let cand = if ext.is_empty() {
            dir.join(format!("{stem}-{i}"))
        } else {
            dir.join(format!("{stem}-{i}.{ext}"))
        };
        if !cand.exists() {
            return Some(cand);
        }
    }
    Some(dest)
}

pub fn copy_into_dir(src: &Path, dir: &Path) -> Result<PathBuf, String> {
    if !src.is_file() {
        return Err("not a file".into());
    }
    if !dir.is_dir() {
        return Err("not a directory".into());
    }
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "invalid file name".to_string())?;
    let dest = unique_dest(dir, name).ok_or_else(|| "invalid file name".to_string())?;
    if src.canonicalize().ok() == dest.canonicalize().ok() {
        return Ok(dest);
    }
    fs::copy(src, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Copy an inbox file onto ~/Desktop. Skips temp-dir sources so tests do not
/// pollute the real Desktop; production inbox sits next to config.json.
fn is_temp_path(src: &Path) -> bool {
    let tmp = std::env::temp_dir();
    if src.starts_with(&tmp) {
        return true;
    }
    let tmp_can = tmp.canonicalize().unwrap_or_else(|_| tmp.clone());
    let src_can = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    src_can.starts_with(&tmp_can)
}

pub fn deliver_to_desktop(src: &Path) -> Option<PathBuf> {
    if !src.is_file() {
        return None;
    }
    if is_temp_path(src) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let desk = PathBuf::from(home).join("Desktop");
    if !desk.is_dir() {
        return None;
    }
    copy_into_dir(src, &desk).ok()
}

fn sanitize_id(id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 64 {
        return None;
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(id.to_string())
}

fn ok_json_at(ent: &FileEntry, root: &str, rel: &str) -> String {
    json!({
        "type": "file",
        "action": "ok",
        "name": ent.name,
        "size": ent.size,
        "root": normalize_root(root).unwrap_or("inbox"),
        "path": sanitize_rel(rel).unwrap_or_default()
    })
    .to_string()
}

fn err_json(msg: &str) -> String {
    json!({"type":"file","action":"error","error":msg}).to_string()
}

pub fn blob_replies(name: &str, bytes: &[u8]) -> Vec<String> {
    if bytes.len() <= MAX_CHUNK {
        return vec![json!({
            "type": "file",
            "action": "blob",
            "name": name,
            "size": bytes.len(),
            "data": encode_b64(bytes)
        })
        .to_string()];
    }
    let mut out = vec![json!({
        "type": "file",
        "action": "blob-begin",
        "name": name,
        "size": bytes.len()
    })
    .to_string()];
    for chunk in bytes.chunks(MAX_CHUNK) {
        out.push(
            json!({
                "type": "file",
                "action": "blob-chunk",
                "name": name,
                "data": encode_b64(chunk)
            })
            .to_string(),
        );
    }
    out.push(json!({"type":"file","action":"blob-end","name":name}).to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_inbox() -> (tempfile::TempDir, Inbox) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(dir.path().join("inbox"));
        (dir, inbox)
    }

    #[test]
    fn sanitize_rejects_traversal_and_hidden() {
        assert_eq!(sanitize_name("notes.txt").as_deref(), Some("notes.txt"));
        assert_eq!(sanitize_name("my photo.png").as_deref(), Some("my photo.png"));
        assert!(sanitize_name("../etc/passwd").is_none());
        assert!(sanitize_name("a/b").is_none());
        assert!(sanitize_name("a\\b").is_none());
        assert!(sanitize_name(".hidden").is_none());
        assert!(sanitize_name("").is_none());
        assert!(sanitize_name("ok\0no").is_none());
    }

    #[test]
    fn put_list_get_roundtrip() {
        let (_dir, inbox) = tmp_inbox();
        let ent = inbox.put_bytes("hello.txt", b"hello world").unwrap();
        assert_eq!(ent.name, "hello.txt");
        assert_eq!(ent.size, 11);
        let list = inbox.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "hello.txt");
        assert_eq!(inbox.get_bytes("hello.txt").unwrap(), b"hello world");
        assert!(inbox.get_bytes("../hello.txt").is_err());
        assert!(inbox.put_bytes("../x", b"no").is_err());
        let gone = inbox.remove("hello.txt").unwrap();
        assert_eq!(gone.name, "hello.txt");
        assert!(inbox.list().is_empty());
        assert!(inbox.remove("hello.txt").is_err());
        assert!(inbox.remove("../hello.txt").is_err());
        let del = serde_json::json!({"action":"delete","name":"nope.txt"});
        let replies = inbox.handle_message(&del);
        assert!(replies.iter().any(|m| m.contains("file not found")));
        inbox.put_bytes("bye.txt", b"x").unwrap();
        let replies = inbox.handle_message(&serde_json::json!({"action":"delete","name":"bye.txt"}));
        assert!(replies.iter().any(|m| m.contains("\"deleted\"")));
        assert!(inbox.list().is_empty());
    }

    #[test]
    fn browse_desktop_lists_dirs_and_rejects_escape() {
        let home = tempfile::tempdir().unwrap();
        let desk = home.path().join("Desktop");
        fs::create_dir(&desk).unwrap();
        fs::write(desk.join("shot.png"), b"png").unwrap();
        fs::create_dir(desk.join("Work")).unwrap();
        fs::write(desk.join("Work").join("a.txt"), b"hi").unwrap();
        let inbox = Inbox::new(home.path().join("inbox")).with_home(home.path().to_path_buf());
        inbox.put_bytes("in.txt", b"x").unwrap();
        let (root, path, files) = inbox.list_at("desktop", "").unwrap();
        assert_eq!(root, "desktop");
        assert_eq!(path, "");
        assert!(files.iter().any(|f| f.name == "shot.png" && !f.dir));
        assert!(files.iter().any(|f| f.name == "Work" && f.dir));
        let files = inbox.list_at("desktop", "Work").unwrap().2;
        assert_eq!(files[0].name, "a.txt");
        assert_eq!(
            fs::read(inbox.join_under("desktop", "Work", "a.txt").unwrap()).unwrap(),
            b"hi"
        );
        assert!(inbox.list_at("desktop", "../inbox").is_err());
        assert!(inbox.join_under("desktop", "", "../in.txt").is_err());
        assert!(inbox.list_at("nope", "").is_err());
        assert_eq!(sanitize_rel("Work/a").as_deref(), Some("Work/a"));
        assert!(sanitize_rel("..").is_none());
        inbox.remove_at("desktop", "Work", "a.txt").unwrap();
        assert!(inbox.list_at("desktop", "Work").unwrap().2.is_empty());
        let listed = inbox.handle_message(&json!({"action":"list","root":"desktop"}));
        assert!(listed[0].contains("shot.png"));
        assert!(listed[0].contains("\"root\":\"desktop\""));
        let home_files = inbox.list_at("home", "").unwrap().2;
        assert!(home_files.iter().any(|f| f.name == "Desktop" && f.dir));
        assert!(inbox.list_at("home", "../").is_err());
        inbox
            .put_bytes_at("home", "", "from-home.txt", b"ok")
            .unwrap();
        assert_eq!(
            fs::read(home.path().join("from-home.txt")).unwrap(),
            b"ok"
        );
        inbox
            .put_bytes_at("desktop", "Work", "drop.txt", b"zz")
            .unwrap();
        assert_eq!(fs::read(desk.join("Work").join("drop.txt")).unwrap(), b"zz");
        assert!(inbox.get_bytes("drop.txt").is_err());
        assert_eq!(inbox.begin_at("d1", "c.bin", 4, "desktop", "Work").unwrap(), 0);
        inbox.chunk("d1", b"abcd").unwrap();
        let (ent, root, rel) = inbox.end("d1").unwrap();
        assert_eq!(ent.name, "c.bin");
        assert_eq!(root, "desktop");
        assert_eq!(rel, "Work");
        assert_eq!(fs::read(desk.join("Work").join("c.bin")).unwrap(), b"abcd");
        let put = inbox.handle_message(&json!({
            "action": "put",
            "root": "desktop",
            "path": "Work",
            "name": "via.json",
            "data": encode_b64(b"ok")
        }));
        assert!(put[0].contains("\"root\":\"desktop\""));
        assert!(put[0].contains("via.json"));
        let dir = inbox.mkdir_at("desktop", "Work", "Sub").unwrap();
        assert!(dir.dir);
        assert!(desk.join("Work").join("Sub").is_dir());
        assert!(inbox.mkdir_at("desktop", "Work", "Sub").is_err());
        assert!(inbox.mkdir_at("desktop", "Work", "../nope").is_err());
        let mk = inbox.handle_message(&json!({
            "action": "mkdir",
            "root": "home",
            "path": "",
            "name": "NewFolder"
        }));
        assert!(mk.iter().any(|m| m.contains("\"mkdir\"") && m.contains("NewFolder")));
        assert!(home.path().join("NewFolder").is_dir());
    }

    #[test]
    fn import_path_copies_host_file_into_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("note.txt");
        fs::write(&src, b"from-finder").unwrap();
        let inbox = Inbox::new(dir.path().join("inbox"));
        let ent = inbox.import_path(&src).unwrap();
        assert_eq!(ent.name, "note.txt");
        assert_eq!(ent.size, 11);
        assert_eq!(inbox.get_bytes("note.txt").unwrap(), b"from-finder");
        assert!(inbox.import_path(dir.path()).is_err());
        assert!(inbox.list_json().contains("note.txt"));
    }

    #[test]
    fn copy_into_dir_uniquifies_and_skips_temp_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"one").unwrap();
        let desk = dir.path().join("Desktop");
        fs::create_dir(&desk).unwrap();
        let p1 = copy_into_dir(&src, &desk).unwrap();
        assert_eq!(p1.file_name().unwrap(), "a.txt");
        assert_eq!(fs::read(&p1).unwrap(), b"one");
        fs::write(&src, b"two").unwrap();
        let p2 = copy_into_dir(&src, &desk).unwrap();
        assert_eq!(p2.file_name().unwrap(), "a-1.txt");
        assert_eq!(fs::read(&p2).unwrap(), b"two");
        assert!(
            deliver_to_desktop(&src).is_none(),
            "temp-dir inbox files must not land on the real Desktop"
        );
        assert!(
            deliver_to_desktop(&src.canonicalize().unwrap()).is_none(),
            "canonical temp paths must also skip Desktop"
        );
    }

    #[test]
    fn chunked_put_assembles() {
        let (_dir, inbox) = tmp_inbox();
        inbox.begin("t1", "chunk.bin", 8).unwrap();
        inbox.chunk("t1", b"abcd").unwrap();
        inbox.chunk("t1", b"efgh").unwrap();
        let (ent, root, rel) = inbox.end("t1").unwrap();
        assert_eq!(ent.size, 8);
        assert_eq!(root, "inbox");
        assert_eq!(rel, "");
        assert_eq!(inbox.get_bytes("chunk.bin").unwrap(), b"abcdefgh");
    }

    #[test]
    fn resume_from_part_file_after_incomplete_end() {
        let (_dir, inbox) = tmp_inbox();
        assert_eq!(inbox.begin("a", "r.bin", 8).unwrap(), 0);
        inbox.chunk("a", b"AAAA").unwrap();
        assert!(inbox.end("a").is_err());
        assert!(
            inbox.list().is_empty(),
            "incomplete transfer must not appear as a finished file"
        );
        assert_eq!(inbox.begin("b", "r.bin", 8).unwrap(), 4);
        inbox.chunk("b", b"BBBB").unwrap();
        let (ent, _, _) = inbox.end("b").unwrap();
        assert_eq!(ent.size, 8);
        assert_eq!(inbox.get_bytes("r.bin").unwrap(), b"AAAABBBB");
    }

    #[test]
    fn handle_begin_chunk_end_json_larger_than_one_chunk() {
        let (_dir, inbox) = tmp_inbox();
        let data = vec![7u8; MAX_CHUNK + 100];
        let begin = inbox.handle_message(&json!({
            "action": "begin",
            "id": "xfer1",
            "name": "big.bin",
            "size": data.len()
        }));
        assert!(begin[0].contains("accept"), "{}", begin[0]);
        assert!(begin[0].contains("\"offset\":0"), "{}", begin[0]);
        for part in data.chunks(MAX_CHUNK) {
            let r = inbox.handle_message(&json!({
                "action": "chunk",
                "id": "xfer1",
                "data": encode_b64(part)
            }));
            assert!(r.is_empty() || r.iter().all(|m| !m.contains("error")), "{r:?}");
        }
        let end = inbox.handle_message(&json!({"action":"end","id":"xfer1"}));
        assert!(end[0].contains("\"ok\""), "{}", end[0]);
        assert_eq!(inbox.get_bytes("big.bin").unwrap(), data);
    }

    #[test]
    fn handle_put_list_get_json() {
        let (_dir, inbox) = tmp_inbox();
        let put = inbox.handle_message(&json!({
            "action": "put",
            "name": "a.txt",
            "data": encode_b64(b"xyz")
        }));
        assert!(put[0].contains("\"ok\""));
        let list = inbox.handle_message(&json!({"action":"list"}));
        assert!(list[0].contains("a.txt"));
        let got = inbox.handle_message(&json!({"action":"get","name":"a.txt"}));
        assert!(got[0].contains("blob"));
        assert!(got[0].contains(&encode_b64(b"xyz")));
    }

    #[test]
    fn oversized_begin_rejected_without_allocating() {
        let (_dir, inbox) = tmp_inbox();
        assert!(inbox.begin("t", "big.bin", MAX_FILE + 1).is_err());
        assert!(inbox.begin("t", "zero.bin", 0).is_err());
    }

    #[test]
    fn chunk_appends_to_part_file() {
        let (_dir, inbox) = tmp_inbox();
        assert_eq!(inbox.begin("t1", "a.bin", 8).unwrap(), 0);
        inbox.chunk("t1", b"abcd").unwrap();
        let part = inbox.dir.join("a.bin.part");
        assert_eq!(fs::metadata(&part).unwrap().len(), 4);
        inbox.chunk("t1", b"efgh").unwrap();
        assert_eq!(fs::metadata(&part).unwrap().len(), 8);
        let (ent, _, _) = inbox.end("t1").unwrap();
        assert_eq!(ent.size, 8);
        assert!(!part.exists());
        assert_eq!(inbox.get_bytes("a.bin").unwrap(), b"abcdefgh");
    }

    #[test]
    fn blob_replies_chunk_large_payloads() {
        let data = vec![7u8; MAX_CHUNK + 10];
        let msgs = blob_replies("n.bin", &data);
        assert!(msgs[0].contains("blob-begin"));
        assert!(msgs.iter().any(|m| m.contains("blob-chunk")));
        assert!(msgs.last().unwrap().contains("blob-end"));
    }

    #[test]
    fn emit_blob_streams_from_disk_matching_blob_replies() {
        let (_dir, inbox) = tmp_inbox();
        inbox.put_bytes("tiny.txt", b"hi").unwrap();
        let mut one = Vec::new();
        inbox.emit_blob("tiny.txt", |m| one.push(m)).unwrap();
        assert_eq!(one, blob_replies("tiny.txt", b"hi"));

        let data = vec![3u8; MAX_CHUNK + 50];
        inbox.put_bytes("s.bin", &data).unwrap();
        let mut msgs = Vec::new();
        inbox.emit_blob("s.bin", |m| msgs.push(m)).unwrap();
        assert_eq!(msgs, blob_replies("s.bin", &data));
        assert!(msgs[0].contains("blob-begin"));
        assert!(msgs.last().unwrap().contains("blob-end"));
    }
}
