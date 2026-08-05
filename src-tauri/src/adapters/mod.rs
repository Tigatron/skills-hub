//! Data-driven agent target descriptors and path expansion.

use std::path::{Path, PathBuf};

use serde::Serialize;
use specta::Type;

use crate::domain::AdapterId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdapterScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdapterMode {
    Symlink,
    ManagedCopy,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdapterDescriptor {
    pub name: &'static str,
    pub version: u16,
    pub display_name: &'static str,
    pub global_path: &'static str,
    pub project_path: &'static str,
    pub official_source_url: &'static str,
    pub caveats: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdapterDescriptorView {
    pub adapter_id: String,
    pub display_name: String,
    pub platform: String,
    pub global_path: String,
    pub project_path: String,
    pub scopes: Vec<AdapterScope>,
    pub supported_modes: Vec<AdapterMode>,
    pub official_source_url: String,
    pub verified_at: String,
    pub confidence: String,
    pub caveats: String,
}

pub(crate) const DESCRIPTORS: [AdapterDescriptor; 6] = [
    AdapterDescriptor {
        name: "universal-agent-skills",
        version: 1,
        display_name: "Universal Agent Skills",
        global_path: ".agents/skills",
        project_path: ".agents/skills",
        official_source_url: "https://agentskills.io/client-implementation/adding-skills-support",
        caveats: "These locations are a convention; the Agent Skills specification does not mandate them.",
    },
    AdapterDescriptor {
        name: "claude-code",
        version: 1,
        display_name: "Claude Code",
        global_path: ".claude/skills",
        project_path: ".claude/skills",
        official_source_url: "https://code.claude.com/docs/en/skills",
        caveats: "Claude supports symlink folders and has nested/project precedence rules.",
    },
    AdapterDescriptor {
        name: "openai-codex",
        version: 1,
        display_name: "OpenAI Codex",
        global_path: ".agents/skills",
        project_path: ".agents/skills",
        official_source_url: "https://developers.openai.com/codex/skills/",
        caveats: "Current paths intentionally overlap Universal; Codex scans project ancestors to the repository root.",
    },
    AdapterDescriptor {
        name: "cursor",
        version: 1,
        display_name: "Cursor",
        global_path: ".cursor/skills",
        project_path: ".cursor/skills",
        official_source_url: "https://cursor.com/docs/context/skills",
        caveats: "Cursor also reads compatibility paths and supports broader recursive/nested discovery than this manager's one-level scan.",
    },
    AdapterDescriptor {
        name: "gemini-cli",
        version: 1,
        display_name: "Gemini CLI",
        global_path: ".gemini/skills",
        project_path: ".gemini/skills",
        official_source_url: "https://geminicli.com/docs/cli/skills/",
        caveats: "Gemini also reads .agents aliases, which take precedence.",
    },
    AdapterDescriptor {
        name: "opencode",
        version: 1,
        display_name: "OpenCode",
        global_path: ".config/opencode/skills",
        project_path: ".opencode/skills",
        official_source_url: "https://opencode.ai/docs/skills",
        caveats: "OpenCode also reads Claude/.agents compatibility paths and walks ancestors to the Git worktree.",
    },
];

impl AdapterDescriptor {
    pub(crate) fn id(self) -> AdapterId {
        AdapterId::new(self.name, self.version).expect("static adapter ID")
    }
    pub(crate) fn source_id(self) -> String {
        format!("{}:global-default", self.id())
    }
    pub(crate) fn global_root(self, home: &Path) -> GlobalAdapterRoot {
        GlobalAdapterRoot {
            adapter_id: self.id(),
            source_root_id: self.source_id(),
            display_name: self.display_name.to_owned(),
            root: home.join(self.global_path),
        }
    }
    fn view(self) -> AdapterDescriptorView {
        AdapterDescriptorView {
            adapter_id: self.id().to_string(),
            display_name: self.display_name.to_owned(),
            platform: "macos".to_owned(),
            global_path: format!("~/{}", self.global_path),
            project_path: self.project_path.to_owned(),
            scopes: vec![AdapterScope::Global, AdapterScope::Project],
            supported_modes: vec![AdapterMode::Symlink, AdapterMode::ManagedCopy],
            official_source_url: self.official_source_url.to_owned(),
            verified_at: "2026-08-05".to_owned(),
            confidence: "verified".to_owned(),
            caveats: self.caveats.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlobalAdapterRoot {
    pub adapter_id: AdapterId,
    pub source_root_id: String,
    pub display_name: String,
    pub root: PathBuf,
}

pub(crate) fn descriptors() -> Vec<AdapterDescriptorView> {
    DESCRIPTORS
        .into_iter()
        .map(AdapterDescriptor::view)
        .collect()
}

pub(crate) fn universal_global_root(home: &Path) -> GlobalAdapterRoot {
    DESCRIPTORS[0].global_root(home)
}
pub(crate) fn global_roots(home: &Path) -> Vec<GlobalAdapterRoot> {
    DESCRIPTORS
        .into_iter()
        .map(|d| d.global_root(home))
        .collect()
}
pub(crate) fn is_known(id: &AdapterId) -> bool {
    DESCRIPTORS.into_iter().any(|d| d.id() == *id)
}
pub(crate) fn descriptor(id: &AdapterId) -> Option<AdapterDescriptor> {
    DESCRIPTORS.into_iter().find(|d| d.id() == *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn six_descriptors_expand_with_stable_unique_sources() {
        let roots = global_roots(Path::new("/Users/example"));
        assert_eq!(roots.len(), 6);
        let ids = roots
            .iter()
            .map(|r| &r.source_root_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 6);
        assert_eq!(roots[2].root, Path::new("/Users/example/.agents/skills"));
        assert_eq!(descriptors()[2].adapter_id, "openai-codex@1");
        assert_eq!(
            descriptors()
                .iter()
                .map(|descriptor| descriptor.adapter_id.as_str())
                .collect::<Vec<_>>(),
            [
                "universal-agent-skills@1",
                "claude-code@1",
                "openai-codex@1",
                "cursor@1",
                "gemini-cli@1",
                "opencode@1",
            ]
        );
        assert_eq!(descriptors()[2].global_path, "~/.agents/skills");
        assert_eq!(descriptors()[2].project_path, ".agents/skills");
        serde_json::to_string(&descriptors()).unwrap();
    }
}
