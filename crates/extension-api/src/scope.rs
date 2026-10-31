//! Workspace, root and activation identities with generations (design §5.1).
//!
//! Handles carry the scope they were issued under. A handle is valid only
//! while every generation it captured is still current, so a removed and
//! re-added root, a remounted workspace or a restarted activation can never
//! revive an old handle.
use crate::error::{ErrorKind, ExtensionError};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootId(pub u64);

/// The scope a handle, request or operation was issued under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeToken {
    pub workspace: WorkspaceId,
    /// `None` for commands that are not tied to a root.
    pub root: Option<(RootId, u64)>,
    pub root_set_generation: u64,
    pub activation_generation: u64,
}

#[derive(Default)]
struct WorkspaceScope {
    roots: BTreeMap<RootId, u64>,
    root_set_generation: u64,
    activation_generation: u64,
}

/// Source of identities and the authority on which tokens are still current.
#[derive(Default)]
pub struct ScopeRegistry {
    next_workspace: u64,
    next_root: u64,
    next_root_generation: u64,
    workspaces: BTreeMap<WorkspaceId, WorkspaceScope>,
}

fn expired(message: &str) -> ExtensionError {
    ExtensionError::new(ErrorKind::ExpiredHandle, message)
}

impl ScopeRegistry {
    /// Every mount gets a new id; a remount never reuses the old one.
    pub fn mount_workspace(&mut self) -> WorkspaceId {
        self.next_workspace += 1;
        let id = WorkspaceId(self.next_workspace);
        self.workspaces.insert(
            id,
            WorkspaceScope {
                activation_generation: 1,
                ..WorkspaceScope::default()
            },
        );
        id
    }

    pub fn unmount_workspace(&mut self, workspace: WorkspaceId) {
        self.workspaces.remove(&workspace);
    }

    pub fn add_root(&mut self, workspace: WorkspaceId) -> Option<(RootId, u64)> {
        let scope = self.workspaces.get_mut(&workspace)?;
        self.next_root += 1;
        self.next_root_generation += 1;
        let root = RootId(self.next_root);
        scope.roots.insert(root, self.next_root_generation);
        scope.root_set_generation += 1;
        Some((root, self.next_root_generation))
    }

    /// Removing a root invalidates its handles and bumps the root-set generation,
    /// while tokens of other roots stay valid.
    pub fn remove_root(&mut self, workspace: WorkspaceId, root: RootId) -> bool {
        let Some(scope) = self.workspaces.get_mut(&workspace) else {
            return false;
        };
        let removed = scope.roots.remove(&root).is_some();
        if removed {
            scope.root_set_generation += 1;
        }
        removed
    }

    /// Nesting or ownership changed without the root itself changing.
    pub fn touch_root_set(&mut self, workspace: WorkspaceId) {
        if let Some(scope) = self.workspaces.get_mut(&workspace) {
            scope.root_set_generation += 1;
        }
    }

    /// Stop, reload or failure-rebuild of the extension instance.
    pub fn bump_activation(&mut self, workspace: WorkspaceId) -> Option<u64> {
        let scope = self.workspaces.get_mut(&workspace)?;
        scope.activation_generation += 1;
        Some(scope.activation_generation)
    }

    pub fn token(&self, workspace: WorkspaceId, root: Option<RootId>) -> Option<ScopeToken> {
        let scope = self.workspaces.get(&workspace)?;
        let root = match root {
            Some(id) => Some((id, *scope.roots.get(&id)?)),
            None => None,
        };
        Some(ScopeToken {
            workspace,
            root,
            root_set_generation: scope.root_set_generation,
            activation_generation: scope.activation_generation,
        })
    }

    /// Whether `token` may still be used for an operation on its own root.
    pub fn validate(&self, token: &ScopeToken) -> Result<(), ExtensionError> {
        let scope = self
            .workspaces
            .get(&token.workspace)
            .ok_or_else(|| expired("the workspace was closed"))?;
        if scope.activation_generation != token.activation_generation {
            return Err(expired("the extension was restarted"));
        }
        if let Some((root, generation)) = token.root {
            if scope.roots.get(&root) != Some(&generation) {
                return Err(expired("the root was removed or replaced"));
            }
        }
        Ok(())
    }

    /// Stricter check for operations whose result depends on the whole root set
    /// (cross-root lookup, document ownership, configuration resolution).
    pub fn validate_root_set(&self, token: &ScopeToken) -> Result<(), ExtensionError> {
        self.validate(token)?;
        let scope = &self.workspaces[&token.workspace];
        if scope.root_set_generation != token.root_set_generation {
            return Err(ExtensionError::new(
                ErrorKind::StaleSnapshot,
                "the set of roots changed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_root_a_does_not_affect_root_b() {
        let mut registry = ScopeRegistry::default();
        let ws = registry.mount_workspace();
        let (a, _) = registry.add_root(ws).unwrap();
        let (b, _) = registry.add_root(ws).unwrap();
        let token_a = registry.token(ws, Some(a)).unwrap();
        let token_b = registry.token(ws, Some(b)).unwrap();
        assert!(registry.remove_root(ws, a));
        assert_eq!(
            registry.validate(&token_a).unwrap_err().kind,
            ErrorKind::ExpiredHandle
        );
        assert!(registry.validate(&token_b).is_ok());
        assert_eq!(
            registry.validate_root_set(&token_b).unwrap_err().kind,
            ErrorKind::StaleSnapshot
        );
    }

    #[test]
    fn a_re_added_root_does_not_revive_old_tokens() {
        let mut registry = ScopeRegistry::default();
        let ws = registry.mount_workspace();
        let (a, _) = registry.add_root(ws).unwrap();
        let old = registry.token(ws, Some(a)).unwrap();
        registry.remove_root(ws, a);
        let (a_again, _) = registry.add_root(ws).unwrap();
        assert_ne!(a, a_again);
        assert!(registry.validate(&old).is_err());
        assert!(registry.token(ws, Some(a)).is_none());
    }

    #[test]
    fn remounting_a_workspace_gets_a_new_identity() {
        let mut registry = ScopeRegistry::default();
        let first = registry.mount_workspace();
        let token = registry.token(first, None).unwrap();
        registry.unmount_workspace(first);
        let second = registry.mount_workspace();
        assert_ne!(first, second);
        assert!(registry.validate(&token).is_err());
    }

    #[test]
    fn restarting_the_activation_invalidates_handles() {
        let mut registry = ScopeRegistry::default();
        let ws = registry.mount_workspace();
        let token = registry.token(ws, None).unwrap();
        registry.bump_activation(ws);
        assert_eq!(
            registry.validate(&token).unwrap_err().kind,
            ErrorKind::ExpiredHandle
        );
    }

    #[test]
    fn nesting_changes_invalidate_root_set_dependent_work_only() {
        let mut registry = ScopeRegistry::default();
        let ws = registry.mount_workspace();
        let (a, _) = registry.add_root(ws).unwrap();
        let token = registry.token(ws, Some(a)).unwrap();
        registry.touch_root_set(ws);
        assert!(registry.validate(&token).is_ok());
        assert!(registry.validate_root_set(&token).is_err());
    }
}
