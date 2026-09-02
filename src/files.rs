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
pub const RM_MAX: usize = 2000;
pub const SEL_MAX: usize = 100;

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

    fn dir_entry_count(path: &Path) -> Result<usize, String> {
        let mut n = 0usize;
        fn walk(path: &Path, n: &mut usize) -> Result<(), String> {
            if *n >= RM_MAX {
                return Err("folder too large".into());
            }
            let rd = fs::read_dir(path).map_err(|e| e.to_string())?;
            for ent in rd {
                *n += 1;
                if *n >= RM_MAX {
                    return Err("folder too large".into());
                }
                let p = ent.map_err(|e| e.to_string())?.path();
                if p.is_dir() && !p.is_symlink() {
                    walk(&p, n)?;
                }
            }
            Ok(())
        }
        walk(path, &mut n)?;
        Ok(n)
    }

    pub fn remove_at(&self, root: &str, rel: &str, name: &str) -> Result<FileEntry, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let path = self.join_under(root, rel, &name)?;
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        if meta.is_dir() {
            let _ = Self::dir_entry_count(&path)?;
            fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            return Ok(FileEntry {
                name,
                size: 0,
                dir: true,
            });
        }
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        if normalize_root(root) == Some("inbox") && sanitize_rel(rel).unwrap_or_default().is_empty()
        {
            let _ = fs::remove_file(self.part_path(&name));
            self.incoming.lock().retain(|_, inc| inc.name != name);
        }
        Ok(FileEntry::file(name, meta.len()))
    }

    pub fn rename_at(
        &self,
        root: &str,
        rel: &str,
        from: &str,
        to: &str,
    ) -> Result<FileEntry, String> {
        let from = sanitize_name(from).ok_or_else(|| "invalid file name".to_string())?;
        let to = sanitize_name(to).ok_or_else(|| "invalid file name".to_string())?;
        if from == to {
            let path = self.join_under(root, rel, &from)?;
            let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
            return Ok(FileEntry {
                name: to,
                size: if meta.is_dir() { 0 } else { meta.len() },
                dir: meta.is_dir(),
            });
        }
        let src = self.join_under(root, rel, &from)?;
        let dest = self.join_under(root, rel, &to)?;
        if !src.exists() {
            return Err("file not found".into());
        }
        if dest.exists() {
            return Err("already exists".into());
        }
        let meta = fs::metadata(&src).map_err(|_| "file not found".to_string())?;
        fs::rename(&src, &dest).map_err(|e| e.to_string())?;
        if normalize_root(root) == Some("inbox") && sanitize_rel(rel).unwrap_or_default().is_empty()
        {
            let _ = fs::rename(self.part_path(&from), self.part_path(&to));
            self.incoming.lock().iter_mut().for_each(|(_, inc)| {
                if inc.name == from {
                    inc.name = to.clone();
                }
            });
        }
        Ok(FileEntry {
            name: to,
            size: if meta.is_dir() { 0 } else { meta.len() },
            dir: meta.is_dir(),
        })
    }

    pub fn copy_at(
        &self,
        from_root: &str,
        from_rel: &str,
        name: &str,
        to_root: &str,
        to_rel: &str,
    ) -> Result<FileEntry, String> {
        self.transfer_at(from_root, from_rel, name, to_root, to_rel, false)
    }

    pub fn move_at(
        &self,
        from_root: &str,
        from_rel: &str,
        name: &str,
        to_root: &str,
        to_rel: &str,
    ) -> Result<FileEntry, String> {
        self.transfer_at(from_root, from_rel, name, to_root, to_rel, true)
    }

    pub fn transfer_names_at(
        &self,
        from_root: &str,
        from_rel: &str,
        names: &[String],
        to_root: &str,
        to_rel: &str,
        moving: bool,
    ) -> Result<FileEntry, String> {
        let names = unique_names(names);
        if names.is_empty() {
            return Err("missing name".into());
        }
        let mut last = None;
        for name in &names {
            last = Some(self.transfer_at(from_root, from_rel, name, to_root, to_rel, moving)?);
        }
        last.ok_or_else(|| "missing name".into())
    }

    pub fn remove_names_at(
        &self,
        root: &str,
        rel: &str,
        names: &[String],
    ) -> Result<FileEntry, String> {
        let names = unique_names(names);
        if names.is_empty() {
            return Err("missing name".into());
        }
        let mut last = None;
        for name in &names {
            last = Some(self.remove_at(root, rel, name)?);
        }
        last.ok_or_else(|| "missing name".into())
    }

    fn transfer_at(
        &self,
        from_root: &str,
        from_rel: &str,
        name: &str,
        to_root: &str,
        to_rel: &str,
        moving: bool,
    ) -> Result<FileEntry, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let src = self.join_under(from_root, from_rel, &name)?;
        if !src.exists() {
            return Err("file not found".into());
        }
        self.ensure_root(to_root)?;
        let dest_dir = self.join_under(to_root, to_rel, "")?;
        if !dest_dir.exists() {
            fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
        }
        if !dest_dir.is_dir() {
            return Err("not a directory".into());
        }
        let dest = if moving {
            let p = dest_dir.join(&name);
            if p.exists() {
                return Err("already exists".into());
            }
            p
        } else {
            unique_dest(&dest_dir, &name).ok_or_else(|| "invalid file name".to_string())?
        };
        let src_can = src.canonicalize().map_err(|e| e.to_string())?;
        if dest == src || dest.starts_with(&src_can) {
            return Err("invalid path".into());
        }
        let meta = fs::metadata(&src).map_err(|_| "file not found".to_string())?;
        if meta.is_dir() {
            let _ = Self::dir_entry_count(&src)?;
        }
        if moving {
            match fs::rename(&src, &dest) {
                Ok(()) => {}
                Err(_) => {
                    copy_tree(&src, &dest)?;
                    if src.is_dir() {
                        fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
                    } else {
                        fs::remove_file(&src).map_err(|e| e.to_string())?;
                    }
                }
            }
        } else {
            copy_tree(&src, &dest)?;
        }
        let out_name = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&name)
            .to_string();
        Ok(FileEntry {
            name: out_name,
            size: if meta.is_dir() { 0 } else { meta.len() },
            dir: meta.is_dir(),
        })
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

    pub fn folder_zip_at(&self, root: &str, rel: &str, name: &str) -> Result<FolderZip, String> {
        let path = self.join_under(root, rel, name)?;
        if !path.is_dir() {
            return Err("not a folder".into());
        }
        FolderZip::from_dir(&path)
    }

    pub fn folder_zip_names_at(
        &self,
        root: &str,
        rel: &str,
        names: &[String],
    ) -> Result<FolderZip, String> {
        let names = unique_names(names);
        if names.is_empty() {
            return Err("missing name".into());
        }
        if names.len() == 1 {
            return self.folder_zip_at(root, rel, &names[0]);
        }
        let mut entries = Vec::new();
        for name in &names {
            let path = self.join_under(root, rel, name)?;
            if !path.exists() {
                return Err("file not found".into());
            }
            entries.push((name.clone(), path));
        }
        FolderZip::from_named_paths("files.zip".into(), &entries)
    }

    pub fn emit_blob_names_at<F: FnMut(String)>(
        &self,
        root: &str,
        rel: &str,
        names: &[String],
        emit: F,
    ) -> Result<(), String> {
        let names = unique_names(names);
        if names.is_empty() {
            return Err("missing name".into());
        }
        if names.len() == 1 {
            return self.emit_blob_at(root, rel, &names[0], emit);
        }
        let zip = self.folder_zip_names_at(root, rel, &names)?;
        emit_zip_blob(&zip, emit)
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
            let zip = FolderZip::from_dir(&path)?;
            return emit_zip_blob(&zip, emit);
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
                let names = names_from_value(v);
                let mut out = Vec::new();
                match self.emit_blob_names_at(root, rel, &names, |m| out.push(m)) {
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
            "copy" | "move" => {
                let names = names_from_value(v);
                let to_root = v.get("toRoot").and_then(|n| n.as_str()).unwrap_or(root);
                let to_rel = v.get("toPath").and_then(|n| n.as_str()).unwrap_or("");
                let moving = action == "move";
                match self.transfer_names_at(root, rel, &names, to_root, to_rel, moving) {
                    Ok(ent) => vec![
                        json!({
                            "type": "file",
                            "action": if moving { "moved" } else { "copied" },
                            "name": ent.name,
                            "names": names,
                            "dir": ent.dir,
                            "root": normalize_root(to_root).unwrap_or("inbox"),
                            "path": sanitize_rel(to_rel).unwrap_or_default()
                        })
                        .to_string(),
                        self.list_at_json(to_root, to_rel),
                    ],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "rename" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let to = v.get("to").and_then(|n| n.as_str()).unwrap_or("");
                match self.rename_at(root, rel, name, to) {
                    Ok(ent) => vec![
                        json!({
                            "type": "file",
                            "action": "renamed",
                            "name": ent.name,
                            "from": name,
                            "dir": ent.dir,
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
                let names = names_from_value(v);
                match self.remove_names_at(root, rel, &names) {
                    Ok(ent) => vec![
                        json!({
                            "type": "file",
                            "action": "deleted",
                            "name": ent.name,
                            "names": names,
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

pub fn folder_zip_name(name: &str) -> String {
    format!("{name}.zip")
}

pub fn names_from_value(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = v.get("names").and_then(|a| a.as_array()) {
        for n in arr {
            if let Some(s) = n.as_str().and_then(sanitize_name) {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            if out.len() >= SEL_MAX {
                break;
            }
        }
    }
    if out.is_empty() {
        if let Some(s) = v.get("name").and_then(|n| n.as_str()).and_then(sanitize_name) {
            out.push(s);
        }
    }
    out
}

fn unique_names(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for n in names {
        if let Some(s) = sanitize_name(n) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        if out.len() >= SEL_MAX {
            break;
        }
    }
    out
}

struct ZipMember {
    name: String,
    src: Option<PathBuf>,
    crc: u32,
    size: u32,
}

pub struct FolderZip {
    pub download_name: String,
    pub size: u64,
    members: Vec<ZipMember>,
}

impl FolderZip {
    fn from_members(download_name: String, members: Vec<ZipMember>) -> Result<Self, String> {
        if members.is_empty() {
            return Err("missing name".into());
        }
        let size = zip_store_len(&members);
        if size > MAX_FILE as u64 {
            return Err("file too large".into());
        }
        Ok(Self {
            download_name,
            size,
            members,
        })
    }

    fn from_dir(dir: &Path) -> Result<Self, String> {
        let shown = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file name".to_string())?;
        let shown = sanitize_name(shown).ok_or_else(|| "invalid file name".to_string())?;
        let mut members = Vec::new();
        let mut n = 0usize;
        let mut bytes = 0u64;
        collect_zip_tree(dir, &shown, &mut members, &mut n, &mut bytes)?;
        Self::from_members(folder_zip_name(&shown), members)
    }

    fn from_named_paths(download_name: String, entries: &[(String, PathBuf)]) -> Result<Self, String> {
        let mut members = Vec::new();
        let mut n = 0usize;
        let mut bytes = 0u64;
        for (name, path) in entries {
            let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
            let meta = fs::symlink_metadata(path).map_err(|_| "file not found".to_string())?;
            if meta.file_type().is_symlink() {
                return Err("invalid path".into());
            }
            if meta.is_dir() {
                collect_zip_tree(path, &name, &mut members, &mut n, &mut bytes)?;
            } else if meta.is_file() {
                push_zip_file(&name, path, &meta, &mut members, &mut bytes)?;
            } else {
                return Err("not a file".into());
            }
        }
        Self::from_members(download_name, members)
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), String> {
        let mut offset: u32 = 0;
        let mut offsets = Vec::with_capacity(self.members.len());
        for m in &self.members {
            offsets.push(offset);
            write_local(w, m)?;
            offset = offset
                .checked_add(30 + m.name.len() as u32 + m.size)
                .ok_or_else(|| "file too large".to_string())?;
        }
        let cd_start = offset;
        for (m, off) in self.members.iter().zip(offsets.into_iter()) {
            write_central(w, m, off)?;
            offset = offset
                .checked_add(46 + m.name.len() as u32)
                .ok_or_else(|| "file too large".to_string())?;
        }
        let cd_size = offset.saturating_sub(cd_start);
        write_eocd(w, self.members.len() as u16, cd_size, cd_start)?;
        w.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn zip_store_len(members: &[ZipMember]) -> u64 {
    let mut n = 22u64;
    for m in members {
        let name = m.name.len() as u64;
        n = n.saturating_add(30 + name + u64::from(m.size));
        n = n.saturating_add(46 + name);
    }
    n
}

fn push_zip_file(
    name: &str,
    path: &Path,
    meta: &fs::Metadata,
    out: &mut Vec<ZipMember>,
    bytes: &mut u64,
) -> Result<(), String> {
    if meta.len() > MAX_FILE as u64 {
        return Err("file too large".into());
    }
    *bytes = bytes.saturating_add(meta.len());
    if *bytes > MAX_FILE as u64 {
        return Err("file too large".into());
    }
    let (crc, size) = crc32_file(path)?;
    if u64::from(size) != meta.len() {
        return Err("file changed".into());
    }
    out.push(ZipMember {
        name: name.to_string(),
        src: Some(path.to_path_buf()),
        crc,
        size,
    });
    Ok(())
}

fn collect_zip_tree(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<ZipMember>,
    n: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    out.push(ZipMember {
        name: format!("{prefix}/"),
        src: None,
        crc: 0,
        size: 0,
    });
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    for ent in ents {
        *n += 1;
        if *n >= RM_MAX {
            return Err("folder too large".into());
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if sanitize_name(&name).is_none() || name.ends_with(".part") {
            continue;
        }
        let path = ent.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_zip_tree(&path, &format!("{prefix}/{name}"), out, n, bytes)?;
        } else if meta.is_file() {
            push_zip_file(&format!("{prefix}/{name}"), &path, &meta, out, bytes)?;
        }
    }
    Ok(())
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB88320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

#[cfg(test)]
fn crc32_bytes(data: &[u8]) -> u32 {
    !crc32_update(0xffffffff, data)
}

fn crc32_file(path: &Path) -> Result<(u32, u32), String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 65536];
    let mut crc = 0xffffffffu32;
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_FILE as u64 {
            return Err("file too large".into());
        }
        crc = crc32_update(crc, &buf[..n]);
    }
    Ok((!crc, size as u32))
}

fn write_u16<W: Write>(w: &mut W, v: u16) -> Result<(), String> {
    w.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())
}

fn write_u32<W: Write>(w: &mut W, v: u32) -> Result<(), String> {
    w.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())
}

fn write_local<W: Write>(w: &mut W, m: &ZipMember) -> Result<(), String> {
    w.write_all(b"PK\x03\x04").map_err(|e| e.to_string())?;
    write_u16(w, 20)?;
    write_u16(w, 1 << 11)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u32(w, m.crc)?;
    write_u32(w, m.size)?;
    write_u32(w, m.size)?;
    write_u16(w, m.name.len() as u16)?;
    write_u16(w, 0)?;
    w.write_all(m.name.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(src) = &m.src {
        copy_exact(w, src, m.size)?;
    }
    Ok(())
}

fn write_central<W: Write>(w: &mut W, m: &ZipMember, local_off: u32) -> Result<(), String> {
    w.write_all(b"PK\x01\x02").map_err(|e| e.to_string())?;
    write_u16(w, 20)?;
    write_u16(w, 20)?;
    write_u16(w, 1 << 11)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u32(w, m.crc)?;
    write_u32(w, m.size)?;
    write_u32(w, m.size)?;
    write_u16(w, m.name.len() as u16)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    let ext = if m.name.ends_with('/') { 0x10 } else { 0 };
    write_u32(w, ext)?;
    write_u32(w, local_off)?;
    w.write_all(m.name.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_eocd<W: Write>(w: &mut W, entries: u16, cd_size: u32, cd_off: u32) -> Result<(), String> {
    w.write_all(b"PK\x05\x06").map_err(|e| e.to_string())?;
    write_u16(w, 0)?;
    write_u16(w, 0)?;
    write_u16(w, entries)?;
    write_u16(w, entries)?;
    write_u32(w, cd_size)?;
    write_u32(w, cd_off)?;
    write_u16(w, 0)?;
    Ok(())
}

fn copy_exact<W: Write>(w: &mut W, path: &Path, expected: u32) -> Result<(), String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 65536];
    let mut left = u64::from(expected);
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if n as u64 > left {
            return Err("file changed".into());
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        left -= n as u64;
    }
    if left != 0 {
        return Err("file changed".into());
    }
    Ok(())
}

fn emit_zip_blob<F: FnMut(String)>(zip: &FolderZip, mut emit: F) -> Result<(), String> {
    let shown = &zip.download_name;
    let size = zip.size as usize;
    if size <= MAX_CHUNK {
        let mut buf = Vec::with_capacity(size);
        zip.write_to(&mut buf)?;
        emit(json!({
            "type": "file",
            "action": "blob",
            "name": shown,
            "size": buf.len(),
            "data": encode_b64(&buf)
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
    {
        let mut chunker = BlobChunker {
            emit: &mut emit,
            name: shown,
            buf: Vec::with_capacity(MAX_CHUNK),
        };
        zip.write_to(&mut chunker)?;
        chunker.flush().map_err(|e| e.to_string())?;
    }
    emit(json!({"type":"file","action":"blob-end","name":shown}).to_string());
    Ok(())
}

struct BlobChunker<'a, F: FnMut(String)> {
    emit: &'a mut F,
    name: &'a str,
    buf: Vec<u8>,
}

impl<'a, F: FnMut(String)> BlobChunker<'a, F> {
    fn emit_buf(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let data = encode_b64(&self.buf);
        self.buf.clear();
        (self.emit)(
            json!({
                "type": "file",
                "action": "blob-chunk",
                "name": self.name,
                "data": data
            })
            .to_string(),
        );
    }
}

impl<'a, F: FnMut(String)> Write for BlobChunker<'a, F> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut rest = buf;
        while !rest.is_empty() {
            if self.buf.len() >= MAX_CHUNK {
                self.emit_buf();
            }
            let space = MAX_CHUNK.saturating_sub(self.buf.len());
            if space == 0 {
                continue;
            }
            let n = rest.len().min(space);
            self.buf.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.emit_buf();
        Ok(())
    }
}

fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    if src.is_symlink() {
        return Err("invalid path".into());
    }
    if src.is_file() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(src, dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !src.is_dir() {
        return Err("not a file".into());
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for ent in fs::read_dir(src).map_err(|e| e.to_string())? {
        let ent = ent.map_err(|e| e.to_string())?;
        let name = ent.file_name().to_string_lossy().to_string();
        let Some(name) = sanitize_name(&name) else {
            continue;
        };
        copy_tree(&ent.path(), &dest.join(name))?;
    }
    Ok(())
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
        inbox.remove_at("home", "", "NewFolder").unwrap();
        assert!(!home.path().join("NewFolder").exists());
        inbox.mkdir_at("desktop", "", "Empty").unwrap();
        let gone = inbox.remove_at("desktop", "", "Empty").unwrap();
        assert!(gone.dir);
        assert!(!desk.join("Empty").exists());
        inbox.mkdir_at("desktop", "", "Tree").unwrap();
        inbox.put_bytes_at("desktop", "Tree", "a.txt", b"x").unwrap();
        inbox.mkdir_at("desktop", "Tree", "Sub").unwrap();
        inbox
            .put_bytes_at("desktop", "Tree/Sub", "b.txt", b"y")
            .unwrap();
        inbox.remove_at("desktop", "", "Tree").unwrap();
        assert!(!desk.join("Tree").exists());
        inbox.mkdir_at("desktop", "", "Old").unwrap();
        let renamed = inbox.rename_at("desktop", "", "Old", "Renamed").unwrap();
        assert!(renamed.dir);
        assert_eq!(renamed.name, "Renamed");
        assert!(desk.join("Renamed").is_dir());
        assert!(!desk.join("Old").exists());
        assert_eq!(
            inbox.rename_at("desktop", "", "Renamed", "Work").unwrap_err(),
            "already exists"
        );
        assert!(inbox.rename_at("desktop", "", "Renamed", "../x").is_err());
        inbox
            .put_bytes_at("desktop", "Work", "n.txt", b"1")
            .unwrap();
        inbox
            .rename_at("desktop", "Work", "n.txt", "m.txt")
            .unwrap();
        assert_eq!(fs::read(desk.join("Work").join("m.txt")).unwrap(), b"1");
        let rn = inbox.handle_message(&json!({
            "action": "rename",
            "root": "desktop",
            "path": "Work",
            "name": "m.txt",
            "to": "z.txt"
        }));
        assert!(rn.iter().any(|m| m.contains("\"renamed\"") && m.contains("z.txt")));
        fs::create_dir(home.path().join("Documents")).unwrap();
        inbox
            .copy_at("desktop", "Work", "z.txt", "documents", "")
            .unwrap();
        assert_eq!(
            fs::read(home.path().join("Documents").join("z.txt")).unwrap(),
            b"1"
        );
        assert!(desk.join("Work").join("z.txt").is_file());
        inbox
            .move_at("desktop", "Work", "z.txt", "inbox", "")
            .unwrap();
        assert!(!desk.join("Work").join("z.txt").exists());
        assert_eq!(inbox.get_bytes("z.txt").unwrap(), b"1");
        assert!(inbox
            .copy_at("desktop", "", "Work", "desktop", "Work")
            .is_err());
        let cp = inbox.handle_message(&json!({
            "action": "copy",
            "root": "inbox",
            "path": "",
            "name": "z.txt",
            "toRoot": "documents",
            "toPath": ""
        }));
        assert!(cp.iter().any(|m| m.contains("\"copied\"")));
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

    #[test]
    fn crc32_matches_ieee() {
        assert_eq!(crc32_bytes(b""), 0);
        assert_eq!(crc32_bytes(b"123456789"), 0xCBF43926);
    }

    fn store_zip_file<'a>(zip: &'a [u8], want: &str) -> Option<&'a [u8]> {
        let mut i = 0;
        while i + 30 <= zip.len() {
            if &zip[i..i + 4] != b"PK\x03\x04" {
                break;
            }
            let method = u16::from_le_bytes(zip[i + 8..i + 10].try_into().ok()?);
            let csize = u32::from_le_bytes(zip[i + 18..i + 22].try_into().ok()?) as usize;
            let nlen = u16::from_le_bytes(zip[i + 26..i + 28].try_into().ok()?) as usize;
            let elen = u16::from_le_bytes(zip[i + 28..i + 30].try_into().ok()?) as usize;
            let name_at = i + 30;
            let data_at = name_at + nlen + elen;
            if name_at + nlen > zip.len() || data_at + csize > zip.len() {
                return None;
            }
            let name = std::str::from_utf8(&zip[name_at..name_at + nlen]).ok()?;
            if name == want && method == 0 {
                return Some(&zip[data_at..data_at + csize]);
            }
            i = data_at + csize;
        }
        None
    }

    #[test]
    fn folder_get_emits_store_zip() {
        let (_dir, inbox) = tmp_inbox();
        inbox.mkdir_at("inbox", "", "Pack").unwrap();
        inbox
            .put_bytes_at("inbox", "Pack", "note.txt", b"hello-zip")
            .unwrap();
        inbox.mkdir_at("inbox", "Pack", "Sub").unwrap();
        inbox
            .put_bytes_at("inbox", "Pack/Sub", "inner.bin", b"AB")
            .unwrap();
        let zip = inbox.folder_zip_at("inbox", "", "Pack").unwrap();
        assert_eq!(zip.download_name, "Pack.zip");
        let mut bytes = Vec::new();
        zip.write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, zip.size);
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(&bytes[bytes.len() - 22..bytes.len() - 18], b"PK\x05\x06");
        assert_eq!(store_zip_file(&bytes, "Pack/note.txt"), Some(&b"hello-zip"[..]));
        assert_eq!(store_zip_file(&bytes, "Pack/Sub/inner.bin"), Some(&b"AB"[..]));
        assert!(store_zip_file(&bytes, "Pack/Sub/").is_some());

        let mut msgs = Vec::new();
        inbox.emit_blob_at("inbox", "", "Pack", |m| msgs.push(m)).unwrap();
        assert!(msgs[0].contains("\"name\":\"Pack.zip\""), "{}", msgs[0]);
        assert!(msgs[0].contains("blob"), "{}", msgs[0]);
        assert!(msgs[0].contains(&encode_b64(&bytes)), "{}", msgs[0]);
        assert!(inbox.readable_path_at("inbox", "", "Pack").is_err());
    }

    #[test]
    fn empty_folder_zip_is_valid() {
        let (_dir, inbox) = tmp_inbox();
        inbox.mkdir_at("inbox", "", "Empty").unwrap();
        let zip = inbox.folder_zip_at("inbox", "", "Empty").unwrap();
        let mut bytes = Vec::new();
        zip.write_to(&mut bytes).unwrap();
        assert_eq!(store_zip_file(&bytes, "Empty/"), Some(&b""[..]));
        assert!(inbox.folder_zip_at("inbox", "", "missing").is_err());
        inbox.put_bytes("file.txt", b"x").unwrap();
        assert_eq!(
            inbox.folder_zip_at("inbox", "", "file.txt").err().as_deref(),
            Some("not a folder")
        );
    }

    #[test]
    fn names_from_value_prefers_array_and_dedupes() {
        let v = json!({"name": "a.txt", "names": ["b.txt", "b.txt", "../x", "c.txt"]});
        assert_eq!(names_from_value(&v), vec!["b.txt".to_string(), "c.txt".to_string()]);
        assert_eq!(
            names_from_value(&json!({"name": "solo.txt"})),
            vec!["solo.txt".to_string()]
        );
        assert!(names_from_value(&json!({"name": "../x"})).is_empty());
    }

    #[test]
    fn selection_zip_and_bulk_copy_delete() {
        let (_dir, inbox) = tmp_inbox();
        inbox.put_bytes("a.txt", b"AAA").unwrap();
        inbox.put_bytes("b.txt", b"BBB").unwrap();
        inbox.mkdir_at("inbox", "", "Pack").unwrap();
        inbox
            .put_bytes_at("inbox", "Pack", "in.txt", b"IN")
            .unwrap();
        let zip = inbox
            .folder_zip_names_at(
                "inbox",
                "",
                &["a.txt".into(), "Pack".into(), "b.txt".into()],
            )
            .unwrap();
        assert_eq!(zip.download_name, "files.zip");
        let mut bytes = Vec::new();
        zip.write_to(&mut bytes).unwrap();
        assert_eq!(store_zip_file(&bytes, "a.txt"), Some(&b"AAA"[..]));
        assert_eq!(store_zip_file(&bytes, "b.txt"), Some(&b"BBB"[..]));
        assert_eq!(store_zip_file(&bytes, "Pack/in.txt"), Some(&b"IN"[..]));

        let mut msgs = Vec::new();
        inbox
            .emit_blob_names_at(
                "inbox",
                "",
                &["a.txt".into(), "b.txt".into()],
                |m| msgs.push(m),
            )
            .unwrap();
        assert!(msgs[0].contains("\"name\":\"files.zip\""), "{}", msgs[0]);

        inbox
            .transfer_names_at(
                "inbox",
                "",
                &["a.txt".into(), "b.txt".into()],
                "inbox",
                "",
                false,
            )
            .unwrap();
        assert_eq!(inbox.get_bytes("a-1.txt").unwrap(), b"AAA");
        assert_eq!(inbox.get_bytes("b-1.txt").unwrap(), b"BBB");

        let cp = inbox.handle_message(&json!({
            "action": "copy",
            "names": ["a.txt", "b.txt"],
            "toRoot": "inbox",
            "toPath": "Pack"
        }));
        assert!(cp.iter().any(|m| m.contains("\"copied\"")), "{cp:?}");
        assert!(inbox.join_under("inbox", "Pack", "a.txt").unwrap().is_file());
        assert!(inbox.join_under("inbox", "Pack", "b.txt").unwrap().is_file());

        let del = inbox.handle_message(&json!({
            "action": "delete",
            "names": ["a.txt", "b.txt"]
        }));
        assert!(del.iter().any(|m| m.contains("\"deleted\"")), "{del:?}");
        assert!(inbox.get_bytes("a.txt").is_err());
        assert!(inbox.get_bytes("b.txt").is_err());
    }
}
