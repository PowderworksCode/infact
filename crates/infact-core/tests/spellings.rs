//! The wire spelling of every fact enum.
//!
//! `serde(rename_all)` and `strum(serialize_all)` state one rule twice, and
//! the strings below are what a consumer reads. These are the spellings the
//! hand-written `as_str` arms produced before the mapping became a derive.

use infact_core::{Certainty, Containment, DiscardForm, Effect, Reach};

/// Both spellings must agree, or a fact prints one way and serializes another.
fn agrees<T: serde::Serialize + Copy>(value: T, spelling: &str, as_str: fn(T) -> &'static str) {
    assert_eq!(as_str(value), spelling);
    let json = serde_json::to_string(&value).expect("serializing a fact enum");
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
        agrees(value, spelling, DiscardForm::as_str);
    }
}

#[test]
fn containment_spellings() {
    for (value, spelling) in [
        (Containment::Fallible, "fallible"),
        (Containment::Optional, "optional"),
        (Containment::Infallible, "infallible"),
    ] {
        agrees(value, spelling, Containment::as_str);
    }
}

#[test]
fn certainty_spellings() {
    for (value, spelling) in [
        (Certainty::Certain, "certain"),
        (Certainty::Possible, "possible"),
    ] {
        agrees(value, spelling, Certainty::as_str);
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
        agrees(value, spelling, Reach::as_str);
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
        agrees(value, spelling, Effect::as_str);
    }
}
