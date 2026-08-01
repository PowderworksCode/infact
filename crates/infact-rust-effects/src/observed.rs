//! Effect traces derived from resolved semantic observations.
//!
//! The syntax analyzer has to guess where a call goes, and when it guesses
//! wrong it says nothing at all: `use std::fs; fs::read(path)` looks like a
//! call to something named `fs::read`, matches no catalog entry, and quietly
//! contributes no effect. A compiler resolved that call before the source was
//! ever written down, so when those observations are available the guessing
//! stops.
//!
//! Only the call graph changes. Seeding from verified catalogs, propagation,
//! and evidence paths are the same machinery the syntax path uses.

use std::collections::{BTreeMap, BTreeSet};

use entl_semantics::{CallEdge, Definition, Dispatch, EntityId, SemanticObservations};
use infact_core::{
    CallEffectCatalog, Derivation as FactDerivation, Effect, EffectTrace, Fact, InputEvidence,
    SourceSpan,
};

use crate::{
    CallAccounting, EffectSeed, RepositoryEffectDiagnostic, RepositoryEffectReport, ResolvedCall,
    Result, decode_effect, evidence_path, external_effects, propagate_effects,
};

/// A callable as the observations describe it.
struct ObservedCallable<'a> {
    id: u64,
    definition: &'a Definition,
    span: SourceSpan,
}

fn convert_span(span: &entl_semantics::Span) -> SourceSpan {
    SourceSpan {
        path: span.path.clone(),
        // A compiler reports lines and columns. Byte offsets belong to a
        // particular reading of the file, and inventing them would be worse
        // than saying there are none.
        start_byte: None,
        end_byte: None,
        start_line: span.start_line,
        end_line: span.end_line,
        start_column: Some(span.start_column),
        end_column: Some(span.end_column),
    }
}

/// Derive effect traces from resolved observations rather than from syntax.
pub fn analyze_observed_effects(
    observations: &SemanticObservations,
    catalogs: &[CallEffectCatalog],
) -> Result<RepositoryEffectReport> {
    let external = external_effects(catalogs);

    let mut callables = Vec::new();
    for definition in &observations.definitions {
        let Some(span) = definition.span.as_ref().map(convert_span) else {
            continue;
        };
        callables.push(ObservedCallable {
            id: u64::try_from(callables.len()).expect("callable index fits in u64"),
            definition,
            span,
        });
    }
    let by_entity = callables
        .iter()
        .map(|callable| (&callable.definition.id, callable.id))
        .collect::<BTreeMap<_, _>>();
    let by_id = callables
        .iter()
        .map(|callable| (callable.id, callable))
        .collect::<BTreeMap<_, _>>();

    let mut calls = Vec::new();
    let mut seeds = Vec::new();
    let mut diagnostics = Vec::new();
    let mut accounting = CallAccounting::default();

    for edge in &observations.call_edges {
        accounting.total += 1;
        let Some(&caller) = by_entity.get(&edge.from) else {
            // a call from something with no observed definition cannot be
            // attributed, and silently dropping it would understate the graph
            accounting.unknown += 1;
            diagnostics.push(RepositoryEffectDiagnostic {
                path: edge.span.path.clone(),
                line: edge.span.start_line,
                message: format!("call from unobserved definition {}", edge.from.as_str()),
            });
            continue;
        };
        if edge.to.is_empty() {
            accounting.dynamic_or_ambiguous += 1;
            diagnostics.push(RepositoryEffectDiagnostic {
                path: edge.span.path.clone(),
                line: edge.span.start_line,
                message: format!(
                    "call in {} was resolved to no destination",
                    edge.from.as_str()
                ),
            });
            continue;
        }

        let span = convert_span(&edge.span);
        let mut classified = false;
        for target in &edge.to {
            // an external destination the catalog vouches for is an effect origin
            if let Some(effects) = external_effect(&external, target) {
                for effect in effects {
                    seeds.push(EffectSeed {
                        callable: caller,
                        effect: *effect,
                        origin: target.as_str().to_owned(),
                        span: span.clone(),
                    });
                }
                if !classified {
                    accounting.known_effect_origins += 1;
                    classified = true;
                }
                continue;
            }
            // a destination inside this unit is an edge to propagate along
            if let Some(&callee) = by_entity.get(target) {
                calls.push(ResolvedCall {
                    caller,
                    callee,
                    span: span.clone(),
                });
                if !classified {
                    accounting.linked_internal += 1;
                    classified = true;
                }
            }
        }
        if !classified {
            // resolved, but to something neither catalogued nor local: an
            // external call whose effects are simply unknown
            accounting.outside_selected_corpus += 1;
            if edge.dispatch == Dispatch::Unknown {
                diagnostics.push(RepositoryEffectDiagnostic {
                    path: edge.span.path.clone(),
                    line: edge.span.start_line,
                    message: format!(
                        "call in {} has an undetermined destination",
                        edge.from.as_str()
                    ),
                });
            }
        }
    }

    calls.sort();
    calls.dedup();
    seeds.sort();
    seeds.dedup();

    let propagated = propagate_effects(&calls, &seeds)?;
    let calls_by_caller = calls.iter().fold(
        BTreeMap::<u64, Vec<&ResolvedCall>>::new(),
        |mut by_caller, call| {
            by_caller.entry(call.caller).or_default().push(call);
            by_caller
        },
    );
    let seeds_by_callable = seeds.iter().fold(
        BTreeMap::<u64, Vec<&EffectSeed>>::new(),
        |mut by_callable, seed| {
            by_callable.entry(seed.callable).or_default().push(seed);
            by_callable
        },
    );

    let mut effects = Vec::new();
    for relation in propagated {
        let effect = decode_effect(relation.effect);
        let Some(callable) = by_id.get(&relation.callable) else {
            continue;
        };
        let Some(evidence) = observed_evidence(
            callable.id,
            effect,
            &calls_by_caller,
            &seeds_by_callable,
            &by_id,
        ) else {
            continue;
        };
        let value = EffectTrace {
            callable: callable.definition.id.as_str().to_owned(),
            callable_span: callable.span.clone(),
            effect,
            origin: evidence.origin,
            path: evidence.path,
        };
        effects.push(Fact {
            derivation: derivation(observations),
            value,
        });
    }
    effects.sort();
    effects.dedup();

    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.line.cmp(&right.line))
            .then(left.message.cmp(&right.message))
    });
    diagnostics.dedup();

    Ok(RepositoryEffectReport {
        effects,
        diagnostics,
        calls: accounting,
    })
}

/// The effects a catalog records for a resolved destination.
///
/// The destination is already canonical, so this is an exact lookup rather than
/// the prefix matching the syntax path needs.
fn external_effect<'a>(
    external: &'a BTreeMap<String, Vec<Effect>>,
    target: &EntityId,
) -> Option<&'a [Effect]> {
    external.get(target.as_str()).map(Vec::as_slice)
}

/// Reuses the syntax path's evidence search over the observed graph.
fn observed_evidence(
    start: u64,
    effect: Effect,
    calls: &BTreeMap<u64, Vec<&ResolvedCall>>,
    seeds: &BTreeMap<u64, Vec<&EffectSeed>>,
    callables: &BTreeMap<u64, &ObservedCallable<'_>>,
) -> Option<infact_core::CallEffectEvidence> {
    let named = callables
        .iter()
        .map(|(id, callable)| (*id, callable.definition.id.as_str().to_owned()))
        .collect::<BTreeMap<_, _>>();
    evidence_path(start, effect, calls, seeds, &named)
}

fn derivation(observations: &SemanticObservations) -> FactDerivation {
    FactDerivation {
        analyzer: "infact-rust-effects.observed".to_owned(),
        analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
        inputs: vec![InputEvidence {
            path: std::path::PathBuf::from(&observations.provenance.unit),
            content_sha256: String::new(),
            parser_id: observations.provenance.provider.clone(),
            parser_version: observations.provenance.provider_version.clone(),
            grammar_sha256: observations.provenance.toolchain.clone(),
        }],
    }
}

/// Destinations the observations resolved but nothing explains.
///
/// Exposed so a consumer can report how much of the graph was understood.
pub fn unexplained_destinations(
    observations: &SemanticObservations,
    catalogs: &[CallEffectCatalog],
) -> BTreeSet<String> {
    let external = external_effects(catalogs);
    let local = observations
        .definitions
        .iter()
        .map(|definition| definition.id.as_str())
        .collect::<BTreeSet<_>>();
    observations
        .call_edges
        .iter()
        .flat_map(|edge: &CallEdge| edge.to.iter())
        .map(EntityId::as_str)
        .filter(|target| !external.contains_key(*target) && !local.contains(target))
        .map(str::to_owned)
        .collect()
}
