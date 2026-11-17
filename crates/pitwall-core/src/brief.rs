//! A brief's steps: the checkboxes an agent ticks as it works. Shared so the
//! app's progress count and `pitwall brief tick 3` agree on what step 3 is.

#[derive(Debug, PartialEq)]
pub struct Step {
    /// Line index in the text.
    pub line: usize,
    pub done: bool,
    pub text: String,
}

/// Checkbox lines with something after the box. Lines in HTML comments (the
/// template's guide) and the template's empty `- [ ]` placeholders don't count.
pub fn steps(text: &str) -> Vec<Step> {
    let mut out = vec![];
    let mut in_comment = false;
    for (i, l) in text.lines().enumerate() {
        if in_comment {
            in_comment = !l.contains("-->");
            continue;
        }
        if let Some(at) = l.find("<!--") {
            in_comment = !l[at..].contains("-->");
            continue;
        }
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("- [").or_else(|| t.strip_prefix("* [")) else {
            continue;
        };
        let mut c = rest.chars();
        let (Some(mark), Some(']')) = (c.next(), c.next()) else {
            continue;
        };
        let label = c.as_str().trim();
        if label.is_empty() || !matches!(mark, ' ' | 'x' | 'X') {
            continue;
        }
        out.push(Step {
            line: i,
            done: mark != ' ',
            text: label.to_string(),
        });
    }
    out
}

/// The text with step `n` (from 1) ticked or unticked.
pub fn set(text: &str, n: usize, done: bool) -> Result<String, String> {
    let all = steps(text);
    let Some(step) = n.checked_sub(1).and_then(|i| all.get(i)) else {
        return Err(format!("no step {n}: the brief has {} step(s)", all.len()));
    };
    let mut out = String::with_capacity(text.len());
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == step.line {
            let at = l.find('[').expect("a step line has a box") + 1;
            out.push_str(&l[..at]);
            out.push(if done { 'x' } else { ' ' });
            out.push_str(&l[at + 1..]);
        } else {
            out.push_str(l);
        }
    }
    Ok(out)
}

/// The steps as an agent sees them: `1. [x] Write the parser`.
pub fn listing(text: &str) -> String {
    let all = steps(text);
    if all.is_empty() {
        return "This brief has no steps to tick.".into();
    }
    all.iter()
        .enumerate()
        .map(|(i, s)| format!("{}. [{}] {}", i + 1, if s.done { 'x' } else { ' ' }, s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEF: &str = "<!-- guide (feature)\n- [ ] not a step\n-->\n# T\n\n## Steps\n\n- [x] one\n- [ ] two\n  * [ ] three\n- [ ]\n";

    #[test]
    fn steps_skip_the_guide_and_empty_placeholders() {
        let s = steps(BRIEF);
        assert_eq!(s.len(), 3);
        assert!(s[0].done && !s[1].done);
        assert_eq!(s[2].text, "three");
    }

    #[test]
    fn ticking_changes_only_that_box() {
        let t = set(BRIEF, 2, true).unwrap();
        assert!(
            t.contains("- [x] two") && t.contains("- [ ] not a step") && t.ends_with("- [ ]\n")
        );
        let t = set(&t, 1, false).unwrap();
        assert!(t.contains("- [ ] one"));
        assert!(set(BRIEF, 4, true).is_err() && set(BRIEF, 0, true).is_err());
        assert_eq!(listing(BRIEF).lines().nth(1), Some("2. [ ] two"));
    }
}
