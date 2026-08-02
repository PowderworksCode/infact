use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use entl_tree_sitter::{ParserCatalog, ParserRuntime};
use heck::{ToKebabCase, ToSnakeCase};
use infact_core::{DERIVED_MACRO_BEHAVIOR_SCHEMA, DerivedMacroBehavior, MacroBehavior, StringCase};
use sha2::{Digest, Sha256};
use tree_sitter::Node;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct MacroDerivationRequest<'a> {
    pub macro_package: &'a str,
    pub macro_version: &'a str,
    pub derive_path: &'a str,
    pub probe_source: &'a [u8],
    pub expansion: &'a [u8],
}

pub fn derive_macro_behavior(
    parsers: &ParserCatalog,
    request: MacroDerivationRequest<'_>,
) -> Result<DerivedMacroBehavior> {
    if !matches!(
        request.derive_path,
        "strum::Display" | "strum::AsRefStr" | "strum::VariantArray"
    ) {
        return Err(Error::UnsupportedDerivation {
            callable: request.derive_path.to_owned(),
        });
    }
    let pack = parsers
        .resolve("rust", Path::new("expanded.rs"))
        .ok_or(Error::MissingRustParser)?;
    let parser = ParserRuntime::new()?.load(pack.clone())?;
    let probe = parser.parse("probe.rs", Arc::<[u8]>::from(request.probe_source.to_vec()))?;
    if probe.tree.root_node().has_error() {
        return Err(Error::InvalidMacroProbe {
            derive: request.derive_path.to_owned(),
        });
    }
    let parsed = parser.parse("expanded.rs", Arc::<[u8]>::from(request.expansion.to_vec()))?;
    if parsed.tree.root_node().has_error() {
        return Err(Error::InvalidMacroExpansion {
            derive: request.derive_path.to_owned(),
        });
    }
    let probe_variants = enum_variants(probe.tree.root_node(), &probe.source).ok_or_else(|| {
        Error::UnsupportedMacroExpansion {
            derive: request.derive_path.to_owned(),
            reason: "probe does not contain one non-empty unit enum".to_owned(),
        }
    })?;
    let behavior = match request.derive_path {
        "strum::Display" | "strum::AsRefStr" => {
            let mappings = display_mappings(parsed.tree.root_node(), &parsed.source);
            if mappings.keys().cloned().collect::<BTreeSet<_>>()
                != probe_variants.iter().cloned().collect::<BTreeSet<_>>()
            {
                return Err(Error::UnsupportedMacroExpansion {
                    derive: request.derive_path.to_owned(),
                    reason: "expansion does not map every probe variant exactly once".to_owned(),
                });
            }
            let case = infer_case(&mappings).ok_or_else(|| Error::UnsupportedMacroExpansion {
                derive: request.derive_path.to_owned(),
                reason: "variant strings do not share a supported case conversion".to_owned(),
            })?;
            if request.derive_path == "strum::Display" {
                MacroBehavior::EnumDisplay { case }
            } else {
                MacroBehavior::EnumAsRefStr { case }
            }
        }
        "strum::VariantArray" => {
            if expanded_variant_array(parsed.tree.root_node(), &parsed.source) != probe_variants {
                return Err(Error::UnsupportedMacroExpansion {
                    derive: request.derive_path.to_owned(),
                    reason: "expansion array does not contain every probe variant in order"
                        .to_owned(),
                });
            }
            MacroBehavior::EnumVariantArray
        }
        _ => unreachable!("supported derive paths were checked above"),
    };
    Ok(DerivedMacroBehavior {
        schema: DERIVED_MACRO_BEHAVIOR_SCHEMA,
        macro_package: request.macro_package.to_owned(),
        macro_version: request.macro_version.to_owned(),
        derive_path: request.derive_path.to_owned(),
        probe_source_sha256: digest(request.probe_source),
        expansion_sha256: digest(request.expansion),
        behavior,
    })
}

fn enum_variants(root: Node<'_>, source: &[u8]) -> Option<Vec<String>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_item" {
            let body = node.child_by_field_name("body")?;
            let mut cursor = body.walk();
            let variants = body
                .named_children(&mut cursor)
                .map(|variant| {
                    (variant.kind() == "enum_variant" && variant.named_child_count() == 1)
                        .then(|| variant.child_by_field_name("name"))
                        .flatten()
                        .and_then(|name| text(name, source))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()?;
            return (!variants.is_empty()).then_some(variants);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    None
}

fn expanded_variant_array(root: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "const_item"
            && node
                .child_by_field_name("name")
                .and_then(|name| text(name, source))
                == Some("VARIANTS")
            && let Some(value) = node.child_by_field_name("value")
            && let Some(array) = descendant(value, "array_expression")
        {
            let mut cursor = array.walk();
            return array
                .named_children(&mut cursor)
                .filter_map(|variant| last_identifier(variant, source).map(str::to_owned))
                .collect();
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Vec::new()
}

fn descendant<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn display_mappings(root: Node<'_>, source: &[u8]) -> BTreeMap<String, String> {
    let mut mappings = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "match_arm"
            && let Some(pattern) = node.child_by_field_name("pattern")
            && let Some(variant) = last_identifier(pattern, source)
            && let Some(value) = first_string(node, source)
        {
            mappings.insert(variant.to_owned(), value.to_owned());
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    mappings
}

fn infer_case(mappings: &BTreeMap<String, String>) -> Option<StringCase> {
    if mappings.is_empty() {
        return None;
    }
    if mappings
        .iter()
        .all(|(variant, value)| variant.to_kebab_case() == *value)
    {
        return Some(StringCase::Kebab);
    }
    mappings
        .iter()
        .all(|(variant, value)| variant.to_snake_case() == *value)
        .then_some(StringCase::Snake)
}

fn last_identifier<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        return text(node, source);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|child| last_identifier(child, source))
        .last()
}

fn first_string<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "string_content" {
            return text(node, source);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    None
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}

fn digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
