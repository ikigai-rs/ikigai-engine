//! What the intray does ON DISK, pinned before its disk module was confined to the root (ledger
//! #2875): the claim by rename, the publish, and the recoveries after a crash, each by the
//! files it leaves. The confinement changed how every path is built, so these say the paths
//! did not move.

// Wall time, to plant a live claim and an old one: a native integration test of a native-only
// crate (notify, std::fs), never built for wasm.
#![allow(clippy::disallowed_methods)]

mod common;

use common::{drop_tuple, iri, scratch, Counting};
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Kernel, Request, Verb};
use ikigai_intray::{
    space, tuple_id, Interrupted, Outcome, SpaceEndpoint, SpaceReactor, CAP_OUT, CAP_READ,
    CAP_RETRY, CAP_TAKE, DROPPING_DIR, ERROR_DIR, INBOX_DIR, OUTBOX_DIR, PROCESSING_DIR,
    TAKING_DIR,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn all() -> Capability {
    Capability::scoped([CAP_OUT, CAP_READ, CAP_TAKE, CAP_RETRY])
}

fn stage(root: &Path, space: &str, stage: &str) -> PathBuf {
    root.join(space).join(stage)
}

/// The file names in a stage, sorted; empty when the stage does not exist.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn issue(k: &Kernel, verb: Verb, args: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
    let mut req = Request::new(verb, iri("urn:space:jobs"));
    for (n, v) in args {
        req = req.with_arg(*n, ArgRef::Inline(v.as_bytes().to_vec()));
    }
    block_on(k.issue(req, &all())).map(|r| r.bytes)
}

/// `out` writes the tuple whole under `.dropping/` and renames it into
/// `<root>/<space>/inbox/<id>.tuple`; nothing is left in staging.
#[test]
fn a_drop_is_published_into_the_inbox_by_its_id() {
    let root = scratch("disk-publish");
    let k = Kernel::new(Arc::new(space(root.clone())));
    let id = drop_tuple(&k, &all(), "urn:space:jobs", b"work").unwrap();
    assert_eq!(id, tuple_id(b"work"));
    assert_eq!(
        std::fs::read(stage(&root, "jobs", INBOX_DIR).join(format!("{id}.tuple"))).unwrap(),
        b"work"
    );
    assert_eq!(
        files(&stage(&root, "jobs", INBOX_DIR)),
        [format!("{id}.tuple")]
    );
    assert!(files(&stage(&root, "jobs", DROPPING_DIR)).is_empty());
}

/// A take claims by renaming the tuple into `.taking/` and removes the claim once read: the
/// inbox and the staging are empty after it, and a second take of the same id finds nothing.
#[test]
fn a_take_claims_by_rename_and_leaves_nothing_behind() {
    let root = scratch("disk-take");
    let k = Kernel::new(Arc::new(space(root.clone())));
    let id = drop_tuple(&k, &all(), "urn:space:jobs", b"work").unwrap();
    assert_eq!(issue(&k, Verb::Delete, &[("tuple", &id)]).unwrap(), b"work");
    assert!(files(&stage(&root, "jobs", INBOX_DIR)).is_empty());
    assert!(files(&stage(&root, "jobs", TAKING_DIR)).is_empty());
    assert!(matches!(
        issue(&k, Verb::Delete, &[("tuple", &id)]),
        Err(Error::NotFound(_))
    ));
}

/// A take that stopped after its claim leaves `.taking/<id>.<millis>.<pid>.<n>.claim`; one older
/// than five minutes goes back to the inbox at the next `rd`, and a fresh one stays.
#[test]
fn a_crashed_take_is_requeued_and_a_live_one_is_not() {
    let root = scratch("disk-stale-take");
    let taking = stage(&root, "jobs", TAKING_DIR);
    std::fs::create_dir_all(&taking).unwrap();
    let (stale, live) = (tuple_id(b"stale"), tuple_id(b"live"));
    std::fs::write(taking.join(format!("{stale}.1.0.0.claim")), b"stale").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let fresh = format!("{live}.{now}.0.0.claim");
    std::fs::write(taking.join(&fresh), b"live").unwrap();

    let k = Kernel::new(Arc::new(space(root.clone())));
    assert_eq!(issue(&k, Verb::Source, &[]).unwrap(), stale.as_bytes());
    assert_eq!(
        files(&stage(&root, "jobs", INBOX_DIR)),
        [format!("{stale}.tuple")]
    );
    assert_eq!(files(&taking), [fresh]);
}

/// A claim from before unique staging names (`.taking/<id>.tuple`) is judged by its mtime,
/// which is the drop time: an old one is requeued.
#[test]
fn a_legacy_claim_is_requeued_by_its_age() {
    let root = scratch("disk-legacy-take");
    let taking = stage(&root, "jobs", TAKING_DIR);
    std::fs::create_dir_all(&taking).unwrap();
    let id = tuple_id(b"old");
    let file = taking.join(format!("{id}.tuple"));
    std::fs::write(&file, b"old").unwrap();
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let k = Kernel::new(Arc::new(space(root.clone())));
    assert_eq!(issue(&k, Verb::Delete, &[]).unwrap(), b"old");
    assert!(files(&taking).is_empty());
}

/// A file in the inbox that does not hash to its name is moved to `error/` beside an `.err`
/// note when a take claims it, and the take answers the next tuple.
#[test]
fn a_corrupt_tuple_is_quarantined_by_the_take_that_claims_it() {
    let root = scratch("disk-quarantine");
    let k = Kernel::new(Arc::new(space(root.clone())));
    drop_tuple(&k, &all(), "urn:space:jobs", b"good").unwrap();
    let bad = "0".repeat(64);
    std::fs::write(
        stage(&root, "jobs", INBOX_DIR).join(format!("{bad}.tuple")),
        b"not it",
    )
    .unwrap();

    assert_eq!(issue(&k, Verb::Delete, &[]).unwrap(), b"good");
    let error = stage(&root, "jobs", ERROR_DIR);
    assert_eq!(
        files(&error),
        [format!("{bad}.err"), format!("{bad}.tuple")]
    );
    assert!(std::fs::read_to_string(error.join(format!("{bad}.err")))
        .unwrap()
        .starts_with("corrupt:"));
    assert!(files(&stage(&root, "jobs", INBOX_DIR)).is_empty());
    assert!(files(&stage(&root, "jobs", TAKING_DIR)).is_empty());
}

/// A reactor pass claims into `.processing/` and settles into `outbox/` beside the answer;
/// a failing one into `error/` beside its reason; `retry=` moves it back to the inbox and
/// clears the note.
#[test]
fn a_pass_settles_by_rename_and_a_retry_moves_it_back() {
    let root = scratch("disk-settle");
    std::fs::create_dir_all(root.join("jobs")).unwrap();
    std::fs::write(root.join("jobs").join("handler"), "urn:test:handler").unwrap();
    let k = Kernel::new(Arc::new(space(root.clone())));
    let handled = drop_tuple(&k, &all(), "urn:space:jobs", b"one").unwrap();

    let said = Arc::new(Counting::new());
    said.answers.lock().unwrap().push("done");
    let reactor = SpaceReactor::new(root.clone(), said, Capability::root());
    assert_eq!(
        reactor.drain("jobs"),
        vec![(handled.clone(), Outcome::Handled)]
    );
    let outbox = stage(&root, "jobs", OUTBOX_DIR);
    assert_eq!(
        files(&outbox),
        [format!("{handled}.out"), format!("{handled}.tuple")]
    );
    assert_eq!(
        std::fs::read_to_string(outbox.join(format!("{handled}.out"))).unwrap(),
        "done"
    );
    assert!(files(&stage(&root, "jobs", PROCESSING_DIR)).is_empty());

    // A handler URI that does not parse dead-letters the tuple.
    std::fs::write(root.join("jobs").join("handler"), "not an iri").unwrap();
    let failed = drop_tuple(&k, &all(), "urn:space:jobs", b"two").unwrap();
    assert!(matches!(
        reactor.process("jobs", &failed),
        Outcome::Errored(_)
    ));
    let error = stage(&root, "jobs", ERROR_DIR);
    assert_eq!(
        files(&error),
        [format!("{failed}.err"), format!("{failed}.tuple")]
    );

    assert_eq!(
        issue(&k, Verb::Sink, &[("retry", &failed)]).unwrap(),
        format!("{failed}\n").as_bytes()
    );
    assert!(files(&error).is_empty(), "the tuple and its note are gone");
    assert_eq!(
        files(&stage(&root, "jobs", INBOX_DIR)),
        [format!("{failed}.tuple")]
    );
}

/// A reactor that stopped mid-pass leaves its claim in `.processing/`, under a claim name or,
/// from before those, as `<id>.tuple`. The next reactor dead-letters both (the default), or
/// requeues both into the inbox.
#[test]
fn a_crashed_pass_is_recovered_in_both_claim_forms() {
    for (policy, to) in [
        (Interrupted::DeadLetter, ERROR_DIR),
        (Interrupted::Requeue, INBOX_DIR),
    ] {
        let root = scratch(&format!("disk-recover-{to}"));
        std::fs::create_dir_all(root.join("jobs")).unwrap();
        std::fs::write(root.join("jobs").join("handler"), "urn:test:handler").unwrap();
        let processing = stage(&root, "jobs", PROCESSING_DIR);
        std::fs::create_dir_all(&processing).unwrap();
        let (named, legacy) = (tuple_id(b"named"), tuple_id(b"legacy"));
        std::fs::write(processing.join(format!("{named}.9.9.9.claim")), b"named").unwrap();
        std::fs::write(processing.join(format!("{legacy}.tuple")), b"legacy").unwrap();

        let reactor =
            SpaceReactor::new(root.clone(), Arc::new(Counting::new()), Capability::root())
                .on_interrupted(policy);
        let recovered = reactor.recover_interrupted().unwrap();
        assert_eq!(recovered.len(), 2, "{recovered:?}");
        assert!(recovered.iter().all(|r| r.outcome == Ok(policy)));
        assert!(files(&processing).is_empty());
        let mut want = vec![format!("{named}.tuple"), format!("{legacy}.tuple")];
        if policy == Interrupted::DeadLetter {
            want.extend([format!("{named}.err"), format!("{legacy}.err")]);
        }
        want.sort();
        assert_eq!(files(&stage(&root, "jobs", to)), want);
    }
}

/// `discard_staging` removes what `.dropping/` and `.taking/` hold, and nothing else.
#[test]
fn discarding_staging_touches_only_the_two_staging_stages() {
    let root = scratch("disk-discard");
    let k = Kernel::new(Arc::new(space(root.clone())));
    let id = drop_tuple(&k, &all(), "urn:space:jobs", b"kept").unwrap();
    for (dir, file) in [
        (DROPPING_DIR, "x.1.2.3.drop"),
        (TAKING_DIR, "y.1.2.3.claim"),
    ] {
        std::fs::create_dir_all(stage(&root, "jobs", dir)).unwrap();
        std::fs::write(stage(&root, "jobs", dir).join(file), b"leftover").unwrap();
    }
    assert_eq!(
        SpaceEndpoint::new(root.clone())
            .discard_staging("jobs")
            .unwrap(),
        2
    );
    assert_eq!(
        files(&stage(&root, "jobs", INBOX_DIR)),
        [format!("{id}.tuple")]
    );
}
