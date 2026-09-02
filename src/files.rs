//! Host inbox file transfer. Names are sanitized into a dedicated directory.

use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_FILE: usize = 8 * 1024 * 1024;
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

#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
}

impl FileEntry {
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "size": self.size})
    }
}

struct Incoming {
    name: String,
    buf: Vec<u8>,
    size: usize,
}

pub struct Inbox {
    pub dir: PathBuf,
    incoming: Mutex<HashMap<String, Incoming>>,
}

impl Inbox {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            incoming: Mutex::new(HashMap::new()),
        }
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

    pub fn list(&self) -> Vec<FileEntry> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(_) => return out,
        };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if sanitize_name(&name).is_none() {
                continue;
            }
            if name.ends_with(".part") {
                continue;
            }
            let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(FileEntry { name, size });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn put_bytes(&self, name: &str, data: &[u8]) -> Result<FileEntry, String> {
        if data.len() > MAX_FILE {
            return Err("file too large".into());
        }
        self.ensure_dir()?;
        let path = self.dest(name)?;
        let tmp = self
            .dir
            .join(format!("{}.part", path.file_name().unwrap().to_string_lossy()));
        fs::write(&tmp, data).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })?;
        Ok(FileEntry {
            name: path.file_name().unwrap().to_string_lossy().into(),
            size: data.len() as u64,
        })
    }

    pub fn get_bytes(&self, name: &str) -> Result<Vec<u8>, String> {
        let path = self.dest(name)?;
        let data = fs::read(&path).map_err(|_| "file not found".to_string())?;
        if data.len() > MAX_FILE {
            return Err("file too large".into());
        }
        Ok(data)
    }

    pub fn begin(&self, id: &str, name: &str, size: usize) -> Result<(), String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        if size == 0 || size > MAX_FILE {
            return Err("invalid file size".into());
        }
        let mut g = self.incoming.lock();
        if g.len() >= 4 {
            return Err("too many transfers".into());
        }
        g.insert(
            id,
            Incoming {
                name,
                buf: Vec::with_capacity(size.min(MAX_CHUNK * 4)),
                size,
            },
        );
        Ok(())
    }

    pub fn chunk(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        if data.is_empty() || data.len() > MAX_CHUNK * 2 {
            return Err("invalid chunk".into());
        }
        let mut g = self.incoming.lock();
        let inc = g.get_mut(&id).ok_or_else(|| "unknown transfer".to_string())?;
        if inc.buf.len() + data.len() > inc.size || inc.buf.len() + data.len() > MAX_FILE {
            return Err("file too large".into());
        }
        inc.buf.extend_from_slice(data);
        Ok(())
    }

    pub fn end(&self, id: &str) -> Result<FileEntry, String> {
        let id = sanitize_id(id).ok_or_else(|| "invalid transfer id".to_string())?;
        let inc = self
            .incoming
            .lock()
            .remove(&id)
            .ok_or_else(|| "unknown transfer".to_string())?;
        if inc.buf.len() != inc.size {
            return Err("incomplete file".into());
        }
        self.put_bytes(&inc.name, &inc.buf)
    }

    pub fn handle_message(&self, v: &Value) -> Vec<String> {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        match action {
            "list" => vec![json!({
                "type": "file",
                "action": "list",
                "files": self.list().iter().map(|e| e.to_json()).collect::<Vec<_>>()
            })
            .to_string()],
            "put" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                match decode_b64(data).and_then(|b| self.put_bytes(name, &b)) {
                    Ok(ent) => vec![ok_json(&ent)],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "begin" => {
                let id = v.get("id").and_then(|n| n.as_str()).unwrap_or("");
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let size = v.get("size").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                match self.begin(id, name, size) {
                    Ok(()) => vec![json!({"type":"file","action":"accept","id":id}).to_string()],
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
                    Ok(ent) => vec![ok_json(&ent)],
                    Err(e) => vec![err_json(&e)],
                }
            }
            "get" => {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                match self.get_bytes(name) {
                    Ok(bytes) => blob_replies(name, &bytes),
                    Err(e) => vec![err_json(&e)],
                }
            }
            _ => vec![err_json("unknown file action")],
        }
    }
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

fn ok_json(ent: &FileEntry) -> String {
    json!({"type":"file","action":"ok","name":ent.name,"size":ent.size}).to_string()
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
    }

    #[test]
    fn chunked_put_assembles() {
        let (_dir, inbox) = tmp_inbox();
        inbox.begin("t1", "chunk.bin", 8).unwrap();
        inbox.chunk("t1", b"abcd").unwrap();
        inbox.chunk("t1", b"efgh").unwrap();
        let ent = inbox.end("t1").unwrap();
        assert_eq!(ent.size, 8);
        assert_eq!(inbox.get_bytes("chunk.bin").unwrap(), b"abcdefgh");
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
    fn oversized_put_rejected() {
        let (_dir, inbox) = tmp_inbox();
        let big = vec![0u8; MAX_FILE + 1];
        assert!(inbox.put_bytes("big.bin", &big).is_err());
    }

    #[test]
    fn blob_replies_chunk_large_payloads() {
        let data = vec![7u8; MAX_CHUNK + 10];
        let msgs = blob_replies("n.bin", &data);
        assert!(msgs[0].contains("blob-begin"));
        assert!(msgs.iter().any(|m| m.contains("blob-chunk")));
        assert!(msgs.last().unwrap().contains("blob-end"));
    }
}
