use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use entl_tree_sitter::ParsedFile;
use infact_core::InputEvidence;
use sha2::{Digest, Sha256};
use tree_sitter::Node;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxToken {
    pub ordinal: u32,
    pub kind: String,
    pub lexeme: Vec<u8>,
    pub class: TokenClass,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenClass {
    Other,
    Identifier,
    Literal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Normalization {
    pub identifiers: bool,
    pub literals: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TokenizedFile {
    pub path: PathBuf,
    pub comparison_domain: String,
    pub evidence: InputEvidence,
    pub tokens: Vec<SyntaxToken>,
    pub units: Vec<SyntaxUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SyntaxUnit {
    pub kind: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
}

pub(crate) fn tokenize(parsed: ParsedFile) -> Result<TokenizedFile> {
    let pack = &parsed.pack;
    let ignored = pack
        .manifest()
        .tokenization
        .ignored_node_kinds
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let identifiers = pack
        .manifest()
        .tokenization
        .identifier_node_kinds
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let literals = pack
        .manifest()
        .tokenization
        .literal_node_kinds
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let unit_kinds = pack
        .manifest()
        .tokenization
        .unit_node_kinds
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut collector = TokenCollector {
        source: &parsed.source,
        ignored: &ignored,
        identifiers: &identifiers,
        literals: &literals,
        unit_kinds: &unit_kinds,
        path: &parsed.path,
        tokens: Vec::new(),
        units: Vec::new(),
    };
    collector.collect(parsed.tree.root_node())?;
    let TokenCollector {
        mut tokens, units, ..
    } = collector;
    for (ordinal, token) in tokens.iter_mut().enumerate() {
        token.ordinal = u32::try_from(ordinal).map_err(|_| Error::SourceTooLarge {
            path: parsed.path.clone(),
        })?;
    }

    Ok(TokenizedFile {
        path: parsed.path.clone(),
        comparison_domain: pack.manifest().comparison_domain.clone(),
        evidence: InputEvidence {
            path: parsed.path,
            content_sha256: parsed.provenance.source_sha256,
            parser_id: parsed.provenance.parser_id,
            parser_version: parsed.provenance.parser_version,
            grammar_sha256: parsed.provenance.grammar_sha256,
        },
        tokens,
        units,
    })
}

struct TokenCollector<'a> {
    source: &'a [u8],
    ignored: &'a BTreeSet<&'a str>,
    identifiers: &'a BTreeSet<&'a str>,
    literals: &'a BTreeSet<&'a str>,
    unit_kinds: &'a BTreeSet<&'a str>,
    path: &'a PathBuf,
    tokens: Vec<SyntaxToken>,
    units: Vec<SyntaxUnit>,
}

impl TokenCollector<'_> {
    fn collect(&mut self, node: Node<'_>) -> Result<()> {
        if self.ignored.contains(node.kind()) || node.is_missing() {
            return Ok(());
        }

        if self.unit_kinds.contains(node.kind()) && node.start_byte() < node.end_byte() {
            self.units.push(SyntaxUnit {
                kind: node.kind().to_owned(),
                start_byte: u64::try_from(node.start_byte()).map_err(|_| {
                    Error::SourceTooLarge {
                        path: self.path.clone(),
                    }
                })?,
                end_byte: u64::try_from(node.end_byte()).map_err(|_| Error::SourceTooLarge {
                    path: self.path.clone(),
                })?,
                start_line: u32::try_from(node.start_position().row + 1).map_err(|_| {
                    Error::SourceTooLarge {
                        path: self.path.clone(),
                    }
                })?,
                end_line: u32::try_from(node.end_position().row + 1).map_err(|_| {
                    Error::SourceTooLarge {
                        path: self.path.clone(),
                    }
                })?,
            });
        }

        if node.child_count() == 0 {
            if node.start_byte() == node.end_byte() {
                return Ok(());
            }
            let start_byte =
                u64::try_from(node.start_byte()).map_err(|_| Error::SourceTooLarge {
                    path: self.path.clone(),
                })?;
            let end_byte = u64::try_from(node.end_byte()).map_err(|_| Error::SourceTooLarge {
                path: self.path.clone(),
            })?;
            let start_line = u32::try_from(node.start_position().row + 1).map_err(|_| {
                Error::SourceTooLarge {
                    path: self.path.clone(),
                }
            })?;
            let end_line =
                u32::try_from(node.end_position().row + 1).map_err(|_| Error::SourceTooLarge {
                    path: self.path.clone(),
                })?;
            self.tokens.push(SyntaxToken {
                ordinal: 0,
                kind: node.kind().to_owned(),
                lexeme: self.source[node.byte_range()].to_vec(),
                class: if self.identifiers.contains(node.kind()) {
                    TokenClass::Identifier
                } else if self.literals.contains(node.kind()) {
                    TokenClass::Literal
                } else {
                    TokenClass::Other
                },
                start_byte,
                end_byte,
                start_line,
                end_line,
            });
            return Ok(());
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.collect(child)?;
        }
        Ok(())
    }
}

pub(crate) fn token_digest(tokens: &[SyntaxToken]) -> String {
    digest(&token_identity(tokens, None))
}

pub(crate) fn normalized_token_identity(
    tokens: &[SyntaxToken],
    normalization: Normalization,
) -> Vec<u8> {
    token_identity(tokens, Some(normalization))
}

pub(crate) fn normalized_token_digest(
    tokens: &[SyntaxToken],
    normalization: Normalization,
) -> String {
    digest(&normalized_token_identity(tokens, normalization))
}

pub(crate) fn changed_token_count(left: &[SyntaxToken], right: &[SyntaxToken]) -> u32 {
    left.iter()
        .zip(right)
        .filter(|(left, right)| left.kind != right.kind || left.lexeme != right.lexeme)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn token_identity(tokens: &[SyntaxToken], normalization: Option<Normalization>) -> Vec<u8> {
    let mut identifiers = BTreeMap::<(&str, &[u8]), u32>::new();
    let mut literals = BTreeMap::<(&str, &[u8]), u32>::new();
    let mut next_identifier = 0_u32;
    let mut next_literal = 0_u32;
    let mut output = Vec::new();
    for token in tokens {
        write_part(&mut output, token.kind.as_bytes());
        match (token.class, normalization) {
            (TokenClass::Identifier, Some(normalization)) if normalization.identifiers => {
                output.push(b'i');
                let ordinal = canonical_ordinal(
                    &mut identifiers,
                    &mut next_identifier,
                    &token.kind,
                    &token.lexeme,
                );
                output.extend_from_slice(&ordinal.to_le_bytes());
            }
            (TokenClass::Literal, Some(normalization)) if normalization.literals => {
                output.push(b'l');
                let ordinal =
                    canonical_ordinal(&mut literals, &mut next_literal, &token.kind, &token.lexeme);
                output.extend_from_slice(&ordinal.to_le_bytes());
            }
            _ => {
                output.push(b'e');
                write_part(&mut output, &token.lexeme);
            }
        }
    }
    output
}

fn canonical_ordinal<'a>(
    values: &mut BTreeMap<(&'a str, &'a [u8]), u32>,
    next: &mut u32,
    kind: &'a str,
    lexeme: &'a [u8],
) -> u32 {
    *values.entry((kind, lexeme)).or_insert_with(|| {
        let ordinal = *next;
        *next = next.saturating_add(1);
        ordinal
    })
}

fn write_part(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn digest(identity: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity);
    hex(hasher.finalize())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_preserves_identifier_equality_patterns() {
        let repeated = vec![identifier("x"), other("+"), identifier("x")];
        let distinct = vec![identifier("a"), other("+"), identifier("b")];
        let normalization = Normalization {
            identifiers: true,
            literals: false,
        };

        assert_ne!(
            normalized_token_identity(&repeated, normalization),
            normalized_token_identity(&distinct, normalization)
        );
    }

    fn identifier(lexeme: &str) -> SyntaxToken {
        token("identifier", lexeme, TokenClass::Identifier)
    }

    fn other(lexeme: &str) -> SyntaxToken {
        token(lexeme, lexeme, TokenClass::Other)
    }

    fn token(kind: &str, lexeme: &str, class: TokenClass) -> SyntaxToken {
        SyntaxToken {
            ordinal: 0,
            kind: kind.to_owned(),
            lexeme: lexeme.as_bytes().to_vec(),
            class,
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
        }
    }
}
