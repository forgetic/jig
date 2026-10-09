use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct CrateManifest {
    path: PathBuf,
    name: Option<String>,
    dependencies: Vec<String>,
}

fn kit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("kit root exists")
}

fn walk_directory(name: &str, at_root: bool) -> bool {
    !(name.starts_with('.') || (at_root && name == "target"))
}

fn source_files(dir: &Path, at_root: bool, suffixes: &[&str], out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("kit directory is readable") {
        let entry = entry.expect("directory entry is readable");
        let kind = entry.file_type().expect("directory entry has a file type");
        let path = entry.path();
        if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
            if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| walk_directory(name, at_root)) {
                source_files(&path, false, suffixes, out);
            }
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
    let mut name = None;
    let mut dependencies = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
        } else if section == "package" && line.starts_with("name =") {
            name = Some(
                line.split_once('=')
                    .and_then(|(_, value)| quoted_value(value))
                    .expect("package name is quoted")
                    .to_owned(),
            );
        } else if (section == "dependencies" || section.ends_with(".dependencies"))
            && !line.starts_with('#')
            && let Some(dependency) = dependency_name(line)
        {
            dependencies.push(dependency);
        }
    }
    assert!(
        name.as_ref().is_some_and(|name| !name.is_empty()) || workspace_manifest(source),
        "{} has no package name",
        path.display()
    );
    CrateManifest { path, name, dependencies }
}

fn workspace_manifest(source: &str) -> bool {
    let mut workspace = false;
    for line in source.lines().map(str::trim) {
        let section = line.split('#').next().expect("line has a first part").trim();
        if section == "[package]" {
            return false;
        }
        workspace |= section == "[workspace]";
    }
    workspace
}

fn quoted_end(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let quote = bytes[start];
    let mut end = start + 1;
    while end < bytes.len() {
        if bytes[end] == quote {
            return end + 1;
        }
        if quote == b'"' && bytes[end] == b'\\' {
            end += 1;
        }
        end += 1;
    }
    bytes.len()
}

fn dependency_source_end(source: &str, key_end: usize) -> Option<usize> {
    let tail = source[key_end..].trim_start_matches([' ', '\t']);
    let value = tail.strip_prefix('=')?.trim_start_matches([' ', '\t']);
    if !value.starts_with(['"', '\'']) {
        return None;
    }
    let start = source.len() - value.len();
    Some(quoted_end(source, start))
}

fn workspace_vocabulary(source: &str) -> String {
    let key = ["g", "it"].concat();
    let mut vocabulary = String::new();
    let mut start = 0;
    while start < source.len() {
        let tail = &source[start..];
        let ch = tail.chars().next().expect("source has remaining text");
        let end = if ch == '#' {
            tail.find('\n').map_or(source.len(), |end| start + end)
        } else if ch == '"' || ch == '\'' {
            quoted_end(source, start)
        } else if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            start + tail.find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-').unwrap_or(tail.len())
        } else {
            start + ch.len_utf8()
        };
        let token = &source[start..end];
        let is_key = token == key || quoted_value(token) == Some(key.as_str());
        if is_key && let Some(pair_end) = dependency_source_end(source, end) {
            start = pair_end;
        } else {
            vocabulary.push_str(token);
            start = end;
        }
    }
    vocabulary
}

fn manifests() -> Vec<CrateManifest> {
    let mut files = Vec::new();
    source_files(&kit_root(), true, &["Cargo.toml"], &mut files);
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

fn vocabulary_breaches(path: &Path, source: &str, lower: &str, words: &[String]) -> Vec<String> {
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
    let crates = kit_root().join("crates");
    let failures: Vec<_> = manifests()
        .into_iter()
        .filter(|manifest| {
            manifest.path.starts_with(&crates)
                && manifest.name.as_deref().is_some_and(|name| name == "jig-core" || name.starts_with("jig-core-"))
        })
        .flat_map(|manifest| {
            manifest
                .dependencies
                .iter()
                .filter(|name| {
                    if manifest.name.as_deref() == Some("jig-core") {
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
fn hosts_and_translation_use_no_core_crates_even_when_empty() {
    let failures: Vec<_> = manifests()
        .into_iter()
        .filter(|manifest| matches!(manifest.name.as_deref(), Some("jig-host" | "jig-inline-agent" | "jig-charter")))
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
fn host_hub_links_only_skein() {
    let failures: Vec<_> = manifests()
        .into_iter()
        .filter(|manifest| manifest.name.as_deref() == Some("jig-host"))
        .flat_map(|manifest| {
            manifest
                .dependencies
                .iter()
                .filter(|name| name.as_str() != "skein-lib")
                .map(|name| format!("{}: {name}", manifest.path.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn kit_sources_keep_the_kit_vocabulary() {
    let mut files = Vec::new();
    source_files(&kit_root(), true, &[".rs", "Cargo.toml"], &mut files);
    let words = forbidden_words();
    let failures: Vec<_> = files
        .iter()
        .flat_map(|path| {
            let source = fs::read_to_string(path).expect("source is readable");
            let source = if path.file_name().is_some_and(|name| name == "Cargo.toml") && workspace_manifest(&source) {
                workspace_vocabulary(&source)
            } else {
                source
            };
            let lower = source.to_ascii_lowercase();
            let mut failures = vocabulary_breaches(path, &source, &lower, &words);
            if path.extension().is_some_and(|extension| extension == "rs") {
                failures.extend(super::names::breaches(path, &source, &lower));
            }
            failures
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn workspace_fixture_checks_dependencies_without_a_package() {
    let forbidden = ["tem", "per", "-sample"].concat();
    let manifest =
        format!("[workspace]\nmembers = []\n[workspace.dependencies]\n{forbidden} = {{ path = \"../example\" }}\n");
    let parsed = parse_manifest(PathBuf::from("fixture/Cargo.toml"), &manifest);
    assert_eq!(parsed.name, None);
    assert_eq!(breaches(&[parsed], all_crates), [format!("fixture/Cargo.toml: {forbidden}")]);
    assert!(!workspace_manifest("[workspace]\n[package]\nname = \"jig-sample\"\n"));
}

#[test]
fn workspace_fixture_omits_only_dependency_sources() {
    let word = ["g", "it"].concat();
    let url = format!("https://{word}.ekanayaka.io/ai/skein.{word}");
    let manifest = format!("[workspace]\n[workspace.dependencies]\nskein-lib = {{ {word} = \"{url}\" }}\n");
    let source = workspace_vocabulary(&manifest);
    assert_eq!(source, "[workspace]\n[workspace.dependencies]\nskein-lib = {  }\n");
    assert!(
        vocabulary_breaches(Path::new("fixture/Cargo.toml"), &source, &source.to_ascii_lowercase(), &forbidden_words())
            .is_empty()
    );

    let commented = format!("{manifest}# {word} = \"{url}\"\n");
    let source = workspace_vocabulary(&commented);
    assert!(source.ends_with(&format!("# {word} = \"{url}\"\n")));
    assert_eq!(
        vocabulary_breaches(Path::new("fixture/Cargo.toml"), &source, &source.to_ascii_lowercase(), &forbidden_words())
            .len(),
        3
    );

    let other = format!("[workspace]\nlabel = '{word}'\n{word} = true\n");
    assert_eq!(workspace_vocabulary(&other), other);
    let quoted = format!("[workspace]\n[workspace.dependencies.skein-lib]\n\"{word}\" = '{url}'\nversion = \"1\"\n");
    assert_eq!(workspace_vocabulary(&quoted), "[workspace]\n[workspace.dependencies.skein-lib]\n\nversion = \"1\"\n");
}

#[test]
fn walk_fixture_skips_build_and_hidden_directories() {
    assert!(!walk_directory("target", true));
    assert!(!walk_directory(&[".", "g", "it"].concat(), true));
    assert!(!walk_directory(".config", false));
    assert!(walk_directory("tests", true));
    assert!(walk_directory("target", false));
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
    let failures =
        vocabulary_breaches(Path::new("fixture/src/lib.rs"), &source, &source.to_ascii_lowercase(), &forbidden_words());
    assert_eq!(failures, [format!("fixture/src/lib.rs: {word}")]);
}

#[test]
fn name_fixture_reports_each_file_line_and_breach() {
    let source = include_str!("../fixtures/names.txt");
    let failures = super::names::breaches(Path::new("fixture/names.rs"), source, &source.to_ascii_lowercase());
    assert_eq!(
        failures,
        [
            "fixture/names.rs:1: identifier AnswerV2",
            "fixture/names.rs:2: identifier finish_v19",
            "fixture/names.rs:3: identifier TypedCall",
            "fixture/names.rs:4: identifier UntypedCall",
            "fixture/names.rs:5: identifier LegacyCall",
            "fixture/names.rs:6: identifier assign_typed",
            "fixture/names.rs:7: identifier legacy_call",
            "fixture/names.rs:8: comment LEGACY",
            "fixture/names.rs:10: comment legacy",
        ]
    );
}
