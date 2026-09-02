//! Record the live fMP4 while a remote session is driving.

use crate::files::{sanitize_name, FileEntry};
use crate::protocol::{TYPE_FRAG, TYPE_INIT};
use parking_lot::Mutex;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Active {
    name: String,
    file: File,
}

struct Inner {
    dir: PathBuf,
    active: Option<Active>,
}

#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Mutex<Inner>>,
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { dir, active: None })),
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.inner.lock().dir.clone()
    }

    pub fn is_active(&self) -> bool {
        self.inner.lock().active.is_some()
    }

    pub fn active_name(&self) -> Option<String> {
        self.inner.lock().active.as_ref().map(|a| a.name.clone())
    }

    fn dest(dir: &Path, name: &str) -> Result<PathBuf, String> {
        let name = sanitize_name(name).ok_or_else(|| "invalid file name".to_string())?;
        let path = dir.join(&name);
        if path.parent() != Some(dir) && path.parent() != Some(Path::new("")) {
            return Err("invalid file name".into());
        }
        Ok(path)
    }

    fn uniquify(dir: &Path, stem: &str) -> String {
        let mut name = format!("{stem}.mp4");
        let mut n = 2u32;
        while dir.join(&name).exists() {
            name = format!("{stem}-{n}.mp4");
            n += 1;
            if n > 99 {
                break;
            }
        }
        name
    }

    pub fn start(&self, init: Option<&[u8]>) -> Result<String, String> {
        let mut g = self.inner.lock();
        if let Some(a) = g.active.as_ref() {
            return Ok(a.name.clone());
        }
        fs::create_dir_all(&g.dir).map_err(|e| e.to_string())?;
        let stem = format!(
            "session-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let name = Self::uniquify(&g.dir, &stem);
        let path = Self::dest(&g.dir, &name)?;
        let mut file = File::create(&path).map_err(|e| e.to_string())?;
        if let Some(init) = init {
            if !init.is_empty() {
                file.write_all(init).map_err(|e| e.to_string())?;
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        g.active = Some(Active { name: name.clone(), file });
        Ok(name)
    }

    pub fn write_unit(&self, kind: u8, data: &[u8]) {
        if kind != TYPE_INIT && kind != TYPE_FRAG {
            return;
        }
        if data.is_empty() {
            return;
        }
        let mut g = self.inner.lock();
        if let Some(a) = g.active.as_mut() {
            let _ = a.file.write_all(data);
        }
    }

    pub fn stop(&self) -> Option<String> {
        let mut g = self.inner.lock();
        let mut a = g.active.take()?;
        let _ = a.file.flush();
        Some(a.name)
    }

    pub fn list(&self) -> Vec<FileEntry> {
        let dir = self.inner.lock().dir.clone();
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => return out,
        };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if sanitize_name(&name).is_none() {
                continue;
            }
            if !name.ends_with(".mp4") {
                continue;
            }
            let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(FileEntry::file(name, size));
        }
        out.sort_by(|a, b| b.name.cmp(&a.name));
        out
    }

    pub fn readable_path(&self, name: &str) -> Result<(PathBuf, u64), String> {
        let dir = self.inner.lock().dir.clone();
        let path = Self::dest(&dir, name)?;
        if !name.ends_with(".mp4") {
            return Err("file not found".into());
        }
        let meta = fs::metadata(&path).map_err(|_| "file not found".to_string())?;
        Ok((path, meta.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TYPE_JPEG;
    use tempfile::tempdir;

    #[test]
    fn start_writes_init_and_fragments_then_stop_lists() {
        let dir = tempdir().unwrap();
        let rec = Recorder::new(dir.path().to_path_buf());
        assert!(!rec.is_active());
        rec.write_unit(TYPE_FRAG, b"late");
        assert!(rec.list().is_empty());
        let name = rec.start(Some(b"ftyp-init")).unwrap();
        assert!(name.starts_with("session-"));
        assert!(name.ends_with(".mp4"));
        assert!(rec.is_active());
        rec.write_unit(TYPE_FRAG, b"mdat-frag");
        rec.write_unit(TYPE_JPEG, b"nope");
        rec.write_unit(TYPE_INIT, b"");
        let stopped = rec.stop().unwrap();
        assert_eq!(stopped, name);
        assert!(!rec.is_active());
        assert!(rec.stop().is_none());
        let list = rec.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, name);
        let (path, len) = rec.readable_path(&name).unwrap();
        let data = fs::read(&path).unwrap();
        assert_eq!(data, b"ftyp-initmdat-frag");
        assert_eq!(len, data.len() as u64);
        assert!(rec.readable_path("../x.mp4").is_err());
        assert!(rec.readable_path("notes.txt").is_err());
    }

    #[test]
    fn second_start_is_idempotent_until_stop() {
        let dir = tempdir().unwrap();
        let rec = Recorder::new(dir.path().to_path_buf());
        let a = rec.start(Some(b"A")).unwrap();
        let b = rec.start(Some(b"B")).unwrap();
        assert_eq!(a, b);
        rec.stop();
        let c = rec.start(Some(b"C")).unwrap();
        assert_ne!(c, a);
        rec.stop();
        assert_eq!(rec.list().len(), 2);
    }
}
