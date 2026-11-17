//! Each project's reviewer: an agent asked to look over the work and write
//! what it finds to `.pitwall/reviews/review-N.md`, numbered on from the
//! reviews already there.

use std::path::{Path, PathBuf};

pub fn dir(root: &Path) -> PathBuf {
    root.join(".pitwall").join("reviews")
}

/// The next review's file: one past the highest number there, so deleting
/// an old review doesn't hand its number to a new one.
pub fn next_path(root: &Path) -> PathBuf {
    let n = std::fs::read_dir(dir(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| number(&e.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0)
        + 1;
    dir(root).join(format!("review-{n}.md"))
}

fn number(name: &str) -> Option<u32> {
    name.strip_prefix("review-")?.strip_suffix(".md")?.parse().ok()
}

/// One line: it's typed into an agent's prompt when the reviewer is already
/// running, and a newline there would submit it early.
pub fn prompt(path: &Path) -> String {
    format!(
        "Review the work in this repository; don't change any code. Look at what's been done: \
         `git status`, `git diff HEAD` and any new untracked files. If the working tree is clean, \
         review the latest commit instead (`git show HEAD`). Read the surrounding code as you need \
         to. Look for bugs, edge cases, security problems, missing tests and anything unclear. \
         Write the review as Markdown to {}: a title, a short summary of what changed and your \
         overall verdict, then findings ordered by severity, each with the file and line, what's \
         wrong, and a suggested fix. Say plainly if you found nothing worth changing. That file \
         is the only one you may write.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_go_on_from_the_highest() {
        let root = std::env::temp_dir().join(format!("pitwall-reviews-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(next_path(&root).ends_with("review-1.md"));
        std::fs::create_dir_all(dir(&root)).unwrap();
        for f in ["review-1.md", "review-7.md", "review-x.md", "notes.md"] {
            std::fs::write(dir(&root).join(f), "").unwrap();
        }
        assert!(next_path(&root).ends_with("review-8.md"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_prompt_is_one_line() {
        assert!(!prompt(Path::new("/p/.pitwall/reviews/review-1.md")).contains('\n'));
    }
}
