//! Ledger #2786 (found by the check-then-act audit, ledger #2772): two live reactors over one
//! root, and an identical re-drop of a tuple while one of them is still on its pass.
//!
//! Each reactor's pass lock is its own, and an identical drop after a claim is a NEW request
//! (the Sink arm), so the second reactor claims the re-drop while the first is mid-pass. The
//! claim used to stage at the FIXED name `.processing/<id>.tuple`: the second claim renamed
//! over the first one's staged file, the first settle moved the second pass's tuple, and the
//! second settle failed its move and withdrew its own note. A failed pass left no dead letter
//! and the tuple read as handled, where `retry=` cannot see it.

// Wall time for bounded waits: a native integration test of a native-only crate.
#![allow(clippy::disallowed_methods)]

mod common;

use common::{drop_tuple, scratch};
use ikigai_core::{Capability, Error, Kernel, ReprType, Representation, Request, SpaceEntry};
use ikigai_intray::{space, Outcome, SpaceReactor, CAP_OUT, ERROR_DIR, OUTBOX_DIR};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A handler that says it has started, waits until it is let go, then answers `answer`.
struct Held {
    started: AtomicBool,
    go: AtomicBool,
    answer: Result<&'static str, &'static str>,
}

impl Held {
    fn new(answer: Result<&'static str, &'static str>) -> Arc<Self> {
        Arc::new(Held {
            started: AtomicBool::new(false),
            go: AtomicBool::new(false),
            answer,
        })
    }
}

impl ikigai_resolve::Resolver for Held {
    fn issue(
        &self,
        request: Request,
    ) -> Result<(Representation, ikigai_resolve::CacheStatus), Error> {
        self.issue_as(request, &Capability::root())
    }
    fn issue_as(
        &self,
        _: Request,
        _: &Capability,
    ) -> Result<(Representation, ikigai_resolve::CacheStatus), Error> {
        self.started.store(true, Ordering::SeqCst);
        wait_for(|| self.go.load(Ordering::SeqCst));
        self.answer
            .map(|said| {
                (
                    Representation::new(ReprType::new("text/plain"), said.as_bytes().to_vec()),
                    ikigai_resolve::CacheStatus::Uncacheable,
                )
            })
            .map_err(|why| Error::Endpoint(why.to_string()))
    }
    fn is_cached(&self, _: &Request, _: &Capability) -> bool {
        false
    }
    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        None
    }
}

fn wait_for(what: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !what() {
        assert!(Instant::now() < end, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn reactor(root: &Path, handler: Arc<Held>) -> SpaceReactor {
    SpaceReactor::new(
        root.to_path_buf(),
        handler,
        Capability::scoped(["urn:cap:demo"]),
    )
}

/// A fails slowly while B handles the re-drop: B settles first, A last. Each pass settles its
/// OWN claim, so A's failure is the tuple's record (a dead letter with its note, which the
/// hook hears and `retry=` can re-arm) and neither settle fails its move.
#[test]
fn two_reactors_over_one_root_keep_each_passs_record() {
    let root = scratch("two-reactors");
    std::fs::create_dir_all(root.join("jobs")).unwrap();
    std::fs::write(root.join("jobs/handler"), "urn:test:handler").unwrap();
    let k = Kernel::new(Arc::new(space(root.clone())));
    let out = Capability::scoped([CAP_OUT]);

    let slow = Held::new(Err("the mailer was down"));
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let told = Arc::clone(&heard);
    let a = Arc::new(
        reactor(&root, Arc::clone(&slow)).on_dead_letter(move |_, id, why| {
            told.lock().unwrap().push((id.to_string(), why.to_string()))
        }),
    );
    let quick = Held::new(Ok("booked"));
    quick.go.store(true, Ordering::SeqCst);
    let b = reactor(&root, Arc::clone(&quick));

    let id = drop_tuple(&k, &out, "urn:space:jobs", b"book me").unwrap();
    let pass_a = {
        let (a, id) = (Arc::clone(&a), id.clone());
        std::thread::spawn(move || a.process("jobs", &id))
    };
    wait_for(|| slow.started.load(Ordering::SeqCst));

    // The same bytes again, while A is mid-pass: a new request, and B takes it.
    assert_eq!(
        drop_tuple(&k, &out, "urn:space:jobs", b"book me").unwrap(),
        id
    );
    assert_eq!(b.process("jobs", &id), Outcome::Handled);

    slow.go.store(true, Ordering::SeqCst);
    let failed = pass_a.join().unwrap();
    let Outcome::Errored(why) = &failed else {
        panic!("A's pass settled as {failed:?}");
    };
    assert!(
        !why.contains("and then"),
        "A's settle could not move its own claim: {why}"
    );
    assert_eq!(heard.lock().unwrap().len(), 1, "the hook heard A's failure");

    let at = |stage: &str, ext: &str| root.join("jobs").join(stage).join(format!("{id}.{ext}"));
    assert!(
        at(ERROR_DIR, "tuple").is_file(),
        "the last pass failed, but there is no dead letter"
    );
    let note = std::fs::read_to_string(at(ERROR_DIR, "err")).expect("the dead letter's note");
    assert!(note.contains("the mailer was down"), "{note}");
    assert!(
        !at(OUTBOX_DIR, "tuple").exists(),
        "the tuple reads as handled although its last pass failed"
    );
    let processing = std::fs::read_dir(root.join("jobs/.processing"))
        .unwrap()
        .count();
    assert_eq!(processing, 0, "a claim was left behind");
}
