//! Finding where an effect a callable carries actually comes from.
//!
//! A propagated effect says a callable allocates; it does not say where. The
//! search walks the call graph outward until it reaches a seed, and the path it
//! took is the evidence a reader needs to check the claim.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use infact_core::{CallEdgeEvidence, CallEffectEvidence, Effect};

use crate::{EffectSeed, ResolvedCall};

pub(crate) fn evidence_path(
    start: u64,
    effect: Effect,
    calls: &BTreeMap<u64, Vec<&ResolvedCall>>,
    seeds: &BTreeMap<u64, Vec<&EffectSeed>>,
    callables: &BTreeMap<u64, String>,
) -> Option<CallEffectEvidence> {
    evidence_paths(start, effect, calls, seeds, callables)
        .into_iter()
        .next()
}

/// Every place the effect is reached from here, not just the first.
///
/// A callable that allocates in twelve places does it twelve times, and
/// reporting one of them surfaced the rest a re-run at a time. This returns one
/// evidence per SEED at the first callable that carries any, which is bounded by
/// how many times that callable does the thing. It deliberately does not
/// enumerate every route through the call graph: the number of paths between two
/// nodes is exponential, and a caller three hops away does not need twelve
/// spellings of the same news.
pub(crate) fn evidence_paths(
    start: u64,
    effect: Effect,
    calls: &BTreeMap<u64, Vec<&ResolvedCall>>,
    seeds: &BTreeMap<u64, Vec<&EffectSeed>>,
    // only each callable's path is needed, so the syntax-resolved and the
    // observation-resolved graphs can share this search
    callables: &BTreeMap<u64, String>,
) -> Vec<CallEffectEvidence> {
    let mut queue = VecDeque::from([(start, Vec::<CallEdgeEvidence>::new())]);
    let mut seen = BTreeSet::new();
    while let Some((current, path)) = queue.pop_front() {
        if !seen.insert(current) {
            continue;
        }
        let reached = seeds
            .get(&current)
            .map(|seeds| {
                seeds
                    .iter()
                    .filter(|seed| seed.effect == effect)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !reached.is_empty() {
            let Some(caller) = callables.get(&current) else {
                continue;
            };
            return reached
                .into_iter()
                .map(|seed| {
                    let mut complete = path.clone();
                    complete.push(CallEdgeEvidence {
                        caller: caller.clone(),
                        callee: seed.origin.clone(),
                        call: seed.span.clone(),
                    });
                    CallEffectEvidence {
                        effect,
                        origin: seed.origin.clone(),
                        path: complete,
                    }
                })
                .collect();
        }
        if let Some(outgoing) = calls.get(&current) {
            for call in outgoing {
                let Some(caller) = callables.get(&call.caller) else {
                    continue;
                };
                let Some(callee) = callables.get(&call.callee) else {
                    continue;
                };
                let mut next_path = path.clone();
                next_path.push(CallEdgeEvidence {
                    caller: caller.clone(),
                    callee: callee.clone(),
                    call: call.span.clone(),
                });
                queue.push_back((call.callee, next_path));
            }
        }
    }
    Vec::new()
}
