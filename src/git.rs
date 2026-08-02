//! Reads the checked-out branch straight from a repository's `HEAD` file.
//!
//! Claude Code reports a branch only for the worktrees it manages itself, and
//! the status line runs on every message, so costsee resolves the branch of an
//! ordinary session by reading `HEAD` rather than spawning `git`.

use std::path::{Path, PathBuf};

/// The branch checked out in `directory`, or the short commit when `HEAD` is
/// detached. Returns `None` outside a repository.
pub fn branch(directory: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(directory)?.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(reference) => {
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            (!name.is_empty()).then(|| name.to_string())
        }
        // A detached HEAD holds the commit itself, which only helps abbreviated.
        None => {
            let commit: String = head.chars().take(7).collect();
            (commit.len() == 7 && commit.chars().all(|char| char.is_ascii_hexdigit()))
                .then_some(commit)
        }
    }
}

/// The nearest `.git`, which is a directory in a normal checkout and a file
/// pointing at the real directory inside a worktree or submodule.
fn git_dir(directory: &Path) -> Option<PathBuf> {
    directory.ancestors().find_map(|ancestor| {
        let candidate = ancestor.join(".git");
        let metadata = std::fs::metadata(&candidate).ok()?;
        if metadata.is_dir() {
            Some(candidate)
        } else {
            linked_git_dir(ancestor, &candidate)
        }
    })
}

fn linked_git_dir(base: &Path, pointer: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(pointer).ok()?;
    let target = Path::new(contents.trim().strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn repository(head: &str) -> tempfile::TempDir {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join(".git")).unwrap();
        fs::write(directory.path().join(".git/HEAD"), head).unwrap();
        directory
    }

    #[test]
    fn reads_the_branch_from_the_nearest_repository() {
        let repository = repository("ref: refs/heads/feature/status-line\n");
        let nested = repository.path().join("src/statusline");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            branch(&nested).as_deref(),
            Some("feature/status-line"),
            "a nested directory should still find the repository root"
        );
    }

    #[test]
    fn abbreviates_a_detached_head_and_ignores_anything_else() {
        let detached = repository("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b\n");
        assert_eq!(branch(detached.path()).as_deref(), Some("1a2b3c4"));

        let unusable = repository("ref: \n");
        assert_eq!(branch(unusable.path()), None);

        let truncated = repository("1a2b3\n");
        assert_eq!(branch(truncated.path()), None);
    }

    #[test]
    fn follows_the_gitdir_pointer_a_worktree_leaves_behind() {
        let repository = repository("ref: refs/heads/main\n");
        let linked = repository.path().join(".git/worktrees/review");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("HEAD"), "ref: refs/heads/review\n").unwrap();

        let absolute = tempdir().unwrap();
        fs::write(
            absolute.path().join(".git"),
            format!("gitdir: {}\n", linked.display()),
        )
        .unwrap();
        assert_eq!(branch(absolute.path()).as_deref(), Some("review"));

        let relative = repository.path().join("checkout");
        fs::create_dir(&relative).unwrap();
        fs::write(relative.join(".git"), "gitdir: ../.git/worktrees/review\n").unwrap();
        assert_eq!(branch(&relative).as_deref(), Some("review"));
    }

    #[test]
    fn a_directory_outside_a_repository_has_no_branch() {
        let directory = tempdir().unwrap();

        assert_eq!(branch(directory.path()), None);
    }
}
