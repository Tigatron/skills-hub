//! Agent target descriptors and registry.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use crate::domain::AdapterId;

pub(crate) const UNIVERSAL_GLOBAL_SOURCE_ID: &str = "universal-agent-skills@1:global-default";

static UNIVERSAL_ADAPTER_ID: LazyLock<AdapterId> = LazyLock::new(|| {
    AdapterId::new("universal-agent-skills", 1).expect("static Universal adapter ID is valid")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlobalAdapterRoot {
    pub adapter_id: AdapterId,
    pub source_root_id: &'static str,
    pub display_name: &'static str,
    pub root: PathBuf,
}

#[must_use]
pub(crate) fn universal_global_root(home: &Path) -> GlobalAdapterRoot {
    GlobalAdapterRoot {
        adapter_id: UNIVERSAL_ADAPTER_ID.clone(),
        source_root_id: UNIVERSAL_GLOBAL_SOURCE_ID,
        display_name: "Universal Agent Skills",
        root: home.join(".agents/skills"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universal_fixture_has_a_stable_id_and_rust_expanded_global_path() {
        let root = universal_global_root(Path::new("/Users/example"));

        assert_eq!(root.adapter_id.to_string(), "universal-agent-skills@1");
        assert_eq!(root.source_root_id, UNIVERSAL_GLOBAL_SOURCE_ID);
        assert_eq!(root.root, Path::new("/Users/example/.agents/skills"));
    }
}
