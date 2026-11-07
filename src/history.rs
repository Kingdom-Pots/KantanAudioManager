use crate::project::{EditError, Project};

struct Entry {
    label: &'static str,
    project: Project,
}

/// A project plus its undo and redo stacks.
///
/// This is the only way to change a project, so every edit is undoable. Each
/// entry is a full snapshot, which is cheap because audio is shared.
pub struct Document {
    project: Project,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    revision: u64,
}

impl Document {
    pub fn new(project: Project) -> Self {
        Self { project, undo: Vec::new(), redo: Vec::new(), revision: 0 }
    }

    /// Changes whenever the project does, through an edit, undo or redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Runs one undoable edit. If it fails, the project and history are left
    /// untouched, even when it had already made partial changes.
    pub fn edit<T>(
        &mut self,
        label: &'static str,
        apply: impl FnOnce(&mut Project) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let mut next = self.project.clone();
        let result = apply(&mut next)?;
        let project = std::mem::replace(&mut self.project, next);
        self.undo.push(Entry { label, project });
        self.redo.clear();
        self.revision += 1;
        Ok(result)
    }

    /// Returns the label of the edit that was undone.
    pub fn undo(&mut self) -> Option<&'static str> {
        let entry = self.undo.pop()?;
        let project = std::mem::replace(&mut self.project, entry.project);
        self.redo.push(Entry { label: entry.label, project });
        self.revision += 1;
        Some(entry.label)
    }

    /// Returns the label of the edit that was redone.
    pub fn redo(&mut self) -> Option<&'static str> {
        let entry = self.redo.pop()?;
        let project = std::mem::replace(&mut self.project, entry.project);
        self.undo.push(Entry { label: entry.label, project });
        self.revision += 1;
        Some(entry.label)
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|e| e.label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|e| e.label)
    }
}
