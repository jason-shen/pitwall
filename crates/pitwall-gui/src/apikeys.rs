//! API keys for other services (OpenAI, Stripe, GitHub tokens), kept in
//! ~/.pitwall/apikeys.json. Same scheme as keel's: each secret is sealed with
//! AES-256-GCM under a random key, and that key is wrapped with a key derived
//! from the API keys password (PBKDF2-HMAC-SHA256). The file never holds a
//! secret in the clear, and nothing is decrypted until the password is typed.

use std::num::NonZeroU32;
use std::path::PathBuf;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

pub const ITERATIONS: u32 = 600_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApiLock {
    pub salt: String,
    pub iterations: u32,
    /// The secrets key, sealed with the password's key: nonce then ciphertext, hex.
    pub wrapped_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub service: String,
    /// Sealed secret, hex. Never the plain value.
    pub secret: String,
    /// Last four characters, shown after the dots so keys can be told apart.
    pub hint: String,
    pub notes: String,
    pub created_unix: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Store {
    pub lock: Option<ApiLock>,
    pub keys: Vec<ApiKey>,
}

fn path() -> PathBuf {
    pitwall_core::paths::home().join("apikeys.json")
}

impl Store {
    pub fn load() -> Store {
        std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Written to a temp file first and readable by you alone: the secrets
    /// are sealed, but their names and services aren't.
    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, p)
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SystemRandom::new().fill(&mut b).expect("system randomness");
    b
}

fn password_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations.max(1)).unwrap(),
        salt,
        password.as_bytes(),
        &mut out,
    );
    out
}

fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).unwrap());
    let nonce = random::<NONCE_LEN>();
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .unwrap();
    let mut out = nonce.to_vec();
    out.extend(buf);
    out
}

fn open(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let mut buf = ct.to_vec();
    let plain = k
        .open_in_place(Nonce::try_assume_unique_for_key(nonce).ok()?, Aad::empty(), &mut buf)
        .ok()?;
    Some(plain.to_vec())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 == 1 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn new_id() -> String {
    hex(&random::<8>())
}

impl ApiLock {
    /// A new lock and the secrets key it wraps.
    pub fn new(password: &str) -> (ApiLock, [u8; 32]) {
        let raw = random::<32>();
        (Self::wrap(&raw, password), raw)
    }

    pub fn wrap(raw: &[u8; 32], password: &str) -> ApiLock {
        let salt = random::<16>();
        let wrapped = seal(&password_key(password, &salt, ITERATIONS), raw);
        ApiLock {
            salt: hex(&salt),
            iterations: ITERATIONS,
            wrapped_key: hex(&wrapped),
        }
    }

    /// The secrets key, or None if the password is wrong. Deliberately slow
    /// (PBKDF2), so it runs off the UI thread.
    pub fn unlock(&self, password: &str) -> Option<[u8; 32]> {
        let salt = unhex(&self.salt)?;
        open(
            &password_key(password, &salt, self.iterations),
            &unhex(&self.wrapped_key)?,
        )?
        .try_into()
        .ok()
    }
}

pub fn seal_secret(raw: &[u8; 32], secret: &str) -> String {
    hex(&seal(raw, secret.as_bytes()))
}

pub fn open_secret(raw: &[u8; 32], sealed: &str) -> Option<String> {
    String::from_utf8(open(raw, &unhex(sealed)?)?).ok()
}

/// Short keys get no hint: four characters of an eight-character secret is
/// half of it.
pub fn hint(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 16 {
        return String::new();
    }
    chars[chars.len() - 4..].iter().collect()
}

pub fn masked(hint: &str) -> String {
    format!("{}{hint}", "•".repeat(if hint.is_empty() { 12 } else { 8 }))
}

pub fn check_new(pw: &str, confirm: &str) -> Result<(), &'static str> {
    if pw.chars().count() < 8 {
        return Err("Use at least 8 characters");
    }
    if pw != confirm {
        return Err("The passwords don't match");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_password() {
        let (lock, raw) = ApiLock::new("correct horse");
        let sealed = seal_secret(&raw, "sk-test-1234567890abcdef");
        assert!(!sealed.contains("sk-test"));
        let again = lock.unlock("correct horse").unwrap();
        assert_eq!(open_secret(&again, &sealed).unwrap(), "sk-test-1234567890abcdef");
        assert!(lock.unlock("wrong horse").is_none());
        // Re-wrapped under a new password, the same secrets still open.
        let moved = ApiLock::wrap(&again, "battery staple");
        let k = moved.unlock("battery staple").unwrap();
        assert_eq!(open_secret(&k, &sealed).unwrap(), "sk-test-1234567890abcdef");
    }

    #[test]
    fn hints_and_passwords() {
        assert_eq!(hint("sk-1234567890abcdef"), "cdef");
        assert_eq!(hint("short"), "");
        assert!(check_new("short", "short").is_err());
        assert!(check_new("long enough", "long enougj").is_err());
        assert!(check_new("long enough", "long enough").is_ok());
    }
}

// ── the app side: dialogs, timers, the password check off the UI thread ──

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::i18n::tr;

/// How long a revealed key stays on screen, and a copied one on the clipboard.
pub const SHOW_FOR: Duration = Duration::from_secs(30);
/// After the password is typed, keys can be added or replaced for this long
/// without asking again. Revealing and copying always ask.
const SAVE_GRACE: Duration = Duration::from_secs(300);

#[derive(Clone, PartialEq)]
pub enum Action {
    Reveal(String),
    Copy(String),
    Save,
}

pub struct Prompt {
    pub action: Action,
    pub password: String,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Editor {
    /// The key being edited; None adds one.
    pub id: Option<String>,
    pub name: String,
    pub service: String,
    /// A new or replacement secret in plain text; empty keeps the saved one.
    pub secret: String,
    pub notes: String,
    pub show: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Change {
    pub current: String,
    pub new: String,
    pub confirm: String,
    pub error: Option<String>,
}

enum Outcome {
    Setup(ApiLock, [u8; 32]),
    Unlocked(Action, Option<[u8; 32]>),
    Changed(Option<ApiLock>),
}

#[derive(Default)]
pub struct KeysUi {
    pub editor: Option<Editor>,
    pub prompt: Option<Prompt>,
    pub change: Option<Change>,
    pub setup_pw: String,
    pub setup_confirm: String,
    pub setup_error: Option<String>,
    /// (key id, its value, when it hides again)
    pub revealed: Option<(String, String, Instant)>,
    save_key: Option<([u8; 32], Instant)>,
    clip: Option<(String, Instant)>,
    working: Option<Receiver<Outcome>>,
}

impl KeysUi {
    /// A password is being checked.
    pub fn busy(&self) -> bool {
        self.working.is_some()
    }

    /// Typed recently enough that saving needs no password.
    pub fn unlocked(&self) -> bool {
        self.cached_key().is_some()
    }

    fn cached_key(&self) -> Option<[u8; 32]> {
        self.save_key
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(k, _)| k)
    }

    /// Drop anything decrypted, and don't leave a copied key on the
    /// clipboard for whoever sits down next.
    pub fn forget(&mut self) {
        self.revealed = None;
        self.save_key = None;
        self.prompt = None;
        if let Some((s, _)) = self.clip.take() {
            clear_clipboard_if(&s);
        }
    }
}

/// Only clears it if the key is still there; something else may have been
/// copied since.
fn clear_clipboard_if(secret: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.get_text().ok().as_deref() == Some(secret) {
            let _ = cb.clear();
        }
    }
}

impl App {
    fn keys_work(&mut self, f: impl FnOnce() -> Outcome + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        self.api_ui.working = Some(rx);
    }

    /// Every frame: finish a password check, and hide or clear what's due.
    pub fn keys_tick(&mut self) {
        let now = Instant::now();
        let k = &mut self.api_ui;
        if k.revealed.as_ref().is_some_and(|r| now >= r.2) {
            k.revealed = None;
        }
        if k.clip.as_ref().is_some_and(|c| now >= c.1) {
            if let Some((s, _)) = k.clip.take() {
                clear_clipboard_if(&s);
            }
        }
        let next = [k.revealed.as_ref().map(|r| r.2), k.clip.as_ref().map(|c| c.1)]
            .into_iter()
            .flatten()
            .min();
        if let Some(t) = next {
            self.ctx.request_repaint_after(t.saturating_duration_since(now));
        }
        let done = self.api_ui.working.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(o) = done {
            self.api_ui.working = None;
            self.keys_done(o);
        }
    }

    fn keys_done(&mut self, o: Outcome) {
        match o {
            Outcome::Setup(lock, raw) => {
                self.api_keys.lock = Some(lock);
                self.keys_save();
                let k = &mut self.api_ui;
                k.setup_pw.clear();
                k.setup_confirm.clear();
                k.save_key = Some((raw, Instant::now() + SAVE_GRACE));
            }
            Outcome::Unlocked(action, None) => {
                if let Some(p) = self.api_ui.prompt.as_mut() {
                    p.error = Some(tr("Wrong password").into());
                } else {
                    self.api_ui.prompt = Some(Prompt {
                        action,
                        password: String::new(),
                        error: Some(tr("Wrong password").into()),
                    });
                }
            }
            Outcome::Unlocked(action, Some(raw)) => {
                self.api_ui.prompt = None;
                self.api_ui.save_key = Some((raw, Instant::now() + SAVE_GRACE));
                self.keys_act(action, raw);
            }
            Outcome::Changed(None) => {
                if let Some(c) = self.api_ui.change.as_mut() {
                    c.error = Some(tr("The current password is wrong").into());
                }
            }
            Outcome::Changed(Some(lock)) => {
                self.api_keys.lock = Some(lock);
                self.keys_save();
                self.api_ui.change = None;
                self.api_ui.forget();
                self.notify(tr("API keys password changed"), false);
            }
        }
    }

    fn keys_act(&mut self, action: Action, raw: [u8; 32]) {
        let open = |id: &str| {
            self.api_keys
                .keys
                .iter()
                .find(|k| k.id == id)
                .and_then(|k| open_secret(&raw, &k.secret))
        };
        match action {
            Action::Reveal(id) => match open(&id) {
                Some(s) => self.api_ui.revealed = Some((id, s, Instant::now() + SHOW_FOR)),
                None => self.notify(tr("That key couldn't be decrypted"), true),
            },
            Action::Copy(id) => match open(&id) {
                Some(s) => self.keys_copy(s),
                None => self.notify(tr("That key couldn't be decrypted"), true),
            },
            Action::Save => self.keys_commit(raw),
        }
    }

    fn keys_copy(&mut self, secret: String) {
        self.ctx.copy_text(secret.clone());
        self.api_ui.clip = Some((secret, Instant::now() + SHOW_FOR));
        self.notify(tr("Copied. The clipboard is cleared in 30 seconds."), false);
    }

    fn keys_save(&mut self) {
        if let Err(e) = self.api_keys.save() {
            self.notify(crate::i18n::trf!("Couldn't save API keys: {e}", e = e), true);
        }
    }

    pub fn keys_setup(&mut self) {
        let k = &mut self.api_ui;
        if let Err(e) = check_new(&k.setup_pw, &k.setup_confirm) {
            k.setup_error = Some(tr(e).into());
            return;
        }
        k.setup_error = None;
        let pw = k.setup_pw.clone();
        self.keys_work(move || {
            let (lock, raw) = ApiLock::new(&pw);
            Outcome::Setup(lock, raw)
        });
    }

    /// Reveal, copy or save: straight away if the action allows it and the
    /// password was typed recently, otherwise ask for it.
    pub fn keys_request(&mut self, action: Action) {
        // Revealing the one already showing copies what's on screen.
        if let (Action::Copy(id), Some((rid, s, _))) = (&action, &self.api_ui.revealed) {
            if id == rid {
                let s = s.clone();
                return self.keys_copy(s);
            }
        }
        match (&action, self.api_ui.cached_key()) {
            (Action::Save, Some(raw)) => self.keys_commit(raw),
            _ => {
                self.api_ui.prompt = Some(Prompt {
                    action,
                    password: String::new(),
                    error: None,
                })
            }
        }
    }

    pub fn keys_submit_prompt(&mut self) {
        let Some(p) = self.api_ui.prompt.as_mut() else {
            return;
        };
        let Some(lock) = self.api_keys.lock.clone() else {
            return;
        };
        let pw = std::mem::take(&mut p.password);
        p.error = None;
        let action = p.action.clone();
        self.keys_work(move || {
            let raw = lock.unlock(&pw);
            Outcome::Unlocked(action, raw)
        });
    }

    pub fn keys_open_editor(&mut self, id: Option<&str>) {
        let ed = match id.and_then(|id| self.api_keys.keys.iter().find(|k| k.id == id)) {
            Some(k) => Editor {
                id: Some(k.id.clone()),
                name: k.name.clone(),
                service: k.service.clone(),
                notes: k.notes.clone(),
                ..Default::default()
            },
            None => Editor::default(),
        };
        self.api_ui.editor = Some(ed);
    }

    /// Save the editor. A new secret needs the password; a rename doesn't.
    pub fn keys_save_editor(&mut self) {
        let Some(ed) = self.api_ui.editor.as_mut() else {
            return;
        };
        if ed.name.trim().is_empty() {
            ed.error = Some(tr("Give it a name").into());
            return;
        }
        if ed.id.is_none() && ed.secret.trim().is_empty() {
            ed.error = Some(tr("Paste the key").into());
            return;
        }
        ed.error = None;
        if ed.secret.trim().is_empty() {
            return self.keys_commit([0; 32]);
        }
        self.keys_request(Action::Save);
    }

    fn keys_commit(&mut self, raw: [u8; 32]) {
        let Some(ed) = self.api_ui.editor.take() else {
            return;
        };
        let secret = ed.secret.trim();
        let mut k = ed
            .id
            .as_ref()
            .and_then(|id| self.api_keys.keys.iter().find(|k| k.id == *id).cloned())
            .unwrap_or_else(|| ApiKey {
                id: new_id(),
                created_unix: pitwall_core::now_unix(),
                ..Default::default()
            });
        k.name = ed.name.trim().to_string();
        k.service = ed.service.trim().to_string();
        k.notes = ed.notes.trim().to_string();
        if !secret.is_empty() {
            k.secret = seal_secret(&raw, secret);
            k.hint = hint(secret);
            // A revealed copy would now be the old value.
            if self.api_ui.revealed.as_ref().is_some_and(|r| r.0 == k.id) {
                self.api_ui.revealed = None;
            }
        }
        match self.api_keys.keys.iter_mut().find(|x| x.id == k.id) {
            Some(x) => *x = k,
            None => self.api_keys.keys.push(k),
        }
        self.keys_save();
    }

    pub fn keys_delete(&mut self, id: &str) {
        self.api_keys.keys.retain(|k| k.id != id);
        if self.api_ui.revealed.as_ref().is_some_and(|r| r.0 == id) {
            self.api_ui.revealed = None;
        }
        self.keys_save();
    }

    pub fn keys_submit_change(&mut self) {
        let Some(c) = self.api_ui.change.as_mut() else {
            return;
        };
        if let Err(e) = check_new(&c.new, &c.confirm) {
            c.error = Some(tr(e).into());
            return;
        }
        let Some(lock) = self.api_keys.lock.clone() else {
            return;
        };
        c.error = None;
        let (current, new) = (c.current.clone(), c.new.clone());
        self.keys_work(move || {
            Outcome::Changed(lock.unlock(&current).map(|raw| ApiLock::wrap(&raw, &new)))
        });
    }

    /// For a forgotten password: the keys can't be opened without it, so
    /// they go, and a new password can be set.
    pub fn keys_reset(&mut self) {
        self.api_keys = Store::default();
        self.api_ui = KeysUi::default();
        self.keys_save();
    }
}
