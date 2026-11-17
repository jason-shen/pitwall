//! Append-only JSONL audit log of every gated command and task lifecycle event.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

pub struct Audit {
    f: Mutex<Option<File>>,
}

impl Audit {
    pub fn open(path: &Path) -> Audit {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let f = OpenOptions::new().create(true).append(true).open(path).ok();
        Audit { f: Mutex::new(f) }
    }

    pub fn log(&self, mut v: serde_json::Value) {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ts".into(), chrono::Utc::now().to_rfc3339().into());
        }
        if let Some(f) = self.f.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{v}");
        }
    }
}
