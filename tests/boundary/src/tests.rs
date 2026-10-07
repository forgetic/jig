use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct CrateManifest {
    path: PathBuf,
    name: String,
    dependencies: Vec<String>,
}

fn kit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("kit root exists")
}

fn source_files(dir: &Path, suffixes: &[&str], out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("kit directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            source_files(&path, suffixes, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| suffixes.iter().any(|suffix| name.ends_with(suffix)))
        {
            out.push(path);
        }
    }
}

fn quoted_value(value: &str) -> Option<&str> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    value[1..].split_once(quote).map(|(name, _)| name)
}

fn dependency_name(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim().split_once('.').map_or(key.trim(), |(name, _)| name);
    let key = key.trim_matches('"').trim_matches('\'');
    if key.is_empty() {
        return None;
    }
    let package =
        value.split_once("package").and_then(|(_, rest)| rest.split_once('=')).and_then(|(_, rest)| quoted_value(rest));
    Some(package.unwrap_or(key).to_owned())
}

fn parse_manifest(path: PathBuf, source: &str) -> CrateManifest {
    let mut section = String::new();
    let mut name = String::new();
    let mut dependencies = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
        } else if section == "package" && line.starts_with("name =") {
            name = line
                .split_once('=')
                .and_then(|(_, value)| quoted_value(value))
                .expect("package name is quoted")
                .to_owned();
        } else if (section == "dependencies" || section.ends_with(".dependencies"))
            && !line.starts_with('#')
            && let Some(dependency) = dependency_name(line)
        {
            dependencies.push(dependency);
        }
    }
    assert!(!name.is_empty(), "{} has no package name", path.display());
    CrateManifest { path, name, dependencies }
}

fn manifests() -> Vec<CrateManifest> {
    let mut files = Vec::new();
    source_files(&kit_root(), &["Cargo.toml"], &mut files);
    files
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path).expect("manifest is readable");
            parse_manifest(path, &source)
        })
        .collect()
}

fn breaches(manifests: &[CrateManifest], role: fn(&CrateManifest) -> bool) -> Vec<String> {
    manifests
        .iter()
        .filter(|manifest| role(manifest))
        .flat_map(|manifest| {
            manifest
                .dependencies
                .iter()
                .filter(|dependency| dependency.starts_with(&["tem", "per"].concat()))
                .map(|dependency| format!("{}: {dependency}", manifest.path.display()))
        })
        .collect()
}

fn all_crates(_: &CrateManifest) -> bool {
    true
}

fn forbidden_words() -> Vec<String> {
    [
        vec!["tem", "per"],
        vec!["for", "ge"],
        vec!["for", "gejo"],
        vec!["repo", "sitory"],
        vec!["repo", "sitories"],
        vec!["bra", "nch"],
        vec!["bra", "nches"],
        vec!["pull", " request"],
        vec!["g", "it"],
        vec!["wi", "ki"],
    ]
    .into_iter()
    .map(|parts| parts.concat())
    .collect()
}

fn word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

fn vocabulary_breaches(path: &Path, source: &str, words: &[String]) -> Vec<String> {
    let lower = source.to_ascii_lowercase();
    let mut breaches = Vec::new();
    for word in words {
        for (index, _) in lower.match_indices(word) {
            let before = source[..index].chars().next_back();
            let after = source[index + word.len()..].chars().next();
            let end_boundary = !after.is_some_and(word_char) || after.is_some_and(char::is_uppercase);
            if !before.is_some_and(word_char) && end_boundary {
                breaches.push(format!("{}: {word}", path.display()));
            }
        }
    }
    breaches
}

#[test]
fn no_kit_crate_depends_on_an_application_crate_even_when_empty() {
    let failures = breaches(&manifests(), all_crates);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn core_and_children_use_only_the_kit_and_foundation_even_when_empty() {
    let failures: Vec<_> = manifests()
        .into_iter()
        .filter(|manifest| manifest.name == "jig-core" || manifest.name.starts_with("jig-core-"))
        .flat_map(|manifest| {
            manifest
                .dependencies
                .iter()
                .filter(|name| {
                    if manifest.name == "jig-core" {
                        **name != "skein-lib" && !name.starts_with("jig-core-")
                    } else {
                        **name != "skein-lib"
                    }
                })
                .map(|name| format!("{}: {name}", manifest.path.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn hosts_use_no_core_crates_even_when_empty() {
    let failures: Vec<_> = manifests()
        .into_iter()
        .filter(|manifest| manifest.name == "jig-local-host" || manifest.name == "jig-worker-host")
        .flat_map(|manifest| {
            manifest
                .dependencies
                .iter()
                .filter(|name| name.starts_with("jig-core"))
                .map(|name| format!("{}: {name}", manifest.path.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn kit_sources_name_no_application_or_system_even_when_empty() {
    let mut files = Vec::new();
    source_files(&kit_root(), &[".rs", "Cargo.toml"], &mut files);
    let words = forbidden_words();
    let failures: Vec<_> = files
        .iter()
        .flat_map(|path| {
            let source = fs::read_to_string(path).expect("source is readable");
            vocabulary_breaches(path, &source, &words)
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn fixtures_name_the_path_and_the_breach() {
    let forbidden = ["tem", "per"].concat();
    let manifest =
        format!("[package]\nname = \"jig-sample\"\n[dependencies]\n{forbidden} = {{ path = \"../example\" }}\n");
    let parsed = parse_manifest(PathBuf::from("fixture/Cargo.toml"), &manifest);
    let failures = breaches(&[parsed], all_crates);
    assert_eq!(failures, [format!("fixture/Cargo.toml: {forbidden}")]);

    let workspace_manifest =
        format!("[package]\nname = \"jig-sample\"\n[dependencies]\n{forbidden}.workspace = true\n");
    let parsed = parse_manifest(PathBuf::from("fixture/workspace/Cargo.toml"), &workspace_manifest);
    let failures = breaches(&[parsed], all_crates);
    assert_eq!(failures, [format!("fixture/workspace/Cargo.toml: {forbidden}")]);

    let word = ["for", "ge"].concat();
    let source = format!("const SUBJECT: &str = \"{word}\";");
    let failures = vocabulary_breaches(Path::new("fixture/src/lib.rs"), &source, &forbidden_words());
    assert_eq!(failures, [format!("fixture/src/lib.rs: {word}")]);
}
