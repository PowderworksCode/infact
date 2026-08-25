use std::collections::BTreeSet;

use infact_core::{
    CallableContainer, CallableParameter, CallableSignature, EXTERNAL_CATALOG_SCHEMA,
    ExternalBound, ExternalCallable, ExternalCatalog, ExternalType, TypeRequirement,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy)]
pub struct CatalogRequest<'a> {
    pub package: &'a str,
    pub version: &'a str,
    pub rustdoc_json: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid rustdoc JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rustdoc JSON is missing {0}")]
    Missing(&'static str),
    #[error("rustdoc JSON contains an invalid {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn build_catalog(request: CatalogRequest<'_>) -> Result<ExternalCatalog> {
    let document: Value = serde_json::from_slice(request.rustdoc_json)?;
    let root = object(&document, "document")?;
    let rustdoc_format = number(root.get("format_version"), "format_version")?;
    let index = object_value(root.get("index"), "index")?;
    let paths = object_value(root.get("paths"), "paths")?;
    let mut callables = Vec::new();
    let associated_items = associated_item_ids(index);

    for (id, item) in index {
        let item = object(item, "item")?;
        let Some(trait_value) = item
            .get("inner")
            .and_then(Value::as_object)
            .and_then(|inner| inner.get("trait"))
        else {
            continue;
        };
        let trait_item = object(trait_value, "trait")?;
        let trait_path = public_path(paths, id).unwrap_or_else(|| {
            format!(
                "{}::{}",
                request.package,
                string(item.get("name")).unwrap_or("<unnamed>")
            )
        });
        let Some(method_ids) = trait_item.get("items").and_then(Value::as_array) else {
            continue;
        };
        for method_id in method_ids {
            let method_id = method_id
                .as_u64()
                .ok_or(Error::Invalid("trait method ID"))?
                .to_string();
            let Some(method) = index.get(&method_id).and_then(Value::as_object) else {
                continue;
            };
            let Some(function) = method
                .get("inner")
                .and_then(Value::as_object)
                .and_then(|inner| inner.get("function"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            let Some(name) = string(method.get("name")) else {
                continue;
            };
            callables.push(ExternalCallable {
                path: format!("{trait_path}::{name}"),
                container: CallableContainer::Trait {
                    path: trait_path.clone(),
                },
                signature: Some(signature(function)?),
            });
        }
    }

    // Methods written on a type rather than in a trait. They are associated
    // items, so the free-function pass below skips them, and they belong to no
    // trait, so the pass above never saw them: `Vec::push` and `<[T]>::sorted`
    // fell between the two and the catalog held only what a library declared in
    // traits. For core that was five thousand methods, and for the standard
    // library it is most of what anyone calls.
    for item in index.values() {
        let item = object(item, "item")?;
        let Some(implementation) = item
            .get("inner")
            .and_then(Value::as_object)
            .and_then(|inner| inner.get("impl"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        // A trait implementation's methods belong to the trait, which the pass
        // above already catalogued from its declaration.
        if implementation
            .get("trait")
            .is_some_and(|value| !value.is_null())
        {
            continue;
        }
        let Some(container) = implemented_type(implementation.get("for"), paths) else {
            continue;
        };
        for method_id in implementation
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(method) = method_id
                .as_u64()
                .and_then(|id| index.get(&id.to_string()))
                .and_then(Value::as_object)
            else {
                continue;
            };
            let Some(function) = method
                .get("inner")
                .and_then(Value::as_object)
                .and_then(|inner| inner.get("function"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            let Some(name) = string(method.get("name")) else {
                continue;
            };
            callables.push(ExternalCallable {
                path: format!("{container}::{name}"),
                container: CallableContainer::Type {
                    path: container.clone(),
                },
                signature: Some(signature(function)?),
            });
        }
    }

    for (id, item) in index {
        if associated_items.contains(id) {
            continue;
        }
        let item = object(item, "item")?;
        let Some(function) = item
            .get("inner")
            .and_then(Value::as_object)
            .and_then(|inner| inner.get("function"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(path) = public_path(paths, id)
            .filter(|path| path.starts_with(&format!("{}::", request.package)))
        else {
            continue;
        };
        let module = path
            .rsplit_once("::")
            .map(|(module, _)| module)
            .unwrap_or(request.package)
            .to_owned();
        callables.push(ExternalCallable {
            path,
            container: CallableContainer::Module { path: module },
            signature: Some(signature(function)?),
        });
    }

    callables.sort();
    callables.dedup();
    Ok(ExternalCatalog {
        schema: EXTERNAL_CATALOG_SCHEMA,
        package: request.package.to_owned(),
        version: request.version.to_owned(),
        rustdoc_format,
        source_sha256: hex(Sha256::digest(request.rustdoc_json)),
        callables,
    })
}

fn associated_item_ids(index: &Map<String, Value>) -> BTreeSet<String> {
    index
        .values()
        .filter_map(Value::as_object)
        .filter_map(|item| item.get("inner").and_then(Value::as_object))
        .flat_map(|inner| [inner.get("trait"), inner.get("impl")])
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|container| container.get("items").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_u64)
        .map(|id| id.to_string())
        .collect()
}

fn signature(function: &Map<String, Value>) -> Result<CallableSignature> {
    let sig = object_value(function.get("sig"), "function signature")?;
    let inputs = sig
        .get("inputs")
        .and_then(Value::as_array)
        .ok_or(Error::Missing("function inputs"))?
        .iter()
        .map(|input| {
            let input = input.as_array().ok_or(Error::Invalid("function input"))?;
            if input.len() != 2 {
                return Err(Error::Invalid("function input"));
            }
            Ok(CallableParameter {
                name: string(input.first())
                    .ok_or(Error::Invalid("parameter name"))?
                    .to_owned(),
                ty: external_type(input.get(1).ok_or(Error::Invalid("parameter type"))?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let output = sig
        .get("output")
        .filter(|output| !output.is_null())
        .map(external_type)
        .transpose()?;
    let requirements = function
        .get("generics")
        .and_then(Value::as_object)
        .and_then(|generics| generics.get("where_predicates"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|predicate| predicate.get("bound_predicate"))
        .map(requirement)
        .collect::<Result<Vec<_>>>()?;
    Ok(CallableSignature {
        inputs,
        output,
        requirements,
    })
}

fn requirement(value: &Value) -> Result<TypeRequirement> {
    let predicate = object(value, "bound predicate")?;
    let subject = external_type(
        predicate
            .get("type")
            .ok_or(Error::Missing("bound subject"))?,
    )?;
    let bounds = predicate
        .get("bounds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(external_bound)
        .collect();
    Ok(TypeRequirement { subject, bounds })
}

fn external_bound(value: &Value) -> Option<ExternalBound> {
    let bound = value.as_object()?;
    if let Some(trait_bound) = bound.get("trait_bound")?.as_object() {
        let path = trait_bound.get("trait")?.get("path")?.as_str()?;
        return Some(ExternalBound::Trait {
            path: path.to_owned(),
        });
    }
    bound
        .get("outlives")
        .and_then(Value::as_str)
        .map(|name| ExternalBound::Lifetime {
            name: name.to_owned(),
        })
}

fn external_type(value: &Value) -> Result<ExternalType> {
    let ty = object(value, "type")?;
    if let Some(name) = ty.get("generic").and_then(Value::as_str) {
        return Ok(ExternalType::Generic {
            name: name.to_owned(),
        });
    }
    if let Some(name) = ty.get("primitive").and_then(Value::as_str) {
        return Ok(ExternalType::Primitive {
            name: name.to_owned(),
        });
    }
    if let Some(path) = ty.get("resolved_path").and_then(Value::as_object) {
        return Ok(ExternalType::Path {
            path: string(path.get("path"))
                .ok_or(Error::Missing("resolved type path"))?
                .to_owned(),
            arguments: type_arguments(path.get("args"))?,
        });
    }
    if let Some(reference) = ty.get("borrowed_ref").and_then(Value::as_object) {
        return Ok(ExternalType::Reference {
            mutable: reference
                .get("is_mutable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            inner: Box::new(external_type(
                reference
                    .get("type")
                    .ok_or(Error::Missing("referenced type"))?,
            )?),
        });
    }
    if let Some(qualified) = ty.get("qualified_path").and_then(Value::as_object) {
        return Ok(ExternalType::Associated {
            name: string(qualified.get("name"))
                .ok_or(Error::Missing("associated type name"))?
                .to_owned(),
            self_type: Box::new(external_type(
                qualified
                    .get("self_type")
                    .ok_or(Error::Missing("associated self type"))?,
            )?),
            trait_path: qualified
                .get("trait")
                .and_then(Value::as_object)
                .and_then(|item| item.get("path"))
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        });
    }
    if let Some(elements) = ty.get("tuple").and_then(Value::as_array) {
        return Ok(ExternalType::Tuple {
            elements: elements
                .iter()
                .map(external_type)
                .collect::<Result<Vec<_>>>()?,
        });
    }
    if let Some(element) = ty.get("slice") {
        return Ok(ExternalType::Slice {
            element: Box::new(external_type(element)?),
        });
    }
    if let Some(array) = ty.get("array").and_then(Value::as_object) {
        return Ok(ExternalType::Array {
            element: Box::new(external_type(
                array.get("type").ok_or(Error::Missing("array type"))?,
            )?),
            length: array.get("len").map(Value::to_string).unwrap_or_default(),
        });
    }
    if ty.contains_key("infer") {
        return Ok(ExternalType::Infer);
    }
    if ty.contains_key("never") {
        return Ok(ExternalType::Never);
    }
    Ok(ExternalType::Unsupported {
        kind: ty.keys().next().cloned().unwrap_or_default(),
    })
}

fn type_arguments(value: Option<&Value>) -> Result<Vec<ExternalType>> {
    let Some(angle) = value
        .and_then(Value::as_object)
        .and_then(|args| args.get("angle_bracketed"))
        .and_then(Value::as_object)
    else {
        return Ok(Vec::new());
    };
    angle
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|argument| argument.get("type"))
        .map(external_type)
        .collect()
}

/// The name of the type an inherent implementation is written on.
///
/// A nominal type is named by the path the crate publishes it under. A built-in
/// one has no path to look up and is named the way the language names it, which
/// is also how its documentation is addressed: `slice`, `str`, `u32`. Anything
/// else — a raw pointer, a function type — has no name a caller would write a
/// method path with, and is left out rather than given an invented one.
fn implemented_type(value: Option<&Value>, paths: &Map<String, Value>) -> Option<String> {
    let target = value?.as_object()?;
    if let Some(resolved) = target.get("resolved_path").and_then(Value::as_object) {
        let id = resolved.get("id")?.as_u64()?.to_string();
        // The published path is what a caller writes. Falling back to the bare
        // name would put two types with one name under a single heading.
        return public_path(paths, &id);
    }
    if let Some(primitive) = target.get("primitive").and_then(Value::as_str) {
        return Some(primitive.to_owned());
    }
    if target.contains_key("slice") {
        return Some("slice".to_owned());
    }
    if target.contains_key("array") {
        return Some("array".to_owned());
    }
    None
}

fn public_path(paths: &Map<String, Value>, id: &str) -> Option<String> {
    paths
        .get(id)?
        .get("path")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("::"))
}

fn object<'a>(value: &'a Value, name: &'static str) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or(Error::Invalid(name))
}

fn object_value<'a>(
    value: Option<&'a Value>,
    name: &'static str,
) -> Result<&'a Map<String, Value>> {
    value.and_then(Value::as_object).ok_or(Error::Missing(name))
}

fn string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

fn number(value: Option<&Value>, name: &'static str) -> Result<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| value.try_into().ok()) // straitjacket-allow:error-discard — overflow and absence are both Invalid, which is returned
        .ok_or(Error::Invalid(name))
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
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_trait_methods_and_free_functions() {
        let document = json!({
            "format_version": 60,
            "index": {
                "1": {
                    "name": "ExampleTools",
                    "inner": {"trait": {"items": [2]} }
                },
                "2": {
                    "name": "render",
                    "inner": {"function": {
                        "sig": {
                            "inputs": [
                                ["self", {"generic": "Self"}],
                                ["separator", {"borrowed_ref": {
                                    "is_mutable": false, // straitjacket-allow:deep-nesting — fixture mirrors rustdoc's own shape
                                    "type": {"primitive": "str"}
                                }}]
                            ],
                            "output": {"resolved_path": {"path": "String", "args": null}}
                        },
                        "generics": {"where_predicates": []}
                    }}
                },
                "3": {
                    "name": "render_all",
                    "inner": {"function": {
                        "sig": {
                            "inputs": [["value", {"primitive": "str"}]],
                            "output": {"resolved_path": {"path": "String", "args": null}}
                        },
                        "generics": {"where_predicates": []}
                    }}
                }
            },
            "paths": {
                "1": {"path": ["example", "ExampleTools"]},
                "3": {"path": ["example", "render_all"]}
            }
        });
        let source = serde_json::to_vec(&document).unwrap();
        let catalog = build_catalog(CatalogRequest {
            package: "example",
            version: "1.2.3",
            rustdoc_json: &source,
        })
        .unwrap();

        assert_eq!(catalog.rustdoc_format, 60);
        assert_eq!(catalog.callables.len(), 2);
        assert_eq!(catalog.callables[0].path, "example::ExampleTools::render");
        assert!(matches!(
            catalog.callables[0].signature.as_ref().unwrap().output,
            Some(ExternalType::Path { ref path, .. }) if path == "String"
        ));
        assert_eq!(catalog.callables[1].path, "example::render_all");
        assert_eq!(
            catalog.callables[1].container,
            CallableContainer::Module {
                path: "example".to_owned()
            }
        );
    }
}
