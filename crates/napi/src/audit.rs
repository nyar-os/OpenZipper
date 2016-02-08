//! Audit trail at the N-API capability boundary.
//!
//! Every exported capability and every GUI IPC call is recorded here, including
//! dry runs, failures and capability refusals. Schema, redaction, rotation and
//! querying come from the shared audit contract; this module only supplies
//! OpenZipper's capability names and error codes.

use std::sync::{Arc, PoisonError, RwLock};

use open_zipper_core::Result as CoreResult;
use serde_json::Value;
use wae::audit::{Actor, Auditor, Entry, Mode, Query};

const APP: &str = "open-zipper";

static AUDITOR: RwLock<Option<Arc<Auditor>>> = RwLock::new(None);

#[derive(Debug, Clone)]
pub struct Summary {
    pub count: u64,
    pub summary: String,
}

impl Summary {
    pub fn new(count: u64, summary: impl Into<String>) -> Self {
        Self { count, summary: summary.into() }
    }
}

fn auditor() -> Arc<Auditor> {
    if let Some(existing) = AUDITOR.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
        return Arc::clone(existing);
    }
    let built = Arc::new(Auditor::from_env(APP, env!("CARGO_PKG_VERSION")));
    let mut guard = AUDITOR.write().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(guard.get_or_insert(built))
}

pub fn configure(actor: Actor, session: Option<String>, host: Option<String>) {
    let mut builder = Auditor::builder(APP, env!("CARGO_PKG_VERSION")).actor(actor);
    if let Some(session) = session {
        builder = builder.session(session);
    }
    if let Some(host) = host {
        builder = builder.host(host);
    }
    let configured = Arc::new(builder.build());
    let actor = configured.actor();
    let params = serde_json::json!({
        "actorKind": actor.kind,
        "actorId": actor.id,
        "actorSource": actor.source,
        "session": configured.session(),
        "host": configured.host(),
    });
    let _ = run_with(&configured, "audit-configure", Mode::DryRun, params, || Ok(((), Summary::new(0, "audit configuration updated"))));
    *AUDITOR.write().unwrap_or_else(PoisonError::into_inner) = Some(configured);
}

pub fn run<T>(capability: &str, mode: Mode, params: Value, body: impl FnOnce() -> CoreResult<(T, Summary)>) -> CoreResult<T> {
    let auditor = auditor();
    run_with(&auditor, capability, mode, params, body)
}

fn run_with<T>(
    auditor: &Auditor,
    capability: &str,
    mode: Mode,
    params: Value,
    body: impl FnOnce() -> CoreResult<(T, Summary)>,
) -> CoreResult<T> {
    let call = auditor.call(capability, mode).params(params);
    match body() {
        Ok((value, summary)) => {
            let _ = call.targets(summary.count, summary.summary).succeed();
            Ok(value)
        }
        Err(error) => {
            let code = error.code().to_string();
            if error.capability_code().is_some() {
                let _ = call.reject(code);
            }
            else {
                let _ = call.fail(code);
            }
            Err(error)
        }
    }
}

pub fn query(filter: &Query) -> Result<Vec<Entry>, wae::audit::AuditError> {
    auditor().query(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_zipper_core::{CapabilityState, Error as CoreError, ErrorCode};
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    use wae::audit::{ActorKind, Outcome};

    fn test_auditor() -> (Auditor, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("open-zipper-audit-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
        let actor = Actor { kind: ActorKind::Agent, id: "agent-1".into(), source: "test".into() };
        let auditor = Auditor::builder(APP, "0.0.0").actor(actor).session("session-1").host("host-1").dir(&dir).build();
        (auditor, dir)
    }

    #[test]
    fn success_is_recorded_with_targets() {
        let (auditor, _) = test_auditor();
        run_with(&auditor, "list", Mode::DryRun, serde_json::json!({ "archive": "C:/tmp/a.zip" }), || {
            Ok((1, Summary::new(3, "three entries")))
        })
        .unwrap();
        let entries = auditor.query(&Query::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].capability, "list");
        assert_eq!(entries[0].outcome, Outcome::Success);
        assert_eq!(entries[0].targets.count, 3);
    }

    #[test]
    fn capability_refusal_is_rejected() {
        let (auditor, _) = test_auditor();
        let rejected: CoreResult<()> = run_with(&auditor, "extract", Mode::Execute, Value::Null, || {
            Err(CoreError::capability(CapabilityState::DependencyBlocked, "7z is blocked"))
        });
        assert!(rejected.is_err());
        let entries = auditor.query(&Query::default()).unwrap();
        assert_eq!(entries[0].outcome, Outcome::Rejected);
        assert_eq!(entries[0].error_code.as_deref(), Some("dependency_blocked"));
    }

    #[test]
    fn safety_failure_is_recorded_with_domain_code() {
        let (auditor, _) = test_auditor();
        let rejected: CoreResult<()> =
            run_with(&auditor, "extract", Mode::Execute, Value::Null, || Err(CoreError::coded(ErrorCode::ZipSlip, "entry `../evil` escapes")));
        assert!(rejected.is_err());
        let entries = auditor.query(&Query::default()).unwrap();
        assert_eq!(entries[0].outcome, Outcome::Failure);
        assert_eq!(entries[0].error_code.as_deref(), Some("zip_slip"));
    }

    #[test]
    fn credentials_in_params_are_redacted() {
        let (auditor, dir) = test_auditor();
        run_with(&auditor, "extract", Mode::Execute, serde_json::json!({ "token": "SECRET", "archive": "C:/tmp/a.zip" }), || {
            Ok(((), Summary::new(0, "none")))
        })
        .unwrap();
        let text = std::fs::read_to_string(dir.join("open-zipper.jsonl")).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(text.contains("<redacted>"));
    }
}
