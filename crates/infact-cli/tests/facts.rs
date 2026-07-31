use std::path::PathBuf;
use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_infact"))
}

#[test]
fn validates_a_fact_pack_and_its_contents() {
    let manifest =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-itertools/pack.toml");
    let output = binary()
        .args(["facts", "validate"])
        .arg(manifest)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("rust-itertools 0.15.0 revision 1"));
    assert!(stdout.contains("sha256:"));
    assert!(stdout.contains("1 content blob(s)"));
}

#[test]
fn packages_a_fact_pack_as_an_oci_layout() {
    let manifest =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-itertools/pack.toml");
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("layout");
    let output = binary()
        .args(["facts", "package"])
        .arg(manifest)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_path.join("oci-layout").is_file());
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output_path.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["schemaVersion"], 2);
    assert_eq!(
        index["manifests"][0]["mediaType"],
        "application/vnd.oci.image.manifest.v1+json"
    );
}

#[test]
fn imports_and_lists_a_pack_in_the_local_cache() {
    let manifest =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-itertools/pack.toml");
    let directory = tempfile::tempdir().unwrap();
    let layout = directory.path().join("layout");
    let cache = directory.path().join("cache");
    let packaged = binary()
        .args(["facts", "package"])
        .arg(manifest)
        .arg("--output")
        .arg(&layout)
        .output()
        .unwrap();
    assert!(packaged.status.success());

    for _ in 0..2 {
        let imported = binary()
            .args(["facts", "cache", "import"])
            .arg(&layout)
            .arg("--cache")
            .arg(&cache)
            .output()
            .unwrap();
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
    }

    let listed = binary()
        .args(["facts", "cache", "list", "--cache"])
        .arg(&cache)
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let lines = String::from_utf8(listed.stdout).unwrap();
    assert_eq!(lines.lines().count(), 1);
    assert!(lines.contains("rust-itertools 0.15.0 revision 1  sha256:"));

    let digest = lines.split_whitespace().last().unwrap();
    let lock = directory.path().join("infact.lock.toml");
    let added = binary()
        .args(["facts", "lock", "add", "--lock"])
        .arg(&lock)
        .arg("--cache")
        .arg(&cache)
        .arg("--digest")
        .arg(digest)
        .args(["--origin", "local:test"])
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let locked = binary()
        .args(["facts", "lock", "list", "--lock"])
        .arg(&lock)
        .output()
        .unwrap();
    assert!(locked.status.success());
    assert!(
        String::from_utf8(locked.stdout)
            .unwrap()
            .contains("local:test")
    );
    let verified = binary()
        .args(["facts", "lock", "verify", "--lock"])
        .arg(&lock)
        .arg("--cache")
        .arg(&cache)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn checked_in_fact_packs_are_complete() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["rust-core", "rust-itertools", "rust-strum"] {
        let manifest = repository.join("fact-packs").join(name).join("pack.toml");
        let output = binary()
            .args(["facts", "validate"])
            .arg(manifest)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn hashes_source_trees_for_analyzer_provenance() {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-itertools/behaviors");
    let first = binary()
        .args(["facts", "hash"])
        .arg(&fixture)
        .output()
        .unwrap();
    let second = binary()
        .args(["facts", "hash"])
        .arg(&fixture)
        .output()
        .unwrap();
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    assert!(
        String::from_utf8(first.stdout)
            .unwrap()
            .starts_with("sha256:")
    );
}
