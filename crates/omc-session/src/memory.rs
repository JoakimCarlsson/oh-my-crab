//! The memories of a project, listed without opening a conversation.

use std::path::{Path, PathBuf};

use omc_memory::{Memories, Memory, Project, Store};
use omc_wire::{MemoryEntry, MemoryListing};

/// The memories a conversation in `folder`, or in the current folder when there is none, would start with: those of its project, which every linked worktree of the repository shares, and the user's, read from the store [`Store::locate`] finds.
///
/// # Errors
///
/// Returns what to tell the client when there is no current folder or the memory files cannot be read.
pub fn memory(folder: Option<PathBuf>) -> Result<MemoryListing, String> {
    let directory = match folder {
        Some(folder) => folder,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    listing(
        &directory,
        Store::locate(),
        !omc_memory::disabled_by_environment(),
    )
}

/// The memories of the project `directory` belongs to and of the user in `store`, said to be `enabled` when there is a store; a listing with no memories when there is none.
///
/// # Errors
///
/// Returns what to tell the client when the memory files cannot be read.
fn listing(directory: &Path, store: Option<Store>, enabled: bool) -> Result<MemoryListing, String> {
    let project = Project::of(directory);
    let mut listing = MemoryListing {
        enabled: enabled && store.is_some(),
        name: project.name(),
        project: project.root.display().to_string(),
        ..MemoryListing::default()
    };
    let Some(store) = store else {
        return Ok(listing);
    };
    let memories = Memories::new(&store, project, None);
    listing.project_folder = shown(memories.shelf(omc_memory::Scope::Project).folder());
    listing.user_folder = shown(memories.shelf(omc_memory::Scope::User).folder());
    listing.memories = memories
        .all()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(entry)
        .collect();
    Ok(listing)
}

/// `path` as text.
fn shown(path: &Path) -> String {
    path.display().to_string()
}

/// `memory` as a client reads it.
fn entry(memory: Memory) -> MemoryEntry {
    MemoryEntry {
        scope: memory.scope.as_str().to_owned(),
        id: memory.id,
        text: memory.text,
        tags: memory.tags,
        created: memory.created,
        conversation: memory.conversation,
        updated: memory.updated,
        updated_in: memory.updated_in,
    }
}

#[cfg(test)]
/// Tests for listing memories.
mod tests {
    use super::*;

    /// A listing holds the project's memories and the user's, with where they are kept, and none without a store.
    #[test]
    fn lists_project_and_user_memories() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/memory-listing");
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let memories = Memories::new(&store, Project::of(directory), Some("c9".to_owned()));
        memories
            .remember(
                "Project fact.",
                &["t".to_owned()],
                omc_memory::Scope::Project,
            )
            .unwrap();
        memories
            .remember("User fact.", &[], omc_memory::Scope::User)
            .unwrap();
        let listed = listing(directory, Some(store), true).unwrap();
        assert!(listed.enabled);
        assert_eq!(listed.memories.len(), 2);
        assert_eq!(
            (
                listed.memories[0].scope.as_str(),
                listed.memories[0].text.as_str()
            ),
            ("project", "Project fact.")
        );
        assert_eq!(listed.memories[0].conversation.as_deref(), Some("c9"));
        assert_eq!(listed.memories[1].scope, "user");
        assert!(listed.project_folder.contains("projects"));
        let json = serde_json::to_value(&listed).unwrap();
        assert_eq!(json["memories"][0]["tags"][0], "t");
        let without = listing(directory, None, true).unwrap();
        assert!(!without.enabled && without.memories.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
