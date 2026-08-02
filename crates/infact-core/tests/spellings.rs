//! The wire spelling of every fact enum.
//!
//! `serde(rename_all)` decides what a consumer actually reads, so these are
//! the strings that must not move. They are the spellings the hand-written
//! `as_str` arms produced before that mapping became a derive.
//!
//! Three of these also expose `as_str`, which straitjacket prints from
//! (`rules/error_discard.rs`, `config.rs`), so for those the two spellings
//! must additionally agree.

use infact_core::{Certainty, Containment, DiscardForm, Effect, Reach};

fn serializes<T: serde::Serialize>(value: &T, spelling: &str) {
    let json = serde_json::to_string(value).expect("serializing a fact enum");
    assert_eq!(json, format!("{spelling:?}"));
}

#[test]
fn discard_form_spellings() {
    for (value, spelling) in [
        (DiscardForm::LetUnderscore, "let-underscore"),
        (DiscardForm::OkDiscard, "ok-discard"),
        (DiscardForm::UnwrapOr, "unwrap-or"),
        (DiscardForm::ErrArm, "err-arm"),
        (DiscardForm::OkBinding, "ok-binding"),
        (DiscardForm::IteratorDrop, "iterator-drop"),
        (DiscardForm::CauseErased, "cause-erased"),
        (DiscardForm::Panic, "panic"),
    ] {
        serializes(&value, spelling);
        assert_eq!(value.as_str(), spelling);
    }
}

#[test]
fn containment_spellings() {
    for (value, spelling) in [
        (Containment::Fallible, "fallible"),
        (Containment::Optional, "optional"),
        (Containment::Infallible, "infallible"),
    ] {
        serializes(&value, spelling);
        assert_eq!(value.as_str(), spelling);
    }
}

#[test]
fn certainty_spellings() {
    for (value, spelling) in [
        (Certainty::Certain, "certain"),
        (Certainty::Possible, "possible"),
    ] {
        serializes(&value, spelling);
    }
}

#[test]
fn reach_spellings() {
    for (value, spelling) in [
        (Reach::Local, "local"),
        (Reach::Ancestor, "ancestor"),
        (Reach::Sealed, "sealed"),
        (Reach::Unknown, "unknown"),
    ] {
        serializes(&value, spelling);
    }
}

#[test]
fn effect_spellings() {
    for (value, spelling) in [
        (Effect::Allocate, "allocate"),
        (Effect::Block, "block"),
        (Effect::EnvironmentRead, "environment-read"),
        (Effect::EnvironmentWrite, "environment-write"),
        (Effect::FileRead, "file-read"),
        (Effect::FileWrite, "file-write"),
        (Effect::Network, "network"),
        (Effect::Process, "process"),
        (Effect::Random, "random"),
        (Effect::Time, "time"),
        (Effect::Unsafe, "unsafe"),
    ] {
        serializes(&value, spelling);
        assert_eq!(value.as_str(), spelling);
    }
}
