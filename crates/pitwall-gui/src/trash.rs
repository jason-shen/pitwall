//! Moving a file to the Trash rather than deleting it, so a slip can be undone.

use std::path::Path;

use crate::i18n::tr;

/// macOS: the Finder's own Trash (NSFileManager), so Put Back works.
#[cfg(target_os = "macos")]
pub fn trash(path: &Path) -> std::io::Result<()> {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    let fail = |what: &str| {
        std::io::Error::other(crate::i18n::trf!(
            "couldn't move {file} to the Trash: {why}",
            file = path.display(),
            why = tr(what)
        ))
    };
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| fail("odd path"))?;
    let (Some(fm_class), Some(url_class), Some(str_class)) = (
        AnyClass::get(c"NSFileManager"),
        AnyClass::get(c"NSURL"),
        AnyClass::get(c"NSString"),
    ) else {
        return Err(fail("Foundation isn't loaded"));
    };
    unsafe {
        let s: *mut AnyObject = objc2::msg_send![str_class, stringWithUTF8String: c.as_ptr()];
        let url: *mut AnyObject = objc2::msg_send![url_class, fileURLWithPath: s];
        let fm: *mut AnyObject = objc2::msg_send![fm_class, defaultManager];
        if s.is_null() || url.is_null() || fm.is_null() {
            return Err(fail("no file manager"));
        }
        let none: *mut *mut AnyObject = std::ptr::null_mut();
        let ok: Bool =
            objc2::msg_send![fm, trashItemAtURL: url, resultingItemURL: none, error: none];
        if ok.as_bool() {
            Ok(())
        } else {
            Err(fail("the Finder refused"))
        }
    }
}

/// Elsewhere: the freedesktop.org Trash in the home folder, with the
/// .trashinfo file a file manager needs to restore it.
#[cfg(not(target_os = "macos"))]
pub fn trash(path: &Path) -> std::io::Result<()> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
        .ok_or_else(|| std::io::Error::other(tr("no home folder")))?
        .join("Trash");
    let (files, info) = (base.join("files"), base.join("info"));
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut n = 1;
    let mut taken = name.clone();
    while files.join(&taken).exists() || info.join(format!("{taken}.trashinfo")).exists() {
        n += 1;
        taken = format!("{name}.{n}");
    }
    let full = std::fs::canonicalize(path)?;
    let now = std::process::Command::new("date")
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    std::fs::write(
        info.join(format!("{taken}.trashinfo")),
        format!(
            "[Trash Info]\nPath={}\nDeletionDate={now}\n",
            full.display()
        ),
    )?;
    std::fs::rename(path, files.join(&taken))
}

// Linux only: on a Mac this would put a file in your real Trash every run.
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #[test]
    fn a_file_goes_to_the_trash_with_what_restores_it() {
        let dir = std::env::temp_dir().join(format!("pitwall-trash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        let f = dir.join("old brief.md");
        std::fs::write(&f, "# gone\n").unwrap();
        super::trash(&f).unwrap();
        assert!(!f.exists());
        let trash = dir.join("data/Trash");
        assert!(trash.join("files/old brief.md").exists());
        let info = std::fs::read_to_string(trash.join("info/old brief.md.trashinfo")).unwrap();
        assert!(info.contains("Path=") && info.contains("old brief.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
