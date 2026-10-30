//! Preflight follows the fixed plans of named requests without executing them.
#![cfg(feature = "plan-reader")]
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    AliasTable, ArgRef, Capability, Description, EndpointSpace, Exact, FixedArgument, FnEndpoint,
    Invocation, Iri, Kernel, MetaRenderer, ReprType, Representation, Request, TopologyRule, Verb,
};

struct JsonRenderer;
impl MetaRenderer for JsonRenderer {
    fn render(
        &self,
        description: &Description,
        _: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).unwrap(),
        ))
    }
}

fn plan(target: &str) -> String {
    format!(
        r#"@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:plan:test> a ik:Process ; ik:step <urn:plan:test:step:1> ; ik:result <urn:plan:test:step:1> .
<urn:plan:test:step:1> a ik:Step ; ik:verb "Source" ; ik:resolves <{target}> ."#
    )
}

fn kernel(bindings: &[(&str, String)]) -> Kernel {
    let mut aliases = AliasTable::new();
    for (name, source) in bindings {
        aliases = aliases
            .with_rule(
                TopologyRule::named_request(*name, "urn:plan:eval")
                    .verb(Verb::Source)
                    .fixing(FixedArgument::value("in", source)),
            )
            .unwrap();
    }
    kernel_with_aliases(aliases)
}

fn kernel_with_aliases(aliases: AliasTable) -> Kernel {
    let space = EndpointSpace::new().bind(
        Exact::new("urn:http:get"),
        FnEndpoint::new("http-get", |_: &Invocation<'_>| {
            panic!("preflight must not execute a plan")
        })
        .with_description(
            Description::new("http-get")
                .verb(Verb::Source)
                .requires("urn:cap:net:*"),
        ),
    );
    Kernel::with_meta_renderer(
        Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(space),
            Arc::new(ikigai_engine::plan_space::space()),
        ])),
        Arc::new(JsonRenderer),
    )
    .with_aliases(Arc::new(aliases))
}

fn report(kernel: &Kernel, target: &str, turtle: bool) -> ikigai_core::Result<String> {
    let request = Request::new(Verb::Source, Iri::parse("urn:plan:requires").unwrap())
        .with_arg("in", ArgRef::Inline(plan(target).into_bytes()))
        .with_arg(
            "as",
            ArgRef::Inline(if turtle {
                b"text/turtle".to_vec()
            } else {
                b"text/plain".to_vec()
            }),
        );
    block_on(kernel.issue(request, &Capability::scoped(["urn:cap:fs:read:*"])))
        .map(|answer| String::from_utf8(answer.bytes).unwrap())
}

#[test]
fn nested_named_plan_reports_its_network_requirement() {
    let kernel = kernel(&[("urn:test:nested", plan("urn:http:get"))]);
    let text = report(&kernel, "urn:test:nested", false).unwrap();
    assert!(
        text.starts_with("<urn:plan:test> requires urn:cap:net:*\n"),
        "{text}"
    );
    assert!(text.contains("LACKS urn:cap:net:*"), "{text}");
    let turtle = report(&kernel, "urn:test:nested", true).unwrap();
    assert!(turtle.contains("ik:requires <urn:cap:net:*>"), "{turtle}");
}

#[test]
fn self_referencing_named_plan_is_refused_as_a_cycle() {
    let kernel = kernel(&[("urn:test:loop", plan("urn:test:loop"))]);
    let error = report(&kernel, "urn:test:loop", false).expect_err("cycle must be refused");
    assert!(error.to_string().contains("cycle"), "{error}");
}

#[test]
fn requirements_follow_multiple_named_plans() {
    let kernel = kernel(&[
        ("urn:test:outer", plan("urn:test:inner")),
        ("urn:test:inner", plan("urn:http:get")),
    ]);
    let text = report(&kernel, "urn:test:outer", false).unwrap();
    assert!(
        text.starts_with("<urn:plan:test> requires urn:cap:net:*\n"),
        "{text}"
    );
}

#[test]
fn indirect_cycle_is_refused() {
    let kernel = kernel(&[
        ("urn:test:outer", plan("urn:test:inner")),
        ("urn:test:inner", plan("urn:test:outer")),
    ]);
    let error = report(&kernel, "urn:test:outer", true).unwrap_err();
    assert!(error.to_string().contains("cycle"), "{error}");
}

#[test]
fn named_plan_depth_is_bounded_at_sixteen() {
    for depth in [16, 17] {
        let bindings: Vec<_> = (0..depth)
            .map(|i| {
                let target = if i + 1 == depth {
                    "urn:http:get".to_string()
                } else {
                    format!("urn:test:nested:{}", i + 1)
                };
                (format!("urn:test:nested:{i}"), plan(&target))
            })
            .collect();
        let bindings: Vec<_> = bindings
            .iter()
            .map(|(name, plan)| (name.as_str(), plan.clone()))
            .collect();
        let answer = report(&kernel(&bindings), "urn:test:nested:0", false);
        if depth == 16 {
            assert!(answer.unwrap().contains("requires urn:cap:net:*"));
        } else {
            let error = answer.unwrap_err();
            assert!(error.to_string().contains("maximum depth of 16"), "{error}");
        }
    }
}

#[test]
fn unresolved_nested_target_keeps_the_report_incomplete() {
    let kernel = kernel(&[("urn:test:nested", plan("urn:test:missing"))]);
    let text = report(&kernel, "urn:test:nested", false).unwrap();
    assert!(text.contains("requires AT LEAST nothing"), "{text}");
    assert!(text.contains("UNRESOLVED <urn:test:missing>"), "{text}");
    let turtle = report(&kernel, "urn:test:nested", true).unwrap();
    assert!(turtle.contains("outcome:incomplete>"), "{turtle}");
}

#[test]
fn a_shared_subplan_is_not_a_cycle() {
    let mut shared = plan("urn:test:leaf");
    // Both the step target and its sourced argument execute the same named plan.
    shared.push_str(
        r#"
<urn:plan:test:step:1> ik:argument <urn:plan:test:step:1:arg:in> .
<urn:plan:test:step:1:arg:in> a ik:Argument ; ik:inputName "in" ; ik:ref <urn:test:leaf> ."#,
    );
    let kernel = kernel(&[
        ("urn:test:outer", shared),
        ("urn:test:leaf", plan("urn:http:get")),
    ]);
    let text = report(&kernel, "urn:test:outer", false).unwrap();
    assert!(
        text.starts_with("<urn:plan:test> requires urn:cap:net:*\n"),
        "{text}"
    );
}

#[test]
fn a_fixed_plan_on_an_unrelated_endpoint_is_ordinary_data() {
    let aliases = AliasTable::new()
        .with_rule(
            TopologyRule::named_request("urn:test:data", "urn:http:get")
                .verb(Verb::Source)
                .fixing(FixedArgument::value("in", plan("urn:test:data"))),
        )
        .unwrap();
    // The plan-shaped value is self-referencing, but http-get does not evaluate it.
    let text = report(&kernel_with_aliases(aliases), "urn:test:data", false).unwrap();
    assert!(
        text.starts_with("<urn:plan:test> requires urn:cap:net:*\n"),
        "{text}"
    );
}

#[test]
fn a_binding_that_forwards_every_verb_still_expands_on_source() {
    let aliases = AliasTable::new()
        .with_rule(
            TopologyRule::named_request("urn:test:nested", "urn:plan:eval")
                .fixing(FixedArgument::value("in", plan("urn:http:get"))),
        )
        .unwrap();
    let text = report(&kernel_with_aliases(aliases), "urn:test:nested", false).unwrap();
    assert!(
        text.starts_with("<urn:plan:test> requires urn:cap:net:*\n"),
        "{text}"
    );
}
