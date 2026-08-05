use std::{fmt, path::Path, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

const DANGEROUS_COMPONENT_CHARACTERS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeploymentName {
    spelling: String,
    collision_key: String,
}

impl DeploymentName {
    /// Parses one safe target directory name while retaining its accepted spelling.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] when the value is not one safe UTF-8 path component.
    pub fn parse(value: impl Into<String>) -> Result<Self, NameError> {
        let spelling = value.into();
        validate_component(&spelling)?;
        let collision_key = normalized_collision_key(&spelling);
        Ok(Self {
            spelling,
            collision_key,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.spelling
    }

    #[must_use]
    pub fn collision_key(&self) -> &str {
        &self.collision_key
    }

    #[must_use]
    pub fn collision_key_for(&self, case_sensitivity: PathCaseSensitivity) -> String {
        match case_sensitivity {
            PathCaseSensitivity::Sensitive => self.spelling.nfc().collect(),
            PathCaseSensitivity::Insensitive => self.collision_key.clone(),
        }
    }

    #[must_use]
    pub fn collides_with(&self, other: &Self, case_sensitivity: PathCaseSensitivity) -> bool {
        self.collision_key_for(case_sensitivity) == other.collision_key_for(case_sensitivity)
    }
}

impl fmt::Display for DeploymentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.spelling.fmt(formatter)
    }
}

impl Serialize for DeploymentName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.spelling)
    }
}

impl<'de> Deserialize<'de> for DeploymentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleRelativePath(String);

impl BundleRelativePath {
    /// Parses a slash-delimited, normalized path relative to a Bundle root.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] for absolute, empty, escaping, or unsafe components.
    pub fn parse(value: &str) -> Result<Self, NameError> {
        if value.is_empty() {
            return Err(NameError::Empty);
        }
        if value.starts_with('/') {
            return Err(NameError::AbsolutePath);
        }
        let mut normalized = Vec::new();
        for component in value.split('/') {
            validate_component(component)?;
            normalized.push(component.nfc().collect::<String>());
        }
        Ok(Self(normalized.join("/")))
    }

    /// Converts a platform path without using lossy Unicode conversion.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] when the path is absolute, unsafe, or not valid UTF-8.
    pub fn from_path(path: &Path) -> Result<Self, NameError> {
        if path.is_absolute() {
            return Err(NameError::AbsolutePath);
        }
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(value) => {
                    let value = value.to_str().ok_or(NameError::UnsupportedName)?;
                    validate_component(value)?;
                    components.push(value.nfc().collect::<String>());
                }
                std::path::Component::CurDir => return Err(NameError::CurrentDirectory),
                std::path::Component::ParentDir => return Err(NameError::ParentDirectory),
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(NameError::AbsolutePath);
                }
            }
        }
        if components.is_empty() {
            return Err(NameError::Empty);
        }
        Ok(Self(components.join("/")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }

    #[must_use]
    pub fn collision_key(&self) -> String {
        self.collision_key_for(PathCaseSensitivity::Insensitive)
    }

    #[must_use]
    pub fn collision_key_for(&self, case_sensitivity: PathCaseSensitivity) -> String {
        self.0
            .split('/')
            .map(|component| match case_sensitivity {
                PathCaseSensitivity::Sensitive => component.nfc().collect(),
                PathCaseSensitivity::Insensitive => normalized_collision_key(component),
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    #[must_use]
    pub fn collides_with(&self, other: &Self, case_sensitivity: PathCaseSensitivity) -> bool {
        self.collision_key_for(case_sensitivity) == other.collision_key_for(case_sensitivity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCaseSensitivity {
    Sensitive,
    Insensitive,
}

impl fmt::Display for BundleRelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for BundleRelativePath {
    type Err = NameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for BundleRelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BundleRelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(&String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdapterId {
    name: String,
    schema_version: u16,
}

impl AdapterId {
    /// Creates a stable adapter identifier with a positive schema version.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] when the name or schema version is invalid.
    pub fn new(name: impl Into<String>, schema_version: u16) -> Result<Self, NameError> {
        let name = name.into();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(NameError::InvalidAdapterName);
        }
        if schema_version == 0 {
            return Err(NameError::InvalidAdapterVersion);
        }
        Ok(Self {
            name,
            schema_version,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

impl fmt::Display for AdapterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.name, self.schema_version)
    }
}

impl FromStr for AdapterId {
    type Err = NameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, version) = value
            .rsplit_once('@')
            .ok_or(NameError::InvalidAdapterName)?;
        let version = version
            .parse()
            .map_err(|_| NameError::InvalidAdapterVersion)?;
        Self::new(name, version)
    }
}

impl Serialize for AdapterId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AdapterId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error("'.' is not a valid name component")]
    CurrentDirectory,
    #[error("'..' is not a valid name component")]
    ParentDirectory,
    #[error("absolute paths are not allowed")]
    AbsolutePath,
    #[error("name is not valid UTF-8")]
    UnsupportedName,
    #[error("name contains a control or dangerous character")]
    DangerousCharacter,
    #[error("adapter name must use lowercase ASCII letters, digits, or hyphens")]
    InvalidAdapterName,
    #[error("adapter schema version must be a positive integer")]
    InvalidAdapterVersion,
}

pub fn validate_component(value: &str) -> Result<(), NameError> {
    if value.is_empty() {
        return Err(NameError::Empty);
    }
    if value == "." {
        return Err(NameError::CurrentDirectory);
    }
    if value == ".." {
        return Err(NameError::ParentDirectory);
    }
    if value.chars().any(|character| {
        character.is_control() || DANGEROUS_COMPONENT_CHARACTERS.contains(&character)
    }) {
        return Err(NameError::DangerousCharacter);
    }
    Ok(())
}

#[must_use]
pub fn normalized_collision_key(value: &str) -> String {
    value.nfc().case_fold().nfc().collect()
}

/// Normalizes a persisted path identity without erasing case distinctions on case-sensitive
/// volumes. Collision checks must use [`normalized_collision_key`] separately.
#[must_use]
pub fn normalized_path_identity(value: &str) -> String {
    value.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_key_uses_nfc_and_full_case_folding() {
        assert_eq!(
            normalized_collision_key("Straße"),
            normalized_collision_key("STRASSE")
        );
        assert_eq!(
            normalized_collision_key("Cafe\u{301}"),
            normalized_collision_key("CAFÉ")
        );
    }

    #[test]
    fn path_identity_normalizes_unicode_without_folding_case() {
        assert_eq!(
            normalized_path_identity("Cafe\u{301}"),
            normalized_path_identity("Café")
        );
        assert_ne!(
            normalized_path_identity("/skills/Alpha"),
            normalized_path_identity("/skills/alpha")
        );
    }

    #[test]
    fn deployment_name_preserves_spelling_but_exposes_collision_key() {
        let name = DeploymentName::parse("My-Skill").unwrap();
        let lowercase = DeploymentName::parse("my-skill").unwrap();
        assert_eq!(name.as_str(), "My-Skill");
        assert_eq!(name.collision_key(), "my-skill");
        assert_eq!(
            name.collision_key_for(PathCaseSensitivity::Sensitive),
            "My-Skill"
        );
        assert_eq!(
            name.collision_key_for(PathCaseSensitivity::Insensitive),
            "my-skill"
        );
        assert!(!name.collides_with(&lowercase, PathCaseSensitivity::Sensitive));
        assert!(name.collides_with(&lowercase, PathCaseSensitivity::Insensitive));
    }

    #[test]
    fn bundle_relative_path_rejects_escaping_and_empty_components() {
        assert_eq!(
            BundleRelativePath::parse("/absolute"),
            Err(NameError::AbsolutePath)
        );
        assert_eq!(
            BundleRelativePath::parse("../secret"),
            Err(NameError::ParentDirectory)
        );
        assert_eq!(BundleRelativePath::parse("a//b"), Err(NameError::Empty));
        assert_eq!(
            BundleRelativePath::parse("a/./b"),
            Err(NameError::CurrentDirectory)
        );
        assert!(BundleRelativePath::parse(".hidden/SKILL.md").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn path_parser_rejects_non_utf8_without_lossy_conversion() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::PathBuf};

        let path = PathBuf::from(OsStr::from_bytes(b"bad-\xff"));
        assert_eq!(
            BundleRelativePath::from_path(&path),
            Err(NameError::UnsupportedName)
        );
    }

    #[test]
    fn adapter_id_round_trips_stable_string() {
        let adapter: AdapterId = "claude-code@1".parse().unwrap();
        assert_eq!(adapter.name(), "claude-code");
        assert_eq!(adapter.schema_version(), 1);
        assert_eq!(adapter.to_string(), "claude-code@1");
        assert_eq!(
            serde_json::to_string(&adapter).unwrap(),
            "\"claude-code@1\""
        );
        assert_eq!(
            serde_json::from_str::<AdapterId>("\"claude-code@1\"").unwrap(),
            adapter
        );
    }
}
