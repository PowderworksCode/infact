//! How much of a C tree the normalizer turns into comparable forms.
//!
//! ```sh
//! cargo run --release -p infact-c-normalize --example coverage -- <packs> <root>
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserCatalog, ParserRuntime};
use infact_c_normalize::normalize_file;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let packs = PathBuf::from(arguments.next().expect("packs dir"));
    let root = PathBuf::from(arguments.next().expect("root"));

    let discovery = ParserCatalog::discover([packs]);
    let pack = discovery
        .catalog
        .resolve("c", std::path::Path::new("probe.c"))
        .expect("a C pack");
    let runtime = ParserRuntime::new().expect("runtime");
    let parser = runtime.load(Arc::clone(pack)).expect("load");

    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|kind| kind == "c") {
                files.push(path);
            }
        }
    }
    files.sort();

    let (mut functions, mut traversals, mut comparable, mut trivial) =
        (0usize, 0usize, 0usize, 0usize);
    for path in &files {
        let Ok(source) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = parser.parse(path.clone(), source) else {
            continue;
        };
        for function in normalize_file(&parsed) {
            functions += 1;
            let rendered = format!("{:?}", function.form);
            if rendered.contains("Traverse") {
                traversals += 1;
            }
            if function.form.is_comparable() {
                comparable += 1;
            }
            if function.form.is_trivial() {
                trivial += 1;
            }
        }
    }
    println!("files:              {}", files.len());
    println!("functions:          {functions}");
    println!("with traversals:    {traversals}");
    println!("comparable forms:   {comparable}");
    println!("trivial forms:      {trivial}");
}
