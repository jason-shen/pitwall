//! The kitty keyboard protocol's flags, as far as muster speaks it: a
//! program (Codex, Claude Code) pushes flags to ask for keys it can tell
//! apart, and muster then sends Shift+Return as its own key instead of a
//! plain Return. Programs that never ask (vim, a shell) get the old bytes.

/// The flag stack a program pushes and pops (`CSI > f u`, `CSI < n u`,
/// `CSI = f ; m u`), with its answer to `CSI ? u`.
#[derive(Default, Debug)]
pub struct Keys {
    stack: Vec<u16>,
}

impl Keys {
    pub fn flags(&self) -> u16 {
        self.stack.last().copied().unwrap_or(0)
    }

    /// Distinct keys were asked for (the protocol's first flag).
    pub fn disambiguate(&self) -> bool {
        self.flags() & 1 != 0
    }

    /// Handle a CSI ending in `u`. Returns the reply to send, for a query.
    pub fn csi(&mut self, marker: Option<u8>, params: &[&[u16]]) -> Option<Vec<u8>> {
        let p = |i: usize| params.get(i).and_then(|p| p.first()).copied();
        match marker {
            Some(b'?') => return Some(format!("\x1b[?{}u", self.flags()).into_bytes()),
            Some(b'>') => {
                // A program that never pops mustn't grow this without end.
                if self.stack.len() >= 32 {
                    self.stack.remove(0);
                }
                self.stack.push(p(0).unwrap_or(0));
            }
            Some(b'<') => {
                let n = p(0).unwrap_or(1).max(1) as usize;
                let keep = self.stack.len().saturating_sub(n);
                self.stack.truncate(keep);
            }
            Some(b'=') => {
                let f = p(0).unwrap_or(0);
                let cur = self.flags();
                let new = match p(1).unwrap_or(1) {
                    2 => cur | f,
                    3 => cur & !f,
                    _ => f,
                };
                match self.stack.last_mut() {
                    Some(top) => *top = new,
                    None => self.stack.push(new),
                }
            }
            _ => {}
        }
        None
    }

    /// Bytes that put the current flags on a fresh terminal.
    pub fn restore(&self) -> Vec<u8> {
        match self.flags() {
            0 => vec![],
            f => format!("\x1b[>{f}u").into_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_query_pop() {
        let mut k = Keys::default();
        assert_eq!(k.csi(Some(b'?'), &[]), Some(b"\x1b[?0u".to_vec()));
        k.csi(Some(b'>'), &[&[1]]);
        assert!(k.disambiguate());
        assert_eq!(k.csi(Some(b'?'), &[]), Some(b"\x1b[?1u".to_vec()));
        k.csi(Some(b'='), &[&[8], &[2]]);
        assert_eq!(k.flags(), 9);
        assert_eq!(k.restore(), b"\x1b[>9u");
        k.csi(Some(b'<'), &[]);
        assert!(!k.disambiguate());
    }
}
