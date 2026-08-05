//! Local, bounded, redacted structured diagnostics.

use fs2::FileExt;
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, open, openat, renameat_with, unlinkat};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use specta::Type;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};
use thiserror::Error;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};
use uuid::Uuid;

use crate::filesystem::durable::{
    OwnedDirectoryIdentity, OwnedFileIdentity, owned_directory_identity,
    owned_file_identity_from_metadata,
};

const TOTAL_BYTE_LIMIT: u64 = 25_000_000;
const SEGMENT_BYTE_LIMIT: u64 = 1_000_000;
const RETENTION_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const EXPORT_TTL: Duration = Duration::from_secs(10 * 60);
const PREVIEW: usize = 16_384;
const MAX_RECORD: usize = 32_768;

type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

#[derive(Clone)]
struct DiagnosticsConfig {
    total_byte_limit: u64,
    segment_byte_limit: u64,
    retention_age: Duration,
    export_ttl: Duration,
    clock: Clock,
}

impl DiagnosticsConfig {
    fn production() -> Self {
        Self {
            total_byte_limit: TOTAL_BYTE_LIMIT,
            segment_byte_limit: SEGMENT_BYTE_LIMIT,
            retention_age: RETENTION_AGE,
            export_ttl: EXPORT_TTL,
            clock: Arc::new(SystemTime::now),
        }
    }
}

#[derive(Debug, Error)]
pub enum DiagnosticsError {
    #[error("diagnostics are unavailable")]
    Unavailable,
    #[error("diagnostics storage is blocked by an unowned entry")]
    ForeignEntry,
    #[error("invalid or expired diagnostics export")]
    InvalidExport,
    #[error("diagnostics export digest does not match")]
    DigestMismatch,
    #[error("diagnostics export destination is invalid or already exists")]
    InvalidDestination,
    #[error("diagnostics I/O failed")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsStatus {
    pub available: bool,
    pub debug_logging: bool,
    pub blocked: bool,
    pub level: String,
    pub health: String,
    pub managed_bytes: String,
    pub segment_count: u32,
    pub dropped_record_count: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExport {
    pub export_id: String,
    pub sha256: String,
    pub record_count: u32,
    pub skipped_count: u32,
    pub byte_count: String,
    pub preview: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsSaveResult {
    pub sha256: String,
    pub byte_count: String,
}

struct Prepared {
    bytes: Vec<u8>,
    digest: String,
    expires: SystemTime,
}
struct Inner {
    root: PathBuf,
    root_directory: File,
    root_identity: OwnedDirectoryIdentity,
    home: Option<PathBuf>,
    debug: AtomicBool,
    dropped_records: AtomicU64,
    prepared: Mutex<HashMap<String, Prepared>>,
    gate: Mutex<()>,
    config: DiagnosticsConfig,
    _lock: File,
}
#[derive(Clone)]
pub struct DiagnosticsService(Arc<Inner>);

impl DiagnosticsService {
    /// Opens the process-local diagnostics store with production retention limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be created or exclusively locked.
    pub fn new(
        root: PathBuf,
        home: Option<PathBuf>,
        debug: bool,
    ) -> Result<Self, DiagnosticsError> {
        Self::with_config(root, home, debug, DiagnosticsConfig::production())
    }

    fn with_config(
        root: PathBuf,
        home: Option<PathBuf>,
        debug: bool,
        config: DiagnosticsConfig,
    ) -> Result<Self, DiagnosticsError> {
        fs::create_dir_all(&root)?;
        let root_metadata = fs::symlink_metadata(&root)?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(DiagnosticsError::ForeignEntry);
        }
        let root_identity = owned_directory_identity(&root)?;
        let root_descriptor = open(
            &root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let lock_path = root.with_extension("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock_exclusive()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let service = Self(Arc::new(Inner {
            root,
            root_directory: File::from(root_descriptor),
            root_identity,
            home,
            debug: AtomicBool::new(debug),
            dropped_records: AtomicU64::new(0),
            prepared: Mutex::new(HashMap::new()),
            gate: Mutex::new(()),
            config,
            _lock: lock,
        }));
        Ok(service)
    }

    #[must_use]
    pub fn layer(&self) -> DiagnosticsLayer {
        DiagnosticsLayer(self.clone())
    }

    pub fn set_debug(&self, enabled: bool) {
        self.0.debug.store(enabled, Ordering::Release);
    }

    /// Returns current local diagnostics health without exposing its filesystem path.
    ///
    /// # Errors
    ///
    /// Returns an error when owned storage cannot be inspected.
    pub fn status(&self) -> Result<DiagnosticsStatus, DiagnosticsError> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let now = self.now();
        let files = match maintain_locked(&self.0, now) {
            Ok(files) => files,
            Err(DiagnosticsError::ForeignEntry) => {
                return Ok(self.status_view(true, &[]));
            }
            Err(error) => return Err(error),
        };
        Ok(self.status_view(false, &files))
    }

    fn status_view(&self, blocked: bool, files: &[Segment]) -> DiagnosticsStatus {
        let debug = self.0.debug.load(Ordering::Acquire);
        DiagnosticsStatus {
            available: true,
            debug_logging: debug,
            blocked,
            level: if debug { "debug" } else { "info" }.to_owned(),
            health: if blocked { "blocked" } else { "healthy" }.to_owned(),
            managed_bytes: files
                .iter()
                .map(|segment| segment.size)
                .sum::<u64>()
                .to_string(),
            segment_count: u32::try_from(files.len()).unwrap_or(u32::MAX),
            dropped_record_count: self.0.dropped_records.load(Ordering::Acquire).to_string(),
        }
    }

    fn now(&self) -> SystemTime {
        (self.0.config.clock)()
    }

    fn append(&self, mut value: Value) -> Result<(), DiagnosticsError> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let now = self.now();
        let mut files = maintain_locked(&self.0, now)?;
        redact_value(&mut value, self.0.home.as_deref(), None);
        let mut bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
        if bytes.len() > MAX_RECORD {
            bytes = serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "timestamp": format_time(now),
                "level": "WARN",
                "target": "skills_hub::diagnostics",
                "message": "diagnostic record truncated"
            }))
            .map_err(io::Error::other)?;
        }
        bytes.push(b'\n');
        let record_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if record_bytes > self.0.config.total_byte_limit {
            self.0.dropped_records.fetch_add(1, Ordering::AcqRel);
            return Ok(());
        }
        make_room(
            &self.0.root_directory,
            &mut files,
            record_bytes,
            self.0.config.total_byte_limit,
        )?;
        let current = files
            .last()
            .filter(|segment| {
                segment.size.saturating_add(record_bytes) <= self.0.config.segment_byte_limit
            })
            .cloned();
        let (current, mut file) = if let Some(current) = current {
            let file = open_segment(&self.0.root_directory, &current, true)?;
            (current.name, file)
        } else {
            ensure_root_owned(&self.0)?;
            let current = new_segment_name(now);
            let descriptor = openat(
                &self.0.root_directory,
                &current,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(io::Error::from)?;
            let file = File::from(descriptor);
            (current, file)
        };
        file.write_all(&bytes)?;
        file.sync_data()?;
        self.0.root_directory.sync_all()?;
        debug_assert!(
            file.metadata()?.len() <= self.0.config.total_byte_limit,
            "diagnostics segment exceeded the total byte bound: {}",
            current.to_string_lossy()
        );
        Ok(())
    }
    /// Freezes exact redacted bytes for user review before saving.
    ///
    /// # Errors
    ///
    /// Returns an error when retention is blocked or managed segments cannot be read.
    pub fn prepare(&self) -> Result<DiagnosticsExport, DiagnosticsError> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let now = self.now();
        let files = maintain_locked(&self.0, now)?;
        let mut bytes = Vec::new();
        let mut records = 0u32;
        let mut skipped = 0u32;
        for segment in files {
            let mut file = open_segment(&self.0.root_directory, &segment, false)?;
            let mut segment_bytes = Vec::new();
            file.read_to_end(&mut segment_bytes)?;
            for line in segment_bytes
                .split(|b| *b == b'\n')
                .filter(|x| !x.is_empty())
            {
                if let Ok(value) = serde_json::from_slice::<Value>(line)
                    && let Some(value) = sanitize_export_record(value, self.0.home.as_deref())
                {
                    bytes.extend_from_slice(&serde_json::to_vec(&value).map_err(io::Error::other)?);
                    bytes.push(b'\n');
                    records = records.saturating_add(1);
                } else {
                    skipped = skipped.saturating_add(1);
                }
            }
        }
        let digest = hex::encode(Sha256::digest(&bytes));
        let id = Uuid::now_v7().to_string();
        let expires = now + self.0.config.export_ttl;
        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(PREVIEW)]).into_owned();
        let mut prepared = self
            .0
            .prepared
            .lock()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        prepared.retain(|_, value| value.expires > now);
        if prepared.len() >= 2 {
            prepared.clear();
        }
        prepared.insert(
            id.clone(),
            Prepared {
                bytes: bytes.clone(),
                digest: digest.clone(),
                expires,
            },
        );
        Ok(DiagnosticsExport {
            export_id: id,
            sha256: digest,
            record_count: records,
            skipped_count: skipped,
            byte_count: bytes.len().to_string(),
            preview,
            expires_at: format_time(expires),
        })
    }
    /// Saves exactly the digest-bound bytes returned by [`Self::prepare`] to a new user-selected
    /// file. Existing destinations are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error for an expired/replaced preparation, digest mismatch, unsafe destination,
    /// collision, durability failure, or post-write verification failure.
    pub fn save(
        &self,
        id: &str,
        expected: &str,
        destination: &Path,
    ) -> Result<DiagnosticsSaveResult, DiagnosticsError> {
        if !destination.is_absolute() {
            return Err(DiagnosticsError::InvalidDestination);
        }
        let mut preparations = self
            .0
            .prepared
            .lock()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let prepared = preparations
            .get(id)
            .ok_or(DiagnosticsError::InvalidExport)?;
        if prepared.expires <= self.now() {
            return Err(DiagnosticsError::InvalidExport);
        }
        if prepared.digest != expected {
            return Err(DiagnosticsError::DigestMismatch);
        }
        let parent = destination
            .parent()
            .ok_or(DiagnosticsError::InvalidDestination)?;
        let parent_descriptor = open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| DiagnosticsError::InvalidDestination)?;
        let parent_directory = File::from(parent_descriptor);
        if !parent_directory.metadata()?.is_dir() {
            return Err(DiagnosticsError::InvalidDestination);
        }
        let file_name = destination
            .file_name()
            .ok_or(DiagnosticsError::InvalidDestination)?
            .to_os_string();
        let temporary = OsString::from(format!(
            ".{}.{}.tmp",
            file_name.to_string_lossy(),
            Uuid::now_v7()
        ));
        let descriptor = openat(
            &parent_directory,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(io::Error::from)?;
        let mut file = File::from(descriptor);
        let temporary_identity = owned_file_identity_from_metadata(&file.metadata()?)?;
        let write_result = file
            .write_all(&prepared.bytes)
            .and_then(|()| file.sync_all());
        if let Err(error) = write_result {
            let _ = remove_owned_file_at(&parent_directory, &temporary, temporary_identity);
            return Err(error.into());
        }
        if let Err(error) =
            rename_file_noreplace_at(&parent_directory, &temporary, file_name.as_os_str())
        {
            let _ = remove_owned_file_at(&parent_directory, &temporary, temporary_identity);
            return Err(error);
        }
        parent_directory.sync_all()?;
        if hex::encode(Sha256::digest(&prepared.bytes)) != prepared.digest
            || !temporary_identity.matches(&file.metadata()?)
        {
            return Err(DiagnosticsError::DigestMismatch);
        }
        let result = DiagnosticsSaveResult {
            sha256: prepared.digest.clone(),
            byte_count: prepared.bytes.len().to_string(),
        };
        preparations.remove(id);
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct Segment {
    path: PathBuf,
    name: OsString,
    size: u64,
    created_at: SystemTime,
    identity: OwnedFileIdentity,
}

fn maintain_locked(inner: &Inner, now: SystemTime) -> Result<Vec<Segment>, DiagnosticsError> {
    ensure_root_owned(inner)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&inner.root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_string = name
            .clone()
            .into_string()
            .map_err(|_| DiagnosticsError::ForeignEntry)?;
        let created_at =
            parse_segment_created_at(&name_string).ok_or(DiagnosticsError::ForeignEntry)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(DiagnosticsError::ForeignEntry);
        }
        if now.duration_since(created_at).unwrap_or_default() >= inner.config.retention_age {
            let identity = owned_file_identity_from_metadata(&metadata)?;
            if !remove_owned_file_at(&inner.root_directory, &name, identity)? {
                return Err(DiagnosticsError::ForeignEntry);
            }
        } else {
            out.push(Segment {
                path,
                name,
                size: metadata.len(),
                created_at,
                identity: owned_file_identity_from_metadata(&metadata)?,
            });
        }
    }
    ensure_root_owned(inner)?;
    out.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    make_room(
        &inner.root_directory,
        &mut out,
        0,
        inner.config.total_byte_limit,
    )?;
    Ok(out)
}

fn make_room(
    root_directory: &File,
    segments: &mut Vec<Segment>,
    required_bytes: u64,
    total_byte_limit: u64,
) -> Result<(), DiagnosticsError> {
    let mut total = segments.iter().map(|segment| segment.size).sum::<u64>();
    while total.saturating_add(required_bytes) > total_byte_limit && !segments.is_empty() {
        let oldest = segments.remove(0);
        if !remove_segment(root_directory, &oldest)? {
            return Err(DiagnosticsError::ForeignEntry);
        }
        total = total.saturating_sub(oldest.size);
    }
    Ok(())
}

fn remove_segment(root_directory: &File, segment: &Segment) -> Result<bool, DiagnosticsError> {
    remove_owned_file_at(root_directory, &segment.name, segment.identity).map_err(Into::into)
}

fn open_segment(
    root_directory: &File,
    segment: &Segment,
    append: bool,
) -> Result<File, DiagnosticsError> {
    let flags = if append {
        OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW
    } else {
        OFlags::RDONLY | OFlags::NOFOLLOW
    };
    let descriptor =
        openat(root_directory, &segment.name, flags, Mode::empty()).map_err(io::Error::from)?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    if !segment.identity.matches(&metadata) || metadata.len() != segment.size {
        return Err(DiagnosticsError::ForeignEntry);
    }
    Ok(file)
}

fn new_segment_name(now: SystemTime) -> OsString {
    let millis = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    OsString::from(format!("segment-{millis:020}-{}.jsonl", Uuid::now_v7()))
}

fn ensure_root_owned(inner: &Inner) -> Result<(), DiagnosticsError> {
    if owned_directory_identity(&inner.root).is_ok_and(|identity| identity == inner.root_identity)
        && inner
            .root_identity
            .matches(&inner.root_directory.metadata()?)
    {
        Ok(())
    } else {
        Err(DiagnosticsError::ForeignEntry)
    }
}

fn parse_segment_created_at(name: &str) -> Option<SystemTime> {
    let body = name.strip_prefix("segment-")?.strip_suffix(".jsonl")?;
    let (millis, uuid) = body.split_once('-')?;
    if millis.len() != 20 || uuid.parse::<Uuid>().is_err() {
        return None;
    }
    let millis = millis.parse::<u64>().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(millis))
}

fn rename_file_noreplace_at(
    parent_directory: &File,
    source: &OsStr,
    destination: &OsStr,
) -> Result<(), DiagnosticsError> {
    renameat_with(
        parent_directory,
        source,
        parent_directory,
        destination,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            DiagnosticsError::InvalidDestination
        } else {
            DiagnosticsError::Io(error.into())
        }
    })
}

fn remove_owned_file_at(
    parent_directory: &File,
    name: &OsStr,
    identity: OwnedFileIdentity,
) -> io::Result<bool> {
    let quarantine = OsString::from(format!(
        ".{}.cleanup-{}",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    match renameat_with(
        parent_directory,
        name,
        parent_directory,
        &quarantine,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(false),
        Err(error) => return Err(io::Error::from(error)),
    }
    let descriptor = match openat(
        parent_directory,
        &quarantine,
        OFlags::RDONLY | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(error) => return Err(io::Error::from(error)),
    };
    let file = File::from(descriptor);
    if !identity.matches(&file.metadata()?) {
        let _ = renameat_with(
            parent_directory,
            &quarantine,
            parent_directory,
            name,
            RenameFlags::NOREPLACE,
        );
        return Ok(false);
    }
    unlinkat(parent_directory, &quarantine, AtFlags::empty()).map_err(io::Error::from)?;
    parent_directory.sync_all()?;
    Ok(true)
}

fn format_time(t: SystemTime) -> String {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .ok()
        .and_then(|x| {
            x.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "unavailable".into())
}
fn sensitive(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
        "credential",
        "environment",
        "content",
        "body",
        "payload",
        "markdown",
    ]
    .iter()
    .any(|x| k.contains(x))
}

fn redact_string(value: &mut String, home: Option<&Path>) {
    if value.len() > 2_048 {
        let mut boundary = 2_048;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
        value.push('…');
    }
    let home = home.map(|path| path.to_string_lossy());
    let has_home = home
        .as_ref()
        .is_some_and(|home| value.contains(home.as_ref()));
    let has_absolute_path = value.starts_with('/')
        || value.contains("file:///")
        || value.contains(" /")
        || value.contains("\"/")
        || value.contains("='/")
        || value.contains("=\"/");
    if has_home || has_absolute_path {
        "[REDACTED_PATH]".clone_into(value);
    }
}

fn sanitize_string_fields(fields: &mut BTreeMap<String, Value>, home: Option<&Path>) {
    for (key, value) in fields {
        if sensitive(key) {
            *value = Value::String("[REDACTED]".to_owned());
            continue;
        }
        let Value::String(string) = value else {
            continue;
        };
        if matches!(string.as_str(), "[REDACTED]" | "[REDACTED_PATH]") {
            continue;
        }
        let allowed = key == "event_code" && known_event_code(string);
        if !allowed {
            let original = string.clone();
            redact_string(string, home);
            if *string == original {
                "[REDACTED]".clone_into(string);
            }
        }
    }
}

fn known_event_code(value: &str) -> bool {
    matches!(
        value,
        "operation_execute_started"
            | "operation_execute_succeeded"
            | "operation_recovery_started"
            | "blocking_work_test"
            | "debug_enabled"
            | "info_sentinel"
            | "rotation_event"
    )
}

fn valid_operation_id(value: &str) -> bool {
    value
        .parse::<Uuid>()
        .is_ok_and(|uuid| uuid.get_version_num() == 7)
}

fn sanitize_export_record(value: Value, home: Option<&Path>) -> Option<Value> {
    let Value::Object(mut source) = value else {
        return None;
    };
    if source.get("schemaVersion")?.as_u64()? != 1 {
        return None;
    }
    let timestamp = source.remove("timestamp")?.as_str()?.to_owned();
    time::OffsetDateTime::parse(&timestamp, &time::format_description::well_known::Rfc3339).ok()?;
    let level = source.remove("level")?.as_str()?.to_owned();
    if !matches!(
        level.as_str(),
        "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"
    ) {
        return None;
    }
    let target = source.remove("target")?.as_str()?.to_owned();
    if !target.starts_with("skills_hub")
        || target.len() > 256
        || !target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return None;
    }
    let message = "[REDACTED]".to_owned();
    let mut fields = source
        .remove("fields")
        .and_then(|fields| fields.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for value in fields.values_mut() {
        if matches!(value, Value::Array(_) | Value::Object(_)) {
            *value = Value::String("[REDACTED]".to_owned());
        }
    }
    sanitize_string_fields(&mut fields, home);
    let mut record = Map::new();
    record.insert("schemaVersion".to_owned(), Value::from(1));
    record.insert("timestamp".to_owned(), Value::String(timestamp));
    record.insert("level".to_owned(), Value::String(level));
    record.insert("target".to_owned(), Value::String(target));
    record.insert("message".to_owned(), Value::String(message));
    record.insert(
        "fields".to_owned(),
        Value::Object(fields.into_iter().collect()),
    );
    if let Some(operation_id) = source
        .remove("operationId")
        .and_then(|id| id.as_str().map(ToOwned::to_owned))
        .filter(|id| valid_operation_id(id))
    {
        record.insert("operationId".to_owned(), Value::String(operation_id));
    }
    Some(Value::Object(record))
}

fn redact_value(v: &mut Value, home: Option<&Path>, key: Option<&str>) {
    if key.is_some_and(sensitive) {
        *v = Value::String("[REDACTED]".into());
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                redact_value(v, home, Some(k));
            }
        }
        Value::Array(a) => {
            for v in a {
                redact_value(v, home, key);
            }
        }
        Value::String(value) => redact_string(value, home),
        _ => {}
    }
}

#[derive(Clone)]
pub struct DiagnosticsLayer(DiagnosticsService);
#[derive(Default)]
struct Visitor {
    fields: BTreeMap<String, Value>,
}
impl Visit for Visitor {
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.fields
            .insert(f.name().into(), Value::String(format!("{v:?}")));
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.fields.insert(f.name().into(), Value::String(v.into()));
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.fields.insert(f.name().into(), Value::Bool(v));
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.fields.insert(f.name().into(), v.into());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.fields.insert(f.name().into(), v.into());
    }
}
impl<S> Layer<S> for DiagnosticsLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with("skills_hub")
            || *meta.level() > tracing::Level::INFO
                && !(self.0.0.debug.load(Ordering::Acquire)
                    && *meta.level() == tracing::Level::DEBUG)
        {
            return;
        }
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        visitor.fields.remove("message");
        sanitize_string_fields(&mut visitor.fields, self.0.0.home.as_deref());
        let operation = ctx.event_scope(event).and_then(|scope| {
            scope
                .from_root()
                .filter_map(|s| s.extensions().get::<OperationMarker>().cloned())
                .last()
        });
        let message = visitor
            .fields
            .get("event_code")
            .cloned()
            .unwrap_or(Value::String(meta.name().into()));
        let mut obj = Map::new();
        obj.insert("schemaVersion".into(), 1.into());
        obj.insert("timestamp".into(), format_time(self.0.now()).into());
        obj.insert("level".into(), meta.level().to_string().into());
        obj.insert("target".into(), meta.target().into());
        obj.insert("message".into(), message);
        obj.insert(
            "fields".into(),
            Value::Object(visitor.fields.into_iter().collect()),
        );
        if let Some(id) = operation.filter(|id| valid_operation_id(&id.0)) {
            obj.insert("operationId".into(), id.0.into());
        }
        let _ = self.0.append(Value::Object(obj));
    }
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut v = Visitor::default();
        attrs.record(&mut v);
        if let Some(Value::String(op)) = v.fields.remove("operationId")
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(OperationMarker(op));
        }
    }
}
#[derive(Clone)]
struct OperationMarker(String);
pub fn operation_span(id: &str) -> tracing::Span {
    tracing::info_span!(target:"skills_hub::operation","operation",operationId=id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    fn test_service(
        root: PathBuf,
        home: Option<PathBuf>,
        now: &Arc<Mutex<SystemTime>>,
    ) -> DiagnosticsService {
        let clock = Arc::clone(now);
        DiagnosticsService::with_config(
            root,
            home,
            false,
            DiagnosticsConfig {
                total_byte_limit: 1_000,
                segment_byte_limit: 300,
                retention_age: Duration::from_secs(60),
                export_ttl: Duration::from_secs(10),
                clock: Arc::new(move || *clock.lock().unwrap()),
            },
        )
        .unwrap()
    }

    #[test]
    fn filters_redacts_correlates_and_exports_immutable_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let service = test_service(
            temp.path().join("diagnostics"),
            Some(temp.path().to_path_buf()),
            &now,
        );
        let operation_id = Uuid::now_v7().to_string();
        let subscriber = tracing_subscriber::registry().with(service.layer());
        tracing::subscriber::with_default(subscriber, || {
            let span = operation_span(&operation_id);
            let _guard = span.enter();
            tracing::debug!(target: "skills_hub::test", "debug-sentinel");
            tracing::info!(target: "skills_hub::test", event_code = "info_sentinel", password = "secret-sentinel", path = %temp.path().join("private" ).display(), content = "markdown-sentinel", "info-sentinel");
            tracing::info!(target: "foreign::test", "foreign-sentinel");
        });
        let prepared = service.prepare().unwrap();
        assert_eq!(prepared.record_count, 1);
        assert!(prepared.preview.contains(&operation_id));
        assert!(prepared.preview.contains("[REDACTED]"));
        assert!(prepared.preview.contains("[REDACTED_PATH]"));
        assert!(!prepared.preview.contains("secret-sentinel"));
        assert!(!prepared.preview.contains("markdown-sentinel"));
        assert!(!prepared.preview.contains("info-sentinel"));
        assert!(!prepared.preview.contains("debug-sentinel"));
        assert!(!prepared.preview.contains("foreign-sentinel"));

        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(service.layer()),
            || tracing::info!(target: "skills_hub::test", "later"),
        );
        let export_directory = temp.path().join("export-directory");
        fs::create_dir(&export_directory).unwrap();
        let linked_directory = temp.path().join("linked-export-directory");
        std::os::unix::fs::symlink(&export_directory, &linked_directory).unwrap();
        assert!(matches!(
            service.save(
                &prepared.export_id,
                &prepared.sha256,
                &linked_directory.join("export.jsonl")
            ),
            Err(DiagnosticsError::InvalidDestination)
        ));
        assert!(!export_directory.join("export.jsonl").exists());
        let destination = temp.path().join("export.jsonl");
        let saved = service
            .save(&prepared.export_id, &prepared.sha256, &destination)
            .unwrap();
        assert_eq!(saved.sha256, prepared.sha256);
        assert!(!fs::read_to_string(destination).unwrap().contains("later"));
    }

    #[test]
    fn foreign_entries_block_without_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("diagnostics");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("foreign.txt"), b"preserve").unwrap();
        let service = DiagnosticsService::new(root.clone(), None, false).unwrap();
        let status = service.status().unwrap();
        assert!(status.available);
        assert!(status.blocked);
        assert_eq!(status.health, "blocked");
        assert_eq!(fs::read(root.join("foreign.txt")).unwrap(), b"preserve");
    }

    #[test]
    fn tiny_limits_rotate_prune_by_age_and_never_exceed_total_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let service = test_service(temporary.path().join("diagnostics"), None, &now);
        let subscriber = tracing_subscriber::registry().with(service.layer());
        tracing::subscriber::with_default(subscriber, || {
            for index in 0..20 {
                tracing::info!(target: "skills_hub::rotation", index, note = "bounded-record", "rotation-event");
            }
        });
        let status = service.status().unwrap();
        assert!(status.segment_count > 1);
        assert!(status.managed_bytes.parse::<u64>().unwrap() <= 1_000);

        *now.lock().unwrap() += Duration::from_secs(61);
        let expired = service.status().unwrap();
        assert_eq!(expired.segment_count, 0);
        assert_eq!(expired.managed_bytes, "0");
    }

    #[test]
    fn debug_opt_in_and_export_expiry_use_the_injected_clock() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let service = test_service(temporary.path().join("diagnostics"), None, &now);
        service.set_debug(true);
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(service.layer()),
            || tracing::debug!(target: "skills_hub::test", event_code = "debug_enabled", "debug-enabled"),
        );
        let prepared = service.prepare().unwrap();
        assert!(prepared.preview.contains("debug_enabled"));
        *now.lock().unwrap() += Duration::from_secs(11);
        assert!(matches!(
            service.save(
                &prepared.export_id,
                &prepared.sha256,
                &temporary.path().join("expired.jsonl")
            ),
            Err(DiagnosticsError::InvalidExport)
        ));
    }

    #[test]
    fn inspected_segment_replacement_is_never_opened_or_deleted() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let service = test_service(temporary.path().join("diagnostics"), None, &now);
        let name = new_segment_name(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        let path = service.0.root.join(&name);
        fs::write(&path, b"owned").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let inspected = Segment {
            path: path.clone(),
            name,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            size: metadata.len(),
            identity: owned_file_identity_from_metadata(&metadata).unwrap(),
        };
        let displaced = service.0.root.join("displaced");
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"replacement").unwrap();

        assert!(matches!(
            open_segment(&service.0.root_directory, &inspected, false),
            Err(DiagnosticsError::ForeignEntry)
        ));
        assert!(
            !remove_owned_file_at(
                &service.0.root_directory,
                &inspected.name,
                inspected.identity
            )
            .unwrap()
        );
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(fs::read(&displaced).unwrap(), b"owned");
    }

    #[test]
    fn oversized_multibyte_values_are_redacted_without_panicking() {
        let mut value = "💥".repeat(700);
        redact_string(&mut value, None);
        assert!(value.ends_with('…'));
        assert!(value.len() <= 2_051);
    }

    #[test]
    fn export_reconstructs_untrusted_disk_records_with_fail_closed_redaction() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let service = test_service(temporary.path().join("diagnostics"), None, &now);
        let name = new_segment_name(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        fs::write(
            service.0.root.join(name),
            br#"{"schemaVersion":1,"timestamp":"1970-01-01T00:01:40Z","level":"INFO","target":"skills_hub::legacy","message":"privateSkillContent","fields":{"note":"skill-secret","error":"credential-detail","password":123456,"nested":{"content":"nested-secret"},"items":["array-secret"],"event_code":"operation_execute_started"},"operationId":"operation-safe","unknown":"top-secret"}
"#,
        )
        .unwrap();

        let export = service.prepare().unwrap();
        assert_eq!(export.record_count, 1);
        assert!(export.preview.contains("operation_execute_started"));
        assert!(!export.preview.contains("operation-safe"));
        for secret in [
            "privateSkillContent",
            "skill-secret",
            "credential-detail",
            "123456",
            "nested-secret",
            "array-secret",
            "top-secret",
        ] {
            assert!(!export.preview.contains(secret));
        }
    }

    #[test]
    fn replaced_diagnostics_root_blocks_without_writing_to_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        ));
        let root = temporary.path().join("diagnostics");
        let service = test_service(root.clone(), None, &now);
        let displaced = temporary.path().join("diagnostics-displaced");
        fs::rename(&root, &displaced).unwrap();
        fs::create_dir(&root).unwrap();

        assert!(matches!(
            service.append(json!({"event_code": "must_not_write"})),
            Err(DiagnosticsError::ForeignEntry)
        ));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert!(service.status().unwrap().blocked);
    }
}
