//! Reading an agent's prompt off its screen. From keel's agents.rs.

/// The numbered choices of an approval prompt ("❯ 1. Yes", "  2. No, and tell
/// Claude…"), so they can be answered from the sidebar by typing the number.
pub fn options(bottom: &str) -> Vec<(char, String)> {
    let mut out: Vec<(char, String)> = vec![];
    for l in bottom.lines() {
        let t = l
            .trim_start()
            .trim_start_matches(['❯', '›', '>', '●', '○', '│', '|', ' ']);
        let mut chars = t.chars();
        let (Some(d), Some('.')) = (chars.next(), chars.next()) else {
            continue;
        };
        let label = chars.as_str().trim();
        if d.is_ascii_digit() && !label.is_empty() && !out.iter().any(|(k, _)| *k == d) {
            out.push((d, label.trim_end_matches(['│', '|', ' ']).to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_TRUST: &str = "Claude Code'll be able to read, edit, and execute files here.\n❯ 1. Yes, I trust this folder\n  2. No, exit\nEnter to confirm · Esc to cancel";
    const CODEX_TRUST: &str = "Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Back\nenter continue · esc back";
    const CLAUDE_BASH: &str = "│ Do you want to proceed?\n│ ❯ 1. Yes\n│   2. Yes, and don't ask again for npm test commands\n│   3. No, and tell Claude what to do differently (esc)";

    #[test]
    fn numbered_choices() {
        assert_eq!(
            options(CLAUDE_TRUST),
            vec![
                ('1', "Yes, I trust this folder".to_string()),
                ('2', "No, exit".to_string())
            ]
        );
        assert_eq!(
            options(CODEX_TRUST)[0],
            ('1', "Trust and continue".to_string())
        );
        let bash = options(CLAUDE_BASH);
        assert_eq!(bash.len(), 3);
        assert_eq!(bash[2].0, '3');
    }
}
