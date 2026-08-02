#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What input evidence promises about facts derived before it grew a field.

use infact_core::InputEvidence;

/// Evidence written before queries were recorded still reads.
///
/// `queries_sha256` was added after facts were already being serialized, so a
/// fact pack or JSONL stream produced by an older build must not stop loading.
/// It reads as empty, which is the same thing a compiler-derived input says.
#[test]
fn evidence_without_a_query_digest_still_deserializes() {
    let older = r#"{
        "path": "src/lib.rs",
        "content_sha256": "aa",
        "parser_id": "tree-sitter-rust",
        "parser_version": "0.24.2",
        "grammar_sha256": "bb"
    }"#;
    let evidence: InputEvidence = serde_json::from_str(older).unwrap();
    assert_eq!(evidence.grammar_sha256, "bb");
    assert!(evidence.queries_sha256.is_empty());
}

/// A digest that is present survives a round trip.
#[test]
fn a_query_digest_round_trips() {
    let evidence = InputEvidence {
        path: "src/lib.rs".into(),
        content_sha256: "aa".to_owned(),
        parser_id: "tree-sitter-rust".to_owned(),
        parser_version: "0.24.2".to_owned(),
        grammar_sha256: "bb".to_owned(),
        queries_sha256: "cc".to_owned(),
    };
    let encoded = serde_json::to_string(&evidence).unwrap();
    assert_eq!(
        serde_json::from_str::<InputEvidence>(&encoded).unwrap(),
        evidence
    );
}
