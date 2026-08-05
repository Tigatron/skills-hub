use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub struct TestLayout {
    root: tempfile::TempDir,
    pub home: PathBuf,
    pub vault: PathBuf,
    pub project: PathBuf,
}

impl TestLayout {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let home = root.path().join("home");
        let vault = root.path().join("vault");
        let project = root.path().join("project");

        for path in [&home, &vault, &project] {
            fs::create_dir(path).expect("fixture directory");
        }

        Self {
            root,
            home,
            vault,
            project,
        }
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }
}

#[derive(Default)]
pub struct Failpoints {
    enabled: BTreeSet<(String, usize)>,
}

impl Failpoints {
    pub fn enable(&mut self, name: impl Into<String>, step: usize) {
        self.enabled.insert((name.into(), step));
    }

    pub fn is_enabled(&self, name: &str, step: usize) -> bool {
        self.enabled.contains(&(name.to_owned(), step))
    }
}
