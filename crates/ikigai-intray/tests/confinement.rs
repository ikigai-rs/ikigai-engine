//! The intray reaches nothing outside the root its host bound (ledger #2875): a space name
//! that climbs out (`..`) or replaces the root (an absolute path) is refused, and so is a
//! symbolic link anywhere below the root, whatever it points at. Before the disk module held
//! the root, every path was built by joining onto it, so a reactor handed `../x` worked on
//! `x`, and a link planted in the tree was followed by every read, claim and write.

mod common;

use common::{drop_tuple, iri, scratch, Counting};
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Kernel, Request, Verb};
use ikigai_intray::{space, tuple_id, SpaceReactor, CAP_OUT, CAP_READ, CAP_TAKE, INBOX_DIR};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn all() -> Capability {
    Capability::scoped([CAP_OUT, CAP_READ, CAP_TAKE])
}

/// `<base>/root`, the root the host binds, and `<base>/outside`, a reactive space beside it
/// (a `handler` file and one pending tuple) that nothing under the root may reach.
fn root_beside_a_space(tag: &str) -> (PathBuf, PathBuf, String) {
    let base = scratch(tag);
    let (root, outside) = (base.join("root"), base.join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(outside.join(INBOX_DIR)).unwrap();
    std::fs::write(outside.join("handler"), "urn:test:handler").unwrap();
    let id = tuple_id(b"not yours");
    std::fs::write(
        outside.join(INBOX_DIR).join(format!("{id}.tuple")),
        b"not yours",
    )
    .unwrap();
    (root, outside, id)
}

fn untouched(outside: &Path, id: &str) {
    assert_eq!(
        std::fs::read(outside.join(INBOX_DIR).join(format!("{id}.tuple"))).unwrap(),
        b"not yours",
        "the tuple outside the root is where it was"
    );
    let mut left: Vec<_> = std::fs::read_dir(outside)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["handler", INBOX_DIR],
        "nothing was created beside it"
    );
}

fn issue(k: &Kernel, verb: Verb, args: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
    let mut req = Request::new(verb, iri("urn:space:jobs"));
    for (n, v) in args {
        req = req.with_arg(*n, ArgRef::Inline(v.as_bytes().to_vec()));
    }
    block_on(k.issue(req, &all())).map(|r| r.bytes)
}

/// The reactor's `drain` and `process` take a space name from the host; one that climbs out
/// of the root, or is an absolute path, is not a space under it.
#[test]
fn a_reactor_never_reaches_a_space_outside_its_root() {
    let (root, outside, id) = root_beside_a_space("confine-climb");
    let handler = Arc::new(Counting::new());
    let reactor = SpaceReactor::new(root.clone(), handler.clone(), Capability::root());
    let absolute = outside.to_str().unwrap().to_string();
    for name in ["../outside", absolute.as_str()] {
        let drained = reactor.drain(name);
        assert!(
            drained.is_empty(),
            "`{name}` drained {drained:?} outside the root"
        );
        let outcome = reactor.process(name, &id);
        assert!(
            matches!(outcome, ikigai_intray::Outcome::Skipped(_)),
            "`{name}`: {outcome:?}"
        );
    }
    assert_eq!(handler.calls(), 0, "no handler fired");
    untouched(&outside, &id);
}

/// Reactor entry points use the endpoint's space-name rule, even for confined names.
#[test]
fn process_refuses_names_the_endpoint_refuses() {
    assert_invalid_space_names_refused(false);
}

#[test]
fn drain_refuses_names_the_endpoint_refuses() {
    assert_invalid_space_names_refused(true);
}

fn assert_invalid_space_names_refused(drain: bool) {
    for name in [
        "jobs.pending",
        "jobs ",
        "CON",
        "jobs:pending",
        "jobs\\pending",
    ] {
        let root = scratch(if drain {
            "drain-space-name"
        } else {
            "process-space-name"
        });
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(INBOX_DIR)).unwrap();
        std::fs::write(dir.join("handler"), "urn:test:handler").unwrap();
        let id = tuple_id(b"pending");
        let tuple = dir.join(INBOX_DIR).join(format!("{id}.tuple"));
        std::fs::write(&tuple, b"pending").unwrap();
        let handler = Arc::new(Counting::new());
        let reactor = SpaceReactor::new(root, handler.clone(), Capability::root());
        if drain {
            assert!(reactor.drain(name).is_empty(), "drain accepted {name:?}");
        } else {
            let outcome = reactor.process(name, &id);
            assert!(
                matches!(outcome, ikigai_intray::Outcome::Skipped(_)),
                "process accepted {name:?}: {outcome:?}"
            );
        }
        assert_eq!(handler.calls(), 0, "no handler fired for {name:?}");
        assert_eq!(std::fs::read(tuple).unwrap(), b"pending");
    }
}

/// A tuple id is one file name too: one that climbs out of the stage is refused.
#[test]
fn a_reactor_never_claims_a_tuple_id_that_climbs_out() {
    let (root, outside, id) = root_beside_a_space("confine-id");
    std::fs::create_dir_all(root.join("jobs").join(INBOX_DIR)).unwrap();
    std::fs::write(root.join("jobs").join("handler"), "urn:test:handler").unwrap();
    let handler = Arc::new(Counting::new());
    let reactor = SpaceReactor::new(root.clone(), handler.clone(), Capability::root());
    // `<root>/jobs/inbox/../../../outside/inbox/<id>` + `.tuple`
    let climbing = format!("../../../outside/inbox/{id}");
    let outcome = reactor.process("jobs", &climbing);
    assert!(
        !matches!(outcome, ikigai_intray::Outcome::Handled),
        "{outcome:?}"
    );
    assert_eq!(handler.calls(), 0, "no handler fired");
    untouched(&outside, &id);
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A space's inbox that is a link out of the root lists nothing, takes nothing and
    /// accepts no drop.
    #[test]
    fn an_inbox_linked_out_of_the_root_is_not_followed() {
        let (root, outside, id) = root_beside_a_space("confine-inbox-link");
        std::fs::create_dir_all(root.join("jobs")).unwrap();
        symlink(outside.join(INBOX_DIR), root.join("jobs").join(INBOX_DIR)).unwrap();
        let k = Kernel::new(Arc::new(space(root.clone())));

        assert_eq!(issue(&k, Verb::Source, &[]).unwrap(), b"", "listed nothing");
        assert!(issue(&k, Verb::Delete, &[("tuple", &id)]).is_err());
        assert!(issue(&k, Verb::Delete, &[]).is_err());
        assert!(drop_tuple(&k, &all(), "urn:space:jobs", b"in").is_err());
        untouched(&outside, &id);
    }

    /// A tuple file that is a link is never read through, even when what it points at hashes
    /// to its name: `rd` refuses it, and a take moves the LINK to `error/` and delivers nothing.
    #[test]
    fn a_linked_tuple_is_never_read_through() {
        let (root, outside, id) = root_beside_a_space("confine-tuple-link");
        let inbox = root.join("jobs").join(INBOX_DIR);
        std::fs::create_dir_all(&inbox).unwrap();
        let target = outside.join(INBOX_DIR).join(format!("{id}.tuple"));
        symlink(&target, inbox.join(format!("{id}.tuple"))).unwrap();
        let k = Kernel::new(Arc::new(space(root.clone())));

        let read = issue(&k, Verb::Source, &[("tuple", &id)]);
        assert!(
            matches!(read, Err(Error::NotFound(_))),
            "rd read through a link: {read:?}"
        );
        let matched = issue(&k, Verb::Source, &[("match", "ASK { ?s ?p ?o }")]).unwrap();
        assert_eq!(matched, b"");
        let taken = issue(&k, Verb::Delete, &[("tuple", &id)]);
        assert!(taken.is_err(), "a take delivered through a link: {taken:?}");
        let quarantined = root.join("jobs").join("error").join(format!("{id}.tuple"));
        assert!(
            std::fs::symlink_metadata(&quarantined)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself was moved to `error/`"
        );
        untouched(&outside, &id);
    }

    /// A space directory that is a link out of the root is not a space: the reactor neither
    /// fires its handler nor moves its tuples, and `dead_letters` does not count it.
    #[test]
    fn a_space_linked_out_of_the_root_is_not_reacted_to() {
        let (root, outside, id) = root_beside_a_space("confine-space-link");
        symlink(&outside, root.join("jobs")).unwrap();
        let handler = Arc::new(Counting::new());
        let reactor = SpaceReactor::new(root.clone(), handler.clone(), Capability::root());

        assert!(reactor.drain("jobs").is_empty());
        assert!(matches!(
            reactor.process("jobs", &id),
            ikigai_intray::Outcome::Skipped(_)
        ));
        assert!(reactor.sweep_interrupted().is_empty());
        assert!(ikigai_intray::dead_letters(&root).is_empty());
        assert_eq!(handler.calls(), 0);
        untouched(&outside, &id);
    }

    /// The root itself may be a link (macOS's `/tmp` is one): the host bound it, so it is
    /// trusted, and only what lies below it is judged.
    #[test]
    fn a_root_that_is_a_link_still_serves() {
        let base = scratch("confine-root-link");
        std::fs::create_dir_all(base.join("real")).unwrap();
        symlink(base.join("real"), base.join("root")).unwrap();
        let k = Kernel::new(Arc::new(space(base.join("root"))));
        let id = drop_tuple(&k, &all(), "urn:space:jobs", b"fine").unwrap();
        assert_eq!(issue(&k, Verb::Source, &[]).unwrap(), id.as_bytes());
        assert_eq!(issue(&k, Verb::Delete, &[]).unwrap(), b"fine");
    }
}
