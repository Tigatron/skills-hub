use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

pub const BUNDLE_DIGEST_PREFIX: &str = "sha256-bundle-v1:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleDigest([u8; 32]);

impl BundleDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for BundleDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{BUNDLE_DIGEST_PREFIX}{}", hex::encode(self.0))
    }
}

impl FromStr for BundleDigest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let encoded = value
            .strip_prefix(BUNDLE_DIGEST_PREFIX)
            .ok_or(DigestParseError::UnsupportedSchema)?;
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DigestParseError::InvalidHex);
        }

        let decoded = hex::decode(encoded).map_err(|_| DigestParseError::InvalidHex)?;
        let bytes = decoded
            .try_into()
            .map_err(|_| DigestParseError::InvalidHex)?;
        Ok(Self(bytes))
    }
}

impl Serialize for BundleDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BundleDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DigestParseError {
    #[error("unsupported bundle digest schema")]
    UnsupportedSchema,
    #[error("bundle digest must contain 64 lowercase hexadecimal characters")]
    InvalidHex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(BundleDigest);

impl RevisionId {
    #[must_use]
    pub const fn from_digest(digest: BundleDigest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(self) -> BundleDigest {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_requires_schema_and_lowercase_hex() {
        let digest = BundleDigest::from_bytes([0xab; 32]);
        let encoded = digest.to_string();
        assert_eq!(encoded.len(), BUNDLE_DIGEST_PREFIX.len() + 64);
        assert_eq!(encoded.parse::<BundleDigest>().unwrap(), digest);
        assert_eq!(
            encoded.to_uppercase().parse::<BundleDigest>(),
            Err(DigestParseError::UnsupportedSchema)
        );
    }

    #[test]
    fn digest_serializes_as_qualified_string() {
        let digest = BundleDigest::from_bytes([7; 32]);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(serde_json::from_str::<BundleDigest>(&json).unwrap(), digest);
        assert!(json.contains(BUNDLE_DIGEST_PREFIX));
    }
}
