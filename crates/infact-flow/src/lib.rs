//! Label propagation over a graph, with the path that carried each label.
//!
//! Two analyses in this workspace ask the same question of different graphs.
//! Effects ask which callables reach an allocator, over a graph of calls.
//! Ownership asks where a field's value came from, over a graph of assignments.
//! Both seed some nodes, close the seeds over the edges, and then have to say
//! *why* — because a propagated label with no route back to its origin is a
//! claim a reader cannot check.
//!
//! That shared shape is this crate. It knows nothing about calls, effects,
//! types or languages: nodes are opaque identifiers, labels are opaque bytes,
//! and each analysis owns the meaning of both.
//!
//! ## Which way a label travels
//!
//! The two analyses appear to disagree about direction. An effect moves from a
//! callee up to its caller; a value moves from an assignment's right-hand side
//! into the field on its left. They are the same relation seen from opposite
//! ends, so [`Flow`] is phrased the way both read naturally: a label at `from`
//! flows `into` another node.
//!
//! ```text
//! effects     Flow { into: caller,      from: callee }
//! ownership   Flow { into: field,       from: allocation }
//! ```
//!
//! The witness search then walks `into -> from` until it reaches a seed, which
//! is the direction a reader traces the argument anyway: start at the claim,
//! end at the evidence.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use infact_core::SourceSpan;

/// An edge along which labels travel.
///
/// Read it as a sentence: a label at `from` flows `into` this node. The span is
/// where the edge is written in the source, which is what a witness quotes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Flow {
    pub into: u64,
    pub from: u64,
    pub span: SourceSpan,
}

/// A label a node carries directly, and where that was established.
///
/// The `origin` is not a node in the graph. It names the thing outside the
/// graph that justifies the label — a catalogued library call, an allocator, a
/// declaration — and it is what a witness terminates in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seed {
    pub node: u64,
    /// An analysis-private encoding. This crate never interprets it.
    pub label: u8,
    pub origin: String,
    pub span: SourceSpan,
}

/// One step of a witness: a node, what it drew the label from, and where.
///
/// Deliberately not [`infact_core::CallEdgeEvidence`], whose field names commit
/// to calls. An analysis converts at its own boundary, which costs a `map` and
/// keeps call vocabulary out of a crate that also carries assignments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WitnessEdge {
    pub subject: String,
    pub source: String,
    pub span: SourceSpan,
}

/// Why a node carries a label: the route from the claim back to its origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Witness {
    pub label: u8,
    pub origin: String,
    pub path: Vec<WitnessEdge>,
}

/// A node and a label it carries, whether seeded or inherited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Labelled {
    pub node: u64,
    pub label: u8,
}

ascent::ascent! {
    struct Closure;

    /// A label at `from` flows into `into`.
    relation flows(u64, u64);
    /// `node` carries `label`, whether seeded or inherited.
    relation carries(u64, u8);

    // a label reaches a node through any edge that reaches it
    carries(into, *label) <-- flows(into, from), carries(from, label);
}

/// A graph, its seeds, and a name for each node.
///
/// Names exist because a witness has to be readable. A node identifier is an
/// index; a reader needs the callable path or the field it stands for.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    flows: Vec<Flow>,
    seeds: Vec<Seed>,
    names: BTreeMap<u64, String>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Name a node, so witnesses through it can be read.
    ///
    /// A node with no name is not an error: the search declines to route a
    /// witness through it rather than inventing a label for it.
    pub fn name(&mut self, node: u64, name: impl Into<String>) -> &mut Self {
        self.names.insert(node, name.into());
        self
    }

    pub fn flow(&mut self, flow: Flow) -> &mut Self {
        self.flows.push(flow);
        self
    }

    pub fn seed(&mut self, seed: Seed) -> &mut Self {
        self.seeds.push(seed);
        self
    }

    pub fn extend_flows(&mut self, flows: impl IntoIterator<Item = Flow>) -> &mut Self {
        self.flows.extend(flows);
        self
    }

    pub fn extend_seeds(&mut self, seeds: impl IntoIterator<Item = Seed>) -> &mut Self {
        self.seeds.extend(seeds);
        self
    }

    /// Sort and deduplicate, so a run is reproducible whatever order the
    /// caller discovered the graph in.
    pub fn settle(&mut self) -> &mut Self {
        self.flows.sort();
        self.flows.dedup();
        self.seeds.sort();
        self.seeds.dedup();
        self
    }

    pub fn flows(&self) -> &[Flow] {
        &self.flows
    }

    pub fn seeds(&self) -> &[Seed] {
        &self.seeds
    }

    /// Every label each node carries, following edges transitively.
    ///
    /// Computed from scratch each time, which is all any caller has asked for:
    /// each one builds the whole relation once from a complete graph and drops
    /// it.
    pub fn propagate(&self) -> BTreeSet<Labelled> {
        let mut closure = Closure {
            flows: self
                .flows
                .iter()
                .map(|flow| (flow.into, flow.from))
                .collect(),
            carries: self
                .seeds
                .iter()
                .map(|seed| (seed.node, seed.label))
                .collect(),
            ..Closure::default()
        };
        closure.run();
        closure
            .carries
            .into_iter()
            .map(|(node, label)| Labelled { node, label })
            .collect()
    }

    /// A reusable view for asking many witness questions of one graph.
    ///
    /// Indexing the edges and seeds costs a pass over the graph, and a caller
    /// asking for a witness per propagated label asks tens of thousands of
    /// times. Build this once outside that loop.
    pub fn trace(&self) -> Trace<'_> {
        let mut outgoing = BTreeMap::<u64, Vec<&Flow>>::new();
        for flow in &self.flows {
            outgoing.entry(flow.into).or_default().push(flow);
        }
        let mut seeds = BTreeMap::<(u64, u8), Vec<&Seed>>::new();
        for seed in &self.seeds {
            seeds.entry((seed.node, seed.label)).or_default().push(seed);
        }
        Trace {
            names: &self.names,
            outgoing,
            seeds,
        }
    }

    /// Every place `label` is reached from `start`.
    ///
    /// Convenience for a one-off question. This indexes the whole graph per
    /// call, so a caller in a loop wants [`Graph::trace`] instead.
    pub fn witnesses(&self, start: u64, label: u8) -> Vec<Witness> {
        self.trace().witnesses(start, label)
    }

    /// The first witness, when a caller wants one reason rather than all of
    /// them.
    pub fn witness(&self, start: u64, label: u8) -> Option<Witness> {
        self.trace().witness(start, label)
    }
}

/// An indexed view of a graph, for repeated witness searches.
#[derive(Debug, Clone)]
pub struct Trace<'a> {
    names: &'a BTreeMap<u64, String>,
    outgoing: BTreeMap<u64, Vec<&'a Flow>>,
    seeds: BTreeMap<(u64, u8), Vec<&'a Seed>>,
}

impl Trace<'_> {
    /// Every place `label` is reached from `start`, not just the first.
    ///
    /// A node that does the thing twelve times has twelve reasons, and
    /// reporting one of them surfaces the rest a re-run at a time. This returns
    /// one witness per *seed* at the first node that carries any, which is
    /// bounded by how many times that node does the thing. It deliberately does
    /// not enumerate every route through the graph: the number of paths between
    /// two nodes is exponential, and a node three hops away does not need
    /// twelve spellings of the same news.
    pub fn witnesses(&self, start: u64, label: u8) -> Vec<Witness> {
        let mut queue = VecDeque::from([(start, Vec::<WitnessEdge>::new())]);
        let mut seen = BTreeSet::new();
        while let Some((current, path)) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            if let Some(reached) = self.seeds.get(&(current, label)) {
                let Some(subject) = self.names.get(&current) else {
                    continue;
                };
                return reached
                    .iter()
                    .map(|seed| {
                        let mut complete = path.clone();
                        complete.push(WitnessEdge {
                            subject: subject.clone(),
                            source: seed.origin.clone(),
                            span: seed.span.clone(),
                        });
                        Witness {
                            label,
                            origin: seed.origin.clone(),
                            path: complete,
                        }
                    })
                    .collect();
            }
            let Some(edges) = self.outgoing.get(&current) else {
                continue;
            };
            for flow in edges {
                let (Some(subject), Some(source)) =
                    (self.names.get(&flow.into), self.names.get(&flow.from))
                else {
                    continue;
                };
                let mut next = path.clone();
                next.push(WitnessEdge {
                    subject: subject.clone(),
                    source: source.clone(),
                    span: flow.span.clone(),
                });
                queue.push_back((flow.from, next));
            }
        }
        Vec::new()
    }

    pub fn witness(&self, start: u64, label: u8) -> Option<Witness> {
        self.witnesses(start, label).into_iter().next()
    }
}

/// What happened to every construct the graph builder walked past.
///
/// The point is the invariant, not the numbers: a builder that classifies each
/// construct into exactly one bucket can assert [`Accounting::balances`], and
/// then syntax it does not handle is loud rather than silently absent. An
/// analysis whose graph is missing edges reports "nothing found" in exactly the
/// same voice as one whose graph is complete, and this is what tells them
/// apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Accounting {
    /// Every construct considered.
    pub total: usize,
    /// Linked to another node in this graph.
    pub linked: usize,
    /// Seeded from knowledge outside the graph.
    pub seeded: usize,
    /// Recognized, and deliberately carries nothing.
    pub ignored: usize,
    /// Names something outside the corpus being analyzed.
    pub external: usize,
    /// Could refer to more than one thing, and syntax cannot choose.
    pub ambiguous: usize,
    /// Not placed at all.
    pub unknown: usize,
}

impl Accounting {
    pub fn accounted(&self) -> usize {
        self.linked + self.seeded + self.ignored + self.external + self.ambiguous + self.unknown
    }

    /// Whether every construct landed in exactly one bucket.
    pub fn balances(&self) -> bool {
        self.total == self.accounted()
    }

    /// Constructs that reached no node in the graph.
    pub fn unlinked(&self) -> usize {
        self.total.saturating_sub(self.linked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: u32) -> SourceSpan {
        SourceSpan {
            path: "t.zig".into(),
            start_byte: Some(u64::from(line)),
            end_byte: Some(u64::from(line) + 1),
            start_line: line,
            end_line: line,
            start_column: None,
            end_column: None,
        }
    }

    fn chain() -> Graph {
        let mut graph = Graph::new();
        graph
            .name(0, "handler")
            .name(1, "service")
            .name(2, "adapter")
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .flow(Flow {
                into: 1,
                from: 2,
                span: span(2),
            })
            .seed(Seed {
                node: 2,
                label: 7,
                origin: "std::fs::read".to_owned(),
                span: span(3),
            });
        graph
    }

    #[test]
    fn a_label_reaches_everything_that_flows_from_it() {
        let carried = chain().propagate();
        for node in [0, 1, 2] {
            assert!(carried.contains(&Labelled { node, label: 7 }), "{node}");
        }
    }

    #[test]
    fn a_label_does_not_travel_the_wrong_way() {
        let mut graph = Graph::new();
        graph
            .name(0, "caller")
            .name(1, "callee")
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .seed(Seed {
                node: 0,
                label: 1,
                origin: "seeded at the far end".to_owned(),
                span: span(1),
            });
        let carried = graph.propagate();
        assert!(carried.contains(&Labelled { node: 0, label: 1 }));
        assert!(
            !carried.contains(&Labelled { node: 1, label: 1 }),
            "a label flowed backwards along an edge"
        );
    }

    /// The witness is the point of the crate: it has to end at the origin and
    /// name every step in between.
    #[test]
    fn a_witness_runs_from_the_claim_back_to_the_origin() {
        let witness = chain().witness(0, 7).expect("a witness");
        assert_eq!(witness.origin, "std::fs::read");
        let steps: Vec<(&str, &str)> = witness
            .path
            .iter()
            .map(|edge| (edge.subject.as_str(), edge.source.as_str()))
            .collect();
        assert_eq!(
            steps,
            vec![
                ("handler", "service"),
                ("service", "adapter"),
                ("adapter", "std::fs::read"),
            ]
        );
    }

    #[test]
    fn a_node_that_does_the_thing_twice_has_two_witnesses() {
        let mut graph = chain();
        graph.seed(Seed {
            node: 2,
            label: 7,
            origin: "std::fs::write".to_owned(),
            span: span(4),
        });
        let origins: Vec<String> = graph
            .witnesses(0, 7)
            .into_iter()
            .map(|witness| witness.origin)
            .collect();
        assert_eq!(origins, vec!["std::fs::read", "std::fs::write"]);
    }

    #[test]
    fn a_cycle_does_not_hang_the_search() {
        let mut graph = Graph::new();
        graph
            .name(0, "a")
            .name(1, "b")
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .flow(Flow {
                into: 1,
                from: 0,
                span: span(2),
            });
        assert!(graph.witness(0, 3).is_none());
        assert!(graph.propagate().is_empty());
    }

    #[test]
    fn a_label_nothing_seeds_is_carried_by_nobody() {
        assert!(chain().witness(0, 9).is_none());
    }

    #[test]
    fn settling_makes_a_graph_independent_of_discovery_order() {
        let mut forward = Graph::new();
        forward
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .flow(Flow {
                into: 1,
                from: 2,
                span: span(2),
            })
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .settle();
        let mut backward = Graph::new();
        backward
            .flow(Flow {
                into: 1,
                from: 2,
                span: span(2),
            })
            .flow(Flow {
                into: 0,
                from: 1,
                span: span(1),
            })
            .settle();
        assert_eq!(forward.flows(), backward.flows());
    }

    #[test]
    fn accounting_balances_only_when_everything_is_placed() {
        let mut accounting = Accounting {
            total: 3,
            linked: 1,
            seeded: 1,
            ..Accounting::default()
        };
        assert!(!accounting.balances());
        accounting.unknown += 1;
        assert!(accounting.balances());
        assert_eq!(accounting.unlinked(), 2);
    }
}
