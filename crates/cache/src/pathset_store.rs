use std::path::{Path, PathBuf};

pub struct PathsetStore {
    pub _dir: PathBuf,
}

impl PathsetStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            _dir: dir.to_path_buf(),
        }
    }
}
