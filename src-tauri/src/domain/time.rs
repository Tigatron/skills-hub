use std::{
    fmt,
    time::{Duration, Instant},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTimestamp(OffsetDateTime);

impl UtcTimestamp {
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Constructs a UTC timestamp from the `SQLite` projection representation.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::OutOfRange`] when the value cannot be represented.
    pub fn from_unix_millis(value: i64) -> Result<Self, TimeError> {
        let nanos = i128::from(value) * 1_000_000;
        OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .map(Self)
            .map_err(|_| TimeError::OutOfRange)
    }

    /// Parses and normalizes an RFC3339 timestamp to UTC.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::InvalidRfc3339`] when parsing fails.
    pub fn parse_rfc3339(value: &str) -> Result<Self, TimeError> {
        OffsetDateTime::parse(value, &Rfc3339)
            .map(|value| Self(value.to_offset(time::UtcOffset::UTC)))
            .map_err(|_| TimeError::InvalidRfc3339)
    }

    /// Returns the timestamp's `SQLite` projection in UTC Unix milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::OutOfRange`] when the value cannot fit in an `i64`.
    pub fn unix_millis(self) -> Result<i64, TimeError> {
        i64::try_from(self.0.unix_timestamp_nanos() / 1_000_000).map_err(|_| TimeError::OutOfRange)
    }

    /// Adds a durable millisecond duration without overflowing.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::OutOfRange`] when the result is not representable.
    pub fn checked_add(self, duration: DurationMillis) -> Result<Self, TimeError> {
        let duration = time::Duration::milliseconds(
            i64::try_from(duration.0).map_err(|_| TimeError::OutOfRange)?,
        );
        self.0
            .checked_add(duration)
            .map(Self)
            .ok_or(TimeError::OutOfRange)
    }

    fn rfc3339(self) -> Result<String, TimeError> {
        self.0
            .format(&Rfc3339)
            .map_err(|_| TimeError::InvalidRfc3339)
    }
}

impl fmt::Display for UtcTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.rfc3339().map_err(|_| fmt::Error)?)
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.rfc3339().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse_rfc3339(&String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationMillis(pub u64);

impl From<Duration> for DurationMillis {
    fn from(value: Duration) -> Self {
        Self(value.as_millis().try_into().unwrap_or(u64::MAX))
    }
}

impl TryFrom<DurationMillis> for Duration {
    type Error = TimeError;

    fn try_from(value: DurationMillis) -> Result<Self, Self::Error> {
        Ok(Self::from_millis(value.0))
    }
}

#[derive(Debug, Clone)]
pub struct MonotonicTimer(Instant);

impl MonotonicTimer {
    #[must_use]
    pub fn start() -> Self {
        Self(Instant::now())
    }

    #[must_use]
    pub fn elapsed(&self) -> DurationMillis {
        self.0.elapsed().into()
    }

    #[must_use]
    pub fn has_elapsed(&self, timeout: DurationMillis) -> bool {
        self.0.elapsed() >= Duration::from_millis(timeout.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TimeError {
    #[error("timestamp is outside the supported range")]
    OutOfRange,
    #[error("timestamp is not valid RFC3339 UTC")]
    InvalidRfc3339,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_uses_rfc3339_json_and_unix_milliseconds() {
        let timestamp = UtcTimestamp::from_unix_millis(1_721_234_567_890).unwrap();
        let json = serde_json::to_string(&timestamp).unwrap();
        assert!(json.ends_with("Z\""));
        assert_eq!(
            serde_json::from_str::<UtcTimestamp>(&json).unwrap(),
            timestamp
        );
        assert_eq!(timestamp.unix_millis().unwrap(), 1_721_234_567_890);
    }

    #[test]
    fn elapsed_time_uses_a_monotonic_timer() {
        let timer = MonotonicTimer::start();
        assert!(timer.has_elapsed(DurationMillis(0)));
        assert!(timer.elapsed().0 < 1_000);
    }
}
