//! Watch events are unreliable hints; this module only schedules reconciliation.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InvalidationKind {
    PossibleChange,
    CoverageLost,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invalidation {
    pub path: PathBuf,
    pub kind: InvalidationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReconcileReason {
    Startup,
    Resume,
    Wake,
    Overflow,
    OperationFinished,
    OperationRolledBack,
    RootReplaced,
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WatchEvent {
    Paths(Vec<PathBuf>),
    CoverageLost(PathBuf),
    Overflow,
    Disconnected,
}

pub(crate) trait WatchBackend {
    type Error;
    fn watch(&mut self, root: &Path) -> Result<(), Self::Error>;
    fn unwatch(&mut self, root: &Path) -> Result<(), Self::Error>;
    fn try_event(&mut self) -> Option<WatchEvent>;
}

pub(crate) struct NotifyBackend {
    watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<notify::Event>>,
}

impl NotifyBackend {
    pub fn new() -> notify::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        Ok(Self { watcher, receiver })
    }
}

impl WatchBackend for NotifyBackend {
    type Error = notify::Error;
    fn watch(&mut self, root: &Path) -> Result<(), Self::Error> {
        self.watcher.watch(root, RecursiveMode::Recursive)
    }
    fn unwatch(&mut self, root: &Path) -> Result<(), Self::Error> {
        self.watcher.unwatch(root)
    }
    fn try_event(&mut self) -> Option<WatchEvent> {
        match self.receiver.try_recv() {
            Ok(Ok(event)) if event.need_rescan() || matches!(event.kind, EventKind::Other) => {
                Some(WatchEvent::Overflow)
            }
            Ok(Ok(event)) => Some(WatchEvent::Paths(event.paths)),
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => Some(WatchEvent::Disconnected),
            Err(TryRecvError::Empty) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReconcileRequest {
    Targeted(Vec<Invalidation>),
    BoundedFull {
        boundaries: Vec<PathBuf>,
        reasons: Vec<ReconcileReason>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct WatchCoordinator {
    boundaries: BTreeSet<PathBuf>,
    pending: BTreeMap<PathBuf, InvalidationKind>,
    full_reasons: BTreeSet<ReconcileReason>,
}

impl WatchCoordinator {
    pub fn replace_boundaries(&mut self, boundaries: impl IntoIterator<Item = PathBuf>) {
        self.boundaries = boundaries
            .into_iter()
            .map(|path| normalize(&path))
            .collect();
        self.pending
            .retain(|boundary, _| self.boundaries.contains(boundary));
    }
    #[cfg(test)]
    fn register(&mut self, boundary: &Path) {
        self.boundaries.insert(normalize(boundary));
    }
    pub fn proactive(&mut self, reason: ReconcileReason) {
        self.full_reasons.insert(reason);
    }
    pub fn boundaries(&self) -> Vec<PathBuf> {
        self.boundaries.iter().cloned().collect()
    }
    pub fn ingest(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Paths(paths) => {
                for path in paths {
                    self.invalidate(&path, InvalidationKind::PossibleChange);
                }
            }
            WatchEvent::CoverageLost(path) => {
                self.invalidate(&path, InvalidationKind::CoverageLost);
            }
            WatchEvent::Overflow => {
                self.full_reasons.insert(ReconcileReason::Overflow);
            }
            WatchEvent::Disconnected => {
                for boundary in self.boundaries.clone() {
                    self.invalidate(&boundary, InvalidationKind::Disconnected);
                }
                self.full_reasons.insert(ReconcileReason::Disconnect);
            }
        }
    }
    pub fn drain(&mut self) -> Option<ReconcileRequest> {
        if !self.full_reasons.is_empty() {
            self.pending.clear();
            return Some(ReconcileRequest::BoundedFull {
                boundaries: self.boundaries.iter().cloned().collect(),
                reasons: std::mem::take(&mut self.full_reasons).into_iter().collect(),
            });
        }
        if self.pending.is_empty() {
            return None;
        }
        Some(ReconcileRequest::Targeted(
            std::mem::take(&mut self.pending)
                .into_iter()
                .map(|(path, kind)| Invalidation { path, kind })
                .collect(),
        ))
    }
    pub fn requeue(&mut self, request: ReconcileRequest) {
        match request {
            ReconcileRequest::Targeted(invalidations) => {
                for invalidation in invalidations {
                    self.pending
                        .entry(invalidation.path)
                        .and_modify(|old| *old = (*old).max(invalidation.kind))
                        .or_insert(invalidation.kind);
                }
            }
            ReconcileRequest::BoundedFull { reasons, .. } => {
                self.full_reasons.extend(reasons);
            }
        }
    }
    fn invalidate(&mut self, path: &Path, kind: InvalidationKind) {
        let normalized = normalize(path);
        let Some(boundary) = self
            .boundaries
            .iter()
            .filter(|root| normalized.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
        else {
            return;
        };
        self.pending
            .entry(boundary)
            .and_modify(|old| *old = (*old).max(kind))
            .or_insert(kind);
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_reordered_and_rename_storm_coalesce_to_smallest_boundary() {
        let mut c = WatchCoordinator::default();
        c.register(Path::new("/w"));
        c.register(Path::new("/w/project"));
        for p in ["/w/project/a", "/w/project/b", "/w/project/a", "/w/other"] {
            c.ingest(WatchEvent::Paths(vec![p.into()]));
        }
        let Some(ReconcileRequest::Targeted(x)) = c.drain() else {
            panic!()
        };
        assert_eq!(
            x.iter().map(|i| i.path.clone()).collect::<Vec<_>>(),
            vec![PathBuf::from("/w"), PathBuf::from("/w/project")]
        );
    }
    #[test]
    fn loss_strengthens_repeated_hint() {
        let mut c = WatchCoordinator::default();
        c.register(Path::new("/w"));
        c.ingest(WatchEvent::CoverageLost("/w/x".into()));
        c.ingest(WatchEvent::Paths(vec!["/w/y".into()]));
        let Some(ReconcileRequest::Targeted(x)) = c.drain() else {
            panic!()
        };
        assert_eq!(x[0].kind, InvalidationKind::CoverageLost);
    }
    #[test]
    fn drop_disconnect_overflow_and_wake_force_bounded_full() {
        for (event, reason) in [
            (WatchEvent::Overflow, ReconcileReason::Overflow),
            (WatchEvent::Disconnected, ReconcileReason::Disconnect),
        ] {
            let mut c = WatchCoordinator::default();
            c.register(Path::new("/w"));
            c.ingest(WatchEvent::Paths(vec!["/w/x".into()]));
            c.ingest(event);
            let Some(ReconcileRequest::BoundedFull {
                boundaries,
                reasons,
            }) = c.drain()
            else {
                panic!()
            };
            assert_eq!(boundaries, vec![PathBuf::from("/w")]);
            assert_eq!(reasons, vec![reason]);
        }
        let mut c = WatchCoordinator::default();
        c.proactive(ReconcileReason::Wake);
        c.proactive(ReconcileReason::Wake);
        assert!(
            matches!(c.drain(),Some(ReconcileRequest::BoundedFull{reasons,..}) if reasons==vec![ReconcileReason::Wake])
        );
    }
    #[test]
    fn startup_resume_and_operations_are_proactive_even_without_events() {
        let mut c = WatchCoordinator::default();
        for reason in [
            ReconcileReason::Startup,
            ReconcileReason::Resume,
            ReconcileReason::OperationFinished,
            ReconcileReason::OperationRolledBack,
            ReconcileReason::RootReplaced,
        ] {
            c.proactive(reason);
        }
        assert!(
            matches!(c.drain(),Some(ReconcileRequest::BoundedFull{reasons,..}) if reasons.len()==5)
        );
    }

    #[test]
    fn failed_reconciliation_can_be_requeued_without_losing_strength() {
        let mut c = WatchCoordinator::default();
        c.register(Path::new("/w"));
        c.ingest(WatchEvent::CoverageLost("/w/project".into()));
        let request = c.drain().unwrap();
        c.requeue(request);
        assert!(matches!(
            c.drain(),
            Some(ReconcileRequest::Targeted(items))
                if items[0].kind == InvalidationKind::CoverageLost
        ));
    }
}
