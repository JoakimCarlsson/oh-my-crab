//! A compact outline of the working directory, for the system prompt, so the agent need not list folders before it starts.
//!
//! The walk honours `.gitignore`, `.ignore` and git's exclude files whether or not the folder is in a repository, leaves out hidden entries and the folders of version control, builds and vendored packages, and is bounded in depth, entries and time: a walk that runs too long gives no outline rather than holding up the conversation.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ignore::{DirEntry, WalkBuilder};

/// Folders never walked into or shown, whatever the ignore files say.
const SKIPPED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    "target",
    "build",
    "dist",
    "node_modules",
    "bower_components",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".gradle",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    ".direnv",
    ".terraform",
    ".dart_tool",
    "Pods",
    "zig-cache",
    "zig-out",
];

/// How far the outline goes and how long it may take to make.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// Deepest level shown; the working directory's own entries are level 1.
    depth: usize,
    /// Most entries shown for the working directory itself.
    root_entries: usize,
    /// Most entries shown for any folder below it.
    entries: usize,
    /// Most lines in the outline, beyond the working directory's own entries, which are always shown.
    lines: usize,
    /// Most entries one walk may visit before it is tried again one level shallower.
    walked: usize,
    /// Longest the whole walk may take.
    time: Duration,
}

/// The limits of the outline in the system prompt.
const LIMITS: Limits = Limits {
    depth: 3,
    root_entries: 25,
    entries: 15,
    lines: 120,
    walked: 20_000,
    time: Duration::from_millis(300),
};

/// One file or folder found by the walk.
#[derive(Debug)]
struct Node {
    /// Its name, with control characters replaced so that it stays on one line.
    name: String,
    /// Whether it is a folder.
    dir: bool,
    /// Its entries, as indices into the walk's nodes, folders first and then by name.
    children: Vec<usize>,
}

/// How a walk ended.
enum Walk {
    /// It finished; the working directory is the first node.
    Done(Vec<Node>),
    /// It found more entries than [`Limits::walked`].
    TooMany,
    /// It ran past its deadline.
    OutOfTime,
}

/// The outline of `directory` for the system prompt: one entry to a line, folders first and ending in `/`, each level indented two spaces further than its parent.
///
/// It goes three levels deep and is at most about 120 lines long. A folder with many entries shows the first few and a line counting the rest; when the whole tree would be longer, the deepest levels are left out, a level at a time and in order from the top, so the upper levels are always whole. The text is empty when the folder is empty or cannot be read, or when the walk takes longer than 300 milliseconds.
pub fn workspace_tree(directory: &Path) -> String {
    outline(directory, LIMITS)
}

/// The outline of `directory` within `limits`, walked on a thread of its own so that a folder that hangs, such as an unresponsive network mount, cannot hold the caller past [`Limits::time`]; empty when the walk does not finish in time.
fn outline(directory: &Path, limits: Limits) -> String {
    let deadline = Instant::now() + limits.time;
    let (sender, receiver) = mpsc::channel();
    let root = directory.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("workspace-tree".to_owned())
        .spawn(move || {
            let _ = sender.send(scan(&root, limits, deadline));
        });
    if spawned.is_err() {
        return String::new();
    }
    match receiver.recv_timeout(limits.time) {
        Ok(Some(nodes)) => render(&nodes, limits),
        _ => String::new(),
    }
}

/// The nodes of `root` walked to the deepest level within `limits` that holds no more than [`Limits::walked`] entries, or nothing when even one level is too many or `deadline` passes first.
fn scan(root: &Path, limits: Limits, deadline: Instant) -> Option<Vec<Node>> {
    for depth in (1..=limits.depth).rev() {
        match walk(root, depth, limits.walked, deadline) {
            Walk::Done(nodes) => return Some(nodes),
            Walk::TooMany => continue,
            Walk::OutOfTime => return None,
        }
    }
    None
}

/// Walks `root` down to `depth` levels, giving up past `walked` entries or after `deadline`; entries that cannot be read are left out.
fn walk(root: &Path, depth: usize, walked: usize, deadline: Instant) -> Walk {
    let mut nodes = vec![Node {
        name: String::new(),
        dir: true,
        children: Vec::new(),
    }];
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    index.insert(PathBuf::new(), 0);
    let entries = WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(depth))
        .filter_entry(|entry| entry.depth() == 0 || !skipped(entry))
        .build();
    for entry in entries.flatten() {
        if Instant::now() > deadline {
            return Walk::OutOfTime;
        }
        if entry.depth() == 0 {
            continue;
        }
        if nodes.len() > walked {
            return Walk::TooMany;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(&parent) = relative.parent().and_then(|parent| index.get(parent)) else {
            continue;
        };
        let dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        let id = nodes.len();
        nodes.push(Node {
            name: printable(&entry.file_name().to_string_lossy()),
            dir,
            children: Vec::new(),
        });
        nodes[parent].children.push(id);
        if dir {
            index.insert(relative.to_path_buf(), id);
        }
    }
    for id in 0..nodes.len() {
        let mut children = std::mem::take(&mut nodes[id].children);
        children.sort_by(|&a, &b| {
            nodes[b]
                .dir
                .cmp(&nodes[a].dir)
                .then_with(|| nodes[a].name.cmp(&nodes[b].name))
        });
        nodes[id].children = children;
    }
    Walk::Done(nodes)
}

/// Whether `entry` is a folder the outline never shows, by its name.
fn skipped(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|kind| kind.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIPPED.contains(&name))
}

/// `name` with every control character, line breaks among them, replaced by `?`.
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The entries of node `id` that are shown, and how many are left out, when the node is at `level` (the working directory being level 0).
fn shown(nodes: &[Node], id: usize, level: usize, limits: Limits) -> (&[usize], usize) {
    let cap = if level == 0 {
        limits.root_entries
    } else {
        limits.entries
    };
    let children = &nodes[id].children;
    let kept = children.len().min(cap);
    (&children[..kept], children.len() - kept)
}

/// The lines node `id` at `level` adds when its entries are shown: one for each shown entry and one for the count of the rest.
fn cost(nodes: &[Node], id: usize, level: usize, limits: Limits) -> usize {
    let (kept, dropped) = shown(nodes, id, level, limits);
    kept.len() + usize::from(dropped > 0)
}

/// Which folders show their entries: the working directory always, then folders a level at a time, from the top and in the order they are shown, while their entries fit in [`Limits::lines`]; the first that does not fit stops it, so no folder opens below a level left unfinished.
fn expanded(nodes: &[Node], limits: Limits) -> Vec<bool> {
    let mut open = vec![false; nodes.len()];
    open[0] = true;
    let mut used = cost(nodes, 0, 0, limits);
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    let (kept, _) = shown(nodes, 0, 0, limits);
    queue.extend(kept.iter().map(|&child| (child, 1)));
    while let Some((id, level)) = queue.pop_front() {
        if !nodes[id].dir {
            continue;
        }
        let lines = cost(nodes, id, level, limits);
        if used + lines > limits.lines.max(used) {
            break;
        }
        used += lines;
        open[id] = true;
        let (kept, _) = shown(nodes, id, level, limits);
        queue.extend(kept.iter().map(|&child| (child, level + 1)));
    }
    open
}

/// The outline of the walked `nodes` within `limits`; empty when the working directory has no entries.
fn render(nodes: &[Node], limits: Limits) -> String {
    let open = expanded(nodes, limits);
    let mut out = String::new();
    write_entries(nodes, 0, 0, &open, limits, &mut out);
    out.truncate(out.trim_end().len());
    out
}

/// Appends to `out` the shown entries of node `id` at `level`, each folder in `open` followed by its own, and the count of what is left out.
fn write_entries(
    nodes: &[Node],
    id: usize,
    level: usize,
    open: &[bool],
    limits: Limits,
    out: &mut String,
) {
    let indent = "  ".repeat(level);
    let (kept, dropped) = shown(nodes, id, level, limits);
    for &child in kept {
        let node = &nodes[child];
        out.push_str(&indent);
        out.push_str(&node.name);
        if node.dir {
            out.push('/');
        }
        out.push('\n');
        if open[child] {
            write_entries(nodes, child, level + 1, open, limits, out);
        }
    }
    if dropped > 0 {
        let rest = &nodes[id].children[kept.len()..];
        let noun = match (rest.iter().all(|&child| !nodes[child].dir), dropped) {
            (true, 1) => "file",
            (true, _) => "files",
            (false, 1) => "entry",
            (false, _) => "entries",
        };
        out.push_str(&format!("{indent}… {dropped} more {noun}\n"));
    }
}

#[cfg(test)]
/// Tests for the workspace outline.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Limits small enough to reach in a test.
    const SMALL: Limits = Limits {
        depth: 3,
        root_entries: 4,
        entries: 3,
        lines: 120,
        walked: 1_000,
        time: Duration::from_secs(10),
    };

    /// Folders come first and end in `/`, levels are indented, and ignored, hidden and build folders are left out, in a folder that is not a repository.
    #[test]
    fn outlines_respecting_ignores() {
        let base = scratch("tree-basic");
        put(&base, ".gitignore", "*.log\nsecret/\n");
        put(&base, ".ignore", "scratch.txt\n");
        put(&base, "README.md", "");
        put(&base, "debug.log", "");
        put(&base, "scratch.txt", "");
        put(&base, "secret/key", "");
        put(&base, "src/main.rs", "");
        put(&base, "src/deep/a/b/too-deep.rs", "");
        put(&base, "target/debug/x", "");
        put(&base, "node_modules/pkg/index.js", "");
        put(&base, ".hidden/file", "");
        assert_eq!(
            outline(&base, SMALL),
            "src/\n  deep/\n    a/\n  main.rs\nREADME.md"
        );
    }

    /// A git repository's own folder is never shown, and its ignore files are read.
    #[test]
    fn skips_the_repository_folder() {
        let base = scratch("tree-git");
        put(&base, ".git/HEAD", "ref: refs/heads/main\n");
        put(&base, ".gitignore", "out/\n");
        put(&base, "out/a", "");
        put(&base, "lib.rs", "");
        assert_eq!(outline(&base, SMALL), "lib.rs");
    }

    /// A folder with too many entries shows the first ones, folders first, and counts the rest.
    #[test]
    fn cuts_long_folders() {
        let base = scratch("tree-wide");
        for name in ["e", "d", "c", "b", "a"] {
            put(&base, &format!("pkg/{name}.rs"), "");
        }
        put(&base, "pkg/sub/x.rs", "");
        put(&base, "pkg/more/y.rs", "");
        for name in ["1", "2", "3", "4", "5", "6"] {
            put(&base, &format!("{name}.txt"), "");
        }
        assert_eq!(
            outline(&base, SMALL),
            "pkg/\n  more/\n    y.rs\n  sub/\n    x.rs\n  a.rs\n  … 4 more files\n1.txt\n2.txt\n3.txt\n… 3 more files"
        );
    }

    /// Past the line budget deeper levels are dropped first, so the upper levels stay whole.
    #[test]
    fn shrinks_from_the_bottom() {
        let base = scratch("tree-budget");
        for top in ["a", "b"] {
            for inner in ["x", "y"] {
                put(&base, &format!("{top}/{inner}/file.rs"), "");
            }
        }
        let limits = Limits { lines: 6, ..SMALL };
        assert_eq!(outline(&base, limits), "a/\n  x/\n  y/\nb/\n  x/\n  y/");
        let limits = Limits { lines: 7, ..SMALL };
        assert_eq!(
            outline(&base, limits),
            "a/\n  x/\n    file.rs\n  y/\nb/\n  x/\n  y/"
        );
        let limits = Limits { lines: 3, ..SMALL };
        assert_eq!(outline(&base, limits), "a/\nb/");
    }

    /// A walk that finds too many entries is tried again shallower, and one out of time gives no outline.
    #[test]
    fn bounds_the_walk() {
        let base = scratch("tree-bounds");
        for name in ["a", "b", "c"] {
            put(&base, &format!("{name}/inner/file.rs"), "");
        }
        let limits = Limits { walked: 4, ..SMALL };
        assert_eq!(outline(&base, limits), "a/\nb/\nc/");
        let limits = Limits {
            time: Duration::ZERO,
            ..SMALL
        };
        assert_eq!(outline(&base, limits), "");
    }

    /// An empty or missing folder has no outline, and a hidden or build-named working directory is still walked.
    #[test]
    fn empty_and_odd_roots() {
        let base = scratch("tree-roots");
        assert_eq!(outline(&base.join("missing"), SMALL), "");
        std::fs::create_dir_all(base.join("empty")).unwrap();
        assert_eq!(outline(&base.join("empty"), SMALL), "");
        put(&base, ".config/build/settings.toml", "");
        assert_eq!(outline(&base.join(".config/build"), SMALL), "settings.toml");
    }

    /// Names with line breaks stay on one line.
    #[test]
    fn names_stay_on_one_line() {
        assert_eq!(printable("a\nb\tc"), "a?b?c");
    }
}
