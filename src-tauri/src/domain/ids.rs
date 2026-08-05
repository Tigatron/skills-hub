use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdParseError {
    #[error("invalid UUID for {entity}: {source}")]
    InvalidUuid {
        entity: &'static str,
        source: uuid::Error,
    },
    #[error("{entity} ID must be UUIDv7, not UUIDv{version}")]
    UnsupportedVersion {
        entity: &'static str,
        version: usize,
    },
}

macro_rules! uuid_v7_id {
    ($name:ident, $entity:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let parsed =
                    Uuid::parse_str(value).map_err(|source| IdParseError::InvalidUuid {
                        entity: $entity,
                        source,
                    })?;
                if parsed.get_version_num() != 7 {
                    return Err(IdParseError::UnsupportedVersion {
                        entity: $entity,
                        version: parsed.get_version_num(),
                    });
                }
                Ok(Self(parsed))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }
    };
}

uuid_v7_id!(SkillId, "Skill");
uuid_v7_id!(ObservationId, "Observation");
uuid_v7_id!(TargetId, "Target");
uuid_v7_id!(DeploymentId, "Deployment");
uuid_v7_id!(OperationId, "Operation");
uuid_v7_id!(SnapshotId, "Snapshot");
uuid_v7_id!(ActivityId, "Activity");
uuid_v7_id!(VaultId, "Vault");
uuid_v7_id!(WorkspaceRootId, "Workspace root");
uuid_v7_id!(ProjectId, "Project");
uuid_v7_id!(ScanRunId, "Scan run");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_uuid_v7_and_round_trip() {
        let id = SkillId::generate();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        assert_eq!(id.to_string().parse::<SkillId>().unwrap(), id);
    }

    #[test]
    fn typed_ids_can_share_wire_uuid_without_implicit_type_conversion() {
        let skill = SkillId::generate();
        let target = TargetId::from_str(&skill.to_string()).unwrap();

        assert_eq!(target.as_uuid(), skill.as_uuid());
    }

    #[test]
    fn parser_and_serde_reject_non_v7_ids() {
        let uuid_v4 = "550e8400-e29b-41d4-a716-446655440000";
        assert!(matches!(
            uuid_v4.parse::<SkillId>(),
            Err(IdParseError::UnsupportedVersion { version: 4, .. })
        ));
        assert!(serde_json::from_str::<SkillId>(&format!("\"{uuid_v4}\"")).is_err());

        let id = SkillId::generate();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<SkillId>(&json).unwrap(), id);
    }
}
