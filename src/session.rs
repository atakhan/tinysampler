//! Persist session: library index + the one open document's sample-file map.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::model::Project;
use crate::persist::{self, LibraryMeta, SampleFileMap};

pub struct PersistSession {
    pub root: PathBuf,
    pub items: Vec<LibraryMeta>,
    pub next_project_id: u64,
    pub open_id: Option<u64>,
    sample_files: SampleFileMap,
    pub dirty: bool,
    last_persist: Instant,
}

impl PersistSession {
    pub fn load(root: PathBuf) -> (Self, Option<String>) {
        let (items, next_project_id, err) = persist::load_library_index(&root);
        (
            Self {
                root,
                items,
                next_project_id,
                open_id: None,
                sample_files: SampleFileMap::new(),
                dirty: false,
                last_persist: Instant::now(),
            },
            err,
        )
    }

    pub fn library_items(&self) -> &[LibraryMeta] {
        &self.items
    }

    fn refresh_open_meta(&mut self, project: &Project) {
        let Some(id) = self.open_id else {
            return;
        };
        if let Some(item) = self.items.iter_mut().find(|e| e.id == id) {
            item.name = project.name.clone();
            item.track_count = project.tracks.len();
        }
    }

    pub fn save_open(&mut self, project: &Project) -> Result<(), String> {
        let Some(id) = self.open_id else {
            return persist::save_index(&self.root, self.next_project_id);
        };
        persist::save_project(&self.root, id, project, &mut self.sample_files)?;
        self.refresh_open_meta(project);
        persist::save_index(&self.root, self.next_project_id)?;
        self.dirty = false;
        self.last_persist = Instant::now();
        Ok(())
    }

    pub fn create_and_open(&mut self, project: &Project) -> Result<u64, String> {
        let id = self.next_project_id;
        self.next_project_id = self.next_project_id.saturating_add(1);
        self.sample_files.clear();
        persist::save_project(&self.root, id, project, &mut self.sample_files)?;
        persist::save_index(&self.root, self.next_project_id)?;
        self.items.push(LibraryMeta {
            id,
            name: project.name.clone(),
            track_count: project.tracks.len(),
        });
        self.open_id = Some(id);
        self.dirty = false;
        self.last_persist = Instant::now();
        Ok(id)
    }

    pub fn open(&mut self, id: u64) -> Result<(Project, Vec<String>), String> {
        let loaded = persist::load_project(&self.root, id)?;
        self.open_id = Some(id);
        self.sample_files = loaded.sample_files;
        self.dirty = false;
        self.last_persist = Instant::now();
        Ok((loaded.project, loaded.warnings))
    }

    pub fn close_without_save(&mut self) {
        self.open_id = None;
        self.sample_files.clear();
        self.dirty = false;
    }

    pub fn persist_if_due(&mut self, project: &Project, dragging: bool) -> Result<bool, String> {
        if self.open_id.is_none() || !self.dirty || dragging {
            return Ok(false);
        }
        if self.last_persist.elapsed() < Duration::from_millis(2000) {
            return Ok(false);
        }
        self.save_open(project)?;
        Ok(true)
    }

    pub fn mark_dirty(&mut self) {
        if self.open_id.is_some() {
            self.dirty = true;
        }
    }
}
