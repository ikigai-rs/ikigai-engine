//! `bind` / `unbind` (named requests E4, ledger #2509): write a NAMED REQUEST into the host's
//! names atom.
//!
//! `bind <name> = <verb> <target> [key=value …]` reads the binding in the names text form
//! ([`crate::names`]), checks it against the TARGET's contract, and writes the names atom back
//! with the binding in it (a binding of the same name is replaced, and the reply says what it
//! was). `unbind <name>` takes one out; `bind` alone prints the names in force.
//!
//! - **A binding fixes only what its target takes.** Each `key=value` must name an argument
//!   the target declares for the verb (any verb's, for `forward`): `bind urn:x = source
//!   urn:t:upper nosuch=1` is refused naming `nosuch`. A target with an OPEN argument set takes
//!   any name. A target that does not answer, or whose contract cannot be read, is refused: a
//!   binding is checked when it is written, not on the first call.
//! - **A plan binding gets the plan's closed contract.** For a `urn:plan:eval` target whose
//!   `in` is fixed (a reference to the plan, or the plan inline), the plan's `ik:input`s are
//!   copied onto the rule as the name's own declared inputs, and a fixed argument the plan does
//!   not declare is refused, though plan-eval's own argument set is open. Reading a plan needs
//!   the `plan-reader` feature.
//! - **A templated target is checked when it is called.** Its contract is a function of the
//!   capture, so there is none to read before the name is resolved; the reply says so.
//! - **Authority is unchanged.** The atom is read and written under the session's capability,
//!   like any request, and a binding declares and grants nothing: the forward a name performs
//!   carries the caller's capability.
//!
//! The atom is the HOST's to name ([`Engine::with_names_atom`]): which resource holds a
//! host's names is a fact about the host, and a host that names none refuses `bind` by name.

use ikigai_core::{ArgRef, ArgSpec, FixedValue, InputSource, Iri, Request, TopologyRule, Verb};

use super::{
    declared_arguments, describe, parse_target, takes_open_arguments, verb_action, verb_inputs,
    Engine,
};
use crate::names::{in_table_order, parse_binding, parse_names, render_binding, render_names};

/// The resource a plan binding's target is: `urn:plan:eval`.
const PLAN_EVAL: &str = "urn:plan:eval";

impl Engine {
    /// The resource that holds this host's names, for `bind` and `unbind` (builder). Read with
    /// `Source` (nothing there reads as no names) and written with `Sink` `content=`, in the
    /// names text form ([`crate::names::MEDIA_NAMES`]); a host builds its named requests from
    /// it, through `urn:names:to-rdf` when it builds from Turtle.
    pub fn with_names_atom(mut self, atom: Iri) -> Self {
        self.names_atom = Some(atom);
        self
    }

    fn names_atom(&self) -> Result<Iri, String> {
        self.names_atom.clone().ok_or_else(|| {
            "`bind` and `unbind` write the host's names atom, and this host names none".to_string()
        })
    }

    /// `bind`, `bind <name> = <verb> <target> [key=value …]`.
    pub(super) async fn run_bind(&self, rest: &str) -> Result<String, String> {
        let atom = self.names_atom()?;
        if rest.trim().is_empty() {
            let rules = self.read_names(&atom).await?;
            if rules.is_empty() {
                return Ok(format!("no names are bound in <{}>", atom.as_str()));
            }
            return render_names(&rules);
        }
        let (rule, note) = self.checked_binding(parse_binding(rest)?).await?;
        let mut rules = self.read_names(&atom).await?;
        let was = rules
            .iter()
            .position(|r| r.from == rule.from)
            .map(|at| rules.remove(at));
        let line = render_binding(&rule)?;
        rules.push(rule);
        self.write_names(&atom, rules).await?;
        let mut out = format!("bound in <{}>:\n{line}", atom.as_str());
        if let Some(was) = was {
            out.push_str(&format!("(was: {})", render_binding(&was)?.trim_end()));
            out.push('\n');
        }
        if let Some(note) = note {
            out.push_str(&note);
            out.push('\n');
        }
        Ok(out.trim_end().to_string())
    }

    /// `unbind <name>`.
    pub(super) async fn run_unbind(&self, rest: &str) -> Result<String, String> {
        let atom = self.names_atom()?;
        let name = rest.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err("usage: unbind <name>".to_string());
        }
        let mut rules = self.read_names(&atom).await?;
        let Some(at) = rules.iter().position(|r| r.from == name) else {
            return Err(format!("`{name}` is not bound in <{}>", atom.as_str()));
        };
        let was = rules.remove(at);
        self.write_names(&atom, rules).await?;
        Ok(format!(
            "unbound `{name}` in <{}> (was: {})",
            atom.as_str(),
            render_binding(&was)?.trim_end()
        ))
    }

    /// The names in the atom, in table order. Nothing there (unresolved, not found) is no
    /// names; anything else that fails is refused, never read as empty, so a failed read never
    /// becomes a write that drops every binding.
    async fn read_names(&self, atom: &Iri) -> Result<Vec<TopologyRule>, String> {
        let capability = self.capability.borrow().clone();
        let answer = self
            .resolver
            .issue_as_async_in(
                Request::new(Verb::Source, atom.clone()),
                &capability,
                None,
                self.line_scope(),
            )
            .await;
        let bytes = match answer {
            Ok((representation, _)) => representation.bytes.to_vec(),
            Err(ikigai_core::Error::Unresolved(_) | ikigai_core::Error::NotFound(_)) => {
                return Ok(Vec::new())
            }
            Err(e) => {
                return Err(format!(
                    "the names atom <{}> could not be read: {}",
                    atom.as_str(),
                    describe(&*self.resolver, &e, self.host_build.as_ref())
                ))
            }
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("the names atom <{}> is not UTF-8 text", atom.as_str()))?;
        parse_names(&text).map_err(|e| format!("the names atom <{}>: {e}", atom.as_str()))
    }

    /// Write `rules` to the atom as a names file, checked as a whole table first.
    async fn write_names(&self, atom: &Iri, rules: Vec<TopologyRule>) -> Result<(), String> {
        let text = render_names(&in_table_order(rules)?)?;
        let request = Request::new(Verb::Sink, atom.clone())
            .with_arg("content", ArgRef::Inline(text.into_bytes()));
        self.run(request).await.map(drop)
    }

    /// `rule`, checked against its target's contract — and, for a plan binding, with the
    /// plan's parameters copied on as declared inputs — plus a note for the reply when the
    /// contract could not be read because the target is a template.
    async fn checked_binding(
        &self,
        mut rule: TopologyRule,
    ) -> Result<(TopologyRule, Option<String>), String> {
        if rule.to.contains('{') {
            return Ok((
                rule,
                Some(
                    "(the target is a template, so its contract is read when the name is called: \
                     the fixed arguments are checked then)"
                        .to_string(),
                ),
            ));
        }
        let target = parse_target(&rule.to)?;
        let description = match self.fetch_contract(&target).await {
            Ok(description) => description,
            Err(None) => {
                return Err(format!(
                    "nothing answers `{}`: a binding is checked against its target's contract, \
                     so its target must resolve",
                    rule.to
                ))
            }
            Err(Some(reason)) => {
                return Err(format!(
                    "`{}`'s contract could not be read ({reason}), and a binding is checked \
                     against it",
                    rule.to
                ))
            }
        };
        if let Some(verb) = rule.verb {
            if !description.action_specs().is_empty() && verb_action(&description, verb).is_none() {
                return Err(format!(
                    "`{}` does not declare {verb:?}, the verb the binding forwards",
                    rule.to
                ));
            }
        }
        let mut declared: Vec<String> = match rule.verb {
            Some(verb) => verb_inputs(&description, verb)
                .into_iter()
                .filter(|input| input.source == InputSource::Argument)
                .map(|input| input.name)
                .collect(),
            None => declared_arguments(Some(&description)),
        };
        let mut open = takes_open_arguments(Some(&description), rule.verb.unwrap_or(Verb::Source));
        if rule.to == PLAN_EVAL {
            if let Some(plan) = rule.fixes.iter().find(|fixed| fixed.name == "in") {
                let params = self.plan_parameters(&plan.value).await?;
                // The plan's parameters ARE the closed contract plan-eval's open set stands for.
                open = false;
                declared.extend(params.iter().map(|param| param.name.clone()));
                for param in params {
                    let taken = rule.fixes.iter().any(|fixed| fixed.name == param.name)
                        || rule.inputs.iter().any(|input| input.name == param.name);
                    if !taken {
                        rule = rule.input(param);
                    }
                }
            }
        }
        if !open {
            if let Some(fixed) = rule
                .fixes
                .iter()
                .find(|fixed| !declared.contains(&fixed.name))
            {
                return Err(undeclared(&rule.to, &declared, &fixed.name));
            }
        }
        Ok((rule, None))
    }

    /// The parameters of the plan a binding fixes `in` to, as the inputs a name declares.
    #[cfg(feature = "plan-reader")]
    async fn plan_parameters(&self, plan: &FixedValue) -> Result<Vec<ArgSpec>, String> {
        let turtle = match plan {
            FixedValue::Reference(iri) => {
                self.run(Request::new(Verb::Source, iri.clone()))
                    .await
                    .map_err(|e| format!("the plan <{}> could not be read: {e}", iri.as_str()))?
            }
            FixedValue::Value(text) => text.clone(),
            _ => return Ok(Vec::new()),
        };
        let plan = crate::plan::Plan::from_turtle(&turtle)?;
        Ok(plan
            .params
            .into_iter()
            .map(|param| {
                let mut input = ArgSpec::new(param.name);
                input.summary = param.summary.unwrap_or_default();
                input.class = param.class;
                input.default = param.default;
                input.required = param.required;
                input
            })
            .collect())
    }

    /// Without the reader there is no Turtle parser in this build to read the plan's
    /// parameters with, so say which feature is missing rather than bind an open contract.
    #[cfg(not(feature = "plan-reader"))]
    async fn plan_parameters(&self, _plan: &FixedValue) -> Result<Vec<ArgSpec>, String> {
        Err(
            "binding a plan copies its ik:inputs onto the name, which needs the `plan-reader` \
             feature of ikigai-engine"
                .to_string(),
        )
    }
}

/// The refusal for a binding that fixes `name`, which `target` does not declare.
fn undeclared(target: &str, declared: &[String], name: &str) -> String {
    let names = if declared.is_empty() {
        "it declares no arguments".to_string()
    } else {
        let names: Vec<String> = declared.iter().map(|name| format!("`{name}`")).collect();
        format!("its arguments: {}", names.join(", "))
    };
    format!(
        "`{target}` declares no argument `{name}` ({names}): a binding fixes only what its \
         target takes"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ikigai_core::{
        builtins, Alias, AliasTable, ArgSpec, Description, EndpointSpace, Error, Exact, Fallback,
        FnEndpoint, Invocation, Iri, Kernel, MetaRenderer, ReprType, Representation, Space, Verb,
    };

    use crate::engine::{Action, Engine};
    use crate::names::parse_names;

    const ATOM: &str = "urn:host:names";

    /// The JSON Meta face the engine reads a contract through (`fetch_contract`).
    struct JsonRenderer;
    impl MetaRenderer for JsonRenderer {
        fn render(
            &self,
            description: &Description,
            _target: &ReprType,
        ) -> ikigai_core::Result<Representation> {
            Ok(Representation::new(
                ReprType::new("application/json"),
                serde_json::to_vec(description).expect("serialize description"),
            ))
        }
    }

    /// A names atom over `held`: NotFound until written, never cached.
    fn atom(held: Arc<Mutex<Option<Vec<u8>>>>) -> FnEndpoint {
        FnEndpoint::new("names", move |inv: &Invocation<'_>| {
            if inv.request.verb == Verb::Sink {
                let content = inv.inline_arg("content")?.to_vec();
                *held.lock().unwrap() = Some(content);
                return Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"ok".to_vec(),
                ));
            }
            match held.lock().unwrap().clone() {
                Some(bytes) => Ok(Representation::new(ReprType::new("text/plain"), bytes)),
                None => Err(Error::NotFound(ATOM.to_string())),
            }
        })
        .with_description(
            Description::new("names")
                .verb(Verb::Source)
                .verb(Verb::Sink)
                .input(ArgSpec::new("content").optional()),
        )
    }

    /// `urn:t:upper` (declares `in`), an open `urn:t:open`, the plan `urn:test:plan`, the
    /// names atom, and (with the reader) the plan doors.
    fn host(held: Arc<Mutex<Option<Vec<u8>>>>) -> Arc<dyn Space> {
        let open = FnEndpoint::new("open", |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"open".to_vec(),
            ))
        })
        .with_description(
            Description::new("open")
                .verb(Verb::Source)
                .open_arguments("anything"),
        );
        let plan = FnEndpoint::new("plan", |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                include_bytes!("../../tests/fixtures/plan-shout.ttl").to_vec(),
            ))
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:t:upper"), builtins::to_upper())
            .bind(Exact::new("urn:t:open"), open)
            .bind(Exact::new("urn:test:plan"), plan)
            .bind(Exact::new(ATOM), atom(held));
        #[cfg(feature = "plan-reader")]
        return Arc::new(Fallback::new(vec![
            Arc::new(space),
            Arc::new(crate::plan_space::space()),
        ]));
        #[cfg(not(feature = "plan-reader"))]
        Arc::new(space)
    }

    fn engine() -> (Engine, Arc<Mutex<Option<Vec<u8>>>>) {
        let held = Arc::new(Mutex::new(None));
        let kernel = Kernel::with_meta_renderer(host(held.clone()), Arc::new(JsonRenderer));
        let engine = Engine::new(kernel).with_names_atom(Iri::parse(ATOM).unwrap());
        (engine, held)
    }

    fn output(action: Action) -> Result<String, String> {
        match action {
            Action::Output(entry) => entry.result,
            _ => panic!("expected output"),
        }
    }

    fn names(held: &Arc<Mutex<Option<Vec<u8>>>>) -> String {
        String::from_utf8(held.lock().unwrap().clone().unwrap_or_default()).unwrap()
    }

    /// The item's refusal: a binding fixes only what its target takes, and says which name
    /// it does not.
    #[test]
    fn a_binding_that_fixes_an_undeclared_argument_is_refused_naming_it() {
        let (engine, held) = engine();
        let refused = output(engine.eval("bind urn:x = source urn:t:upper nosuch=1")).unwrap_err();
        assert!(refused.contains("`nosuch`"), "{refused}");
        assert!(
            refused.contains("`in`"),
            "names what it does declare: {refused}"
        );
        assert!(held.lock().unwrap().is_none(), "nothing was written");
    }

    #[test]
    fn bind_writes_the_names_atom_and_unbind_takes_it_out() {
        let (engine, held) = engine();
        let bound = output(engine.eval("bind urn:x = source urn:t:upper in=hello")).unwrap();
        assert!(
            bound.contains("urn:x = source urn:t:upper in=hello"),
            "{bound}"
        );
        assert_eq!(names(&held), "urn:x = source urn:t:upper in=hello\n");

        // An open target takes any name; a second binding joins the first.
        output(engine.eval("bind urn:y = forward urn:t:open anything=1")).unwrap();
        assert_eq!(parse_names(&names(&held)).unwrap().len(), 2);
        assert!(output(engine.eval("bind"))
            .unwrap()
            .contains("urn:y = forward"));

        // Rebinding a name replaces it, and says what it was.
        let rebound = output(engine.eval("bind urn:x = source urn:t:upper in=again")).unwrap();
        assert!(
            rebound.contains("(was: urn:x = source urn:t:upper in=hello)"),
            "{rebound}"
        );
        assert!(names(&held).contains("in=again"));

        let unbound = output(engine.eval("unbind urn:x")).unwrap();
        assert!(unbound.contains("unbound `urn:x`"), "{unbound}");
        assert_eq!(names(&held), "urn:y = forward urn:t:open anything=1\n");
        let missing = output(engine.eval("unbind urn:x")).unwrap_err();
        assert!(missing.contains("not bound"), "{missing}");
    }

    #[test]
    fn a_binding_is_checked_when_it_is_written() {
        let (engine, _) = engine();
        let nothing = output(engine.eval("bind urn:x = source urn:t:nothing in=1")).unwrap_err();
        assert!(nothing.contains("nothing answers"), "{nothing}");
        let verb = output(engine.eval("bind urn:x = sink urn:t:upper in=1")).unwrap_err();
        assert!(verb.contains("does not declare Sink"), "{verb}");
        let splice =
            output(engine.eval("bind urn:x:{id} = source urn:t:upper in=a{id}")).unwrap_err();
        assert!(splice.contains("WHOLE value"), "{splice}");
    }

    #[test]
    fn a_host_that_names_no_atom_refuses_bind_by_name() {
        let kernel =
            Kernel::with_meta_renderer(host(Arc::new(Mutex::new(None))), Arc::new(JsonRenderer));
        let refused =
            output(Engine::new(kernel).eval("bind urn:x = source urn:t:upper in=1")).unwrap_err();
        assert!(refused.contains("names atom"), "{refused}");
    }

    /// What `bind` writes is a rule core runs: an alias built from the atom answers the name
    /// with the target's answer, the capture filling the whole value.
    #[test]
    fn a_bound_template_resolves_through_an_alias_built_from_the_atom() {
        let (engine, held) = engine();
        output(engine.eval("bind urn:shout:{word} = source urn:t:upper in={word}")).unwrap();
        let mut table = AliasTable::new();
        for rule in parse_names(&names(&held)).unwrap() {
            table = table.with_rule(rule).unwrap();
        }
        let alias = Alias::new(Arc::new(table), Arc::new(EndpointSpace::new()));
        let root: Arc<dyn Space> =
            Arc::new(Fallback::new(vec![host(held.clone()), Arc::new(alias)]));
        let shout = Engine::new(Kernel::with_meta_renderer(root, Arc::new(JsonRenderer)));
        assert_eq!(
            output(shout.eval("source urn:shout:hello")).unwrap(),
            "HELLO"
        );
        // A fixed argument is the binding's: the caller cannot give it.
        let refused = output(shout.eval("source urn:shout:hello in=other")).unwrap_err();
        assert!(refused.contains("fixed by"), "{refused}");
    }

    /// A plan binding copies the plan's `ik:input`s onto the name, and a fixed argument the
    /// plan does not declare is refused though plan-eval's own set is open.
    #[cfg(feature = "plan-reader")]
    #[test]
    fn a_plan_binding_takes_the_plans_closed_contract() {
        let (engine, held) = engine();
        let bound = output(engine.eval("bind urn:shout = source urn:plan:eval in=<urn:test:plan>"))
            .unwrap();
        assert!(bound.contains("  input who"), "{bound}");
        let rules = parse_names(&names(&held)).unwrap();
        assert_eq!(rules[0].inputs.len(), 1);
        assert_eq!(rules[0].inputs[0].name, "who");
        assert!(rules[0].inputs[0].required);

        let refused = output(
            engine.eval("bind urn:shout2 = source urn:plan:eval in=<urn:test:plan> nosuch=1"),
        )
        .unwrap_err();
        assert!(refused.contains("`nosuch`"), "{refused}");
        // A parameter the binding fixes is not also declared.
        output(engine.eval("bind urn:shout3 = source urn:plan:eval in=<urn:test:plan> who=x"))
            .unwrap();
        let rules = parse_names(&names(&held)).unwrap();
        let fixed = rules.iter().find(|r| r.from == "urn:shout3").unwrap();
        assert!(fixed.inputs.is_empty());
    }
}
