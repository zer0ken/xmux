use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const TOP_LEVEL_MODULES: &[&str] = &[
    "app",
    "cli",
    "display",
    "driver",
    "link",
    "logging",
    "model",
    "mux",
    "provision",
    "session",
    "state",
    "transport",
    "ui",
];

const SOURCE_ROOTS: &[&str] = &[
    "app",
    "cli",
    "display",
    "driver",
    "lib",
    "link",
    "logging",
    "main",
    "model",
    "mux",
    "provision",
    "session",
    "state",
    "transport",
    "ui",
];

const BACKEND_MODULES: &[&str] = &[
    "display",
    "driver",
    "link",
    "logging",
    "model",
    "mux",
    "provision",
    "session",
    "transport",
];

const BACKEND_ALLOWED_TARGETS: &[&str] = &[
    "display",
    "driver",
    "link",
    "logging",
    "model",
    "mux",
    "provision",
    "session",
    "transport",
];

const STATE_ALLOWED_TARGETS: &[&str] = &[
    "display",
    "driver",
    "link",
    "logging",
    "model",
    "mux",
    "provision",
    "session",
    "state",
    "transport",
];

const KNOWN_VIOLATIONS: &[(&str, &str)] = &[
    ("src/provision/config.rs", "ui"),
    ("src/state/mod.rs", "app"),
    ("src/state/mod.rs", "ui"),
];

#[test]
fn layer_direction_matches_the_allowed_edges() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    files.sort();

    let known: BTreeSet<_> = KNOWN_VIOLATIONS
        .iter()
        .map(|&(file, target)| (file.to_owned(), target.to_owned()))
        .collect();
    assert_eq!(
        known.len(),
        KNOWN_VIOLATIONS.len(),
        "KNOWN_VIOLATIONS contains duplicates"
    );

    let mut actual = BTreeSet::new();
    for file in files {
        let source = top_level_module(&src, &file);
        assert!(
            SOURCE_ROOTS.contains(&source),
            "{} belongs to unknown source root `{source}`",
            relative_path(root, &file)
        );
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", file.display()));
        for target in crate_targets(&text) {
            assert!(
                TOP_LEVEL_MODULES.contains(&target.as_str()),
                "{} refers to unknown top-level module `{target}`",
                relative_path(root, &file)
            );
            if !edge_is_allowed(source, &target) {
                actual.insert((relative_path(root, &file), target));
            }
        }
    }

    let new: Vec<_> = actual.difference(&known).collect();
    let stale: Vec<_> = known.difference(&actual).collect();
    assert!(
        new.is_empty() && stale.is_empty(),
        "Layer Direction: every import must follow the allowed-edge table.\nNew violations:\n{}\nKnown violations that no longer exist:\n{}",
        format_violations(&new),
        format_violations(&stale)
    );
}

#[test]
fn scanner_finds_direct_and_grouped_crate_paths() {
    let source = r####"
        use crate::display::Grid;
        use crate::{link, model::{Action, Command}, transport as host_transport};
        let _ = crate::session::Session::default();
        let quote = '"';
        let _ = crate::app::run();
        // crate::app::ignored_comment();
        /* crate::state::ignored_block(/* nested */); */
        let _ = "crate::ui::ignored_string";
        let _ = b"crate::state::ignored_byte_string";
        let _ = r#"crate::ui::ignored_raw_string"#;
        let _ = br##"crate::state::ignored_raw_byte_string"##;
        let escaped_quote = '\'';
        let unicode_quote = '\u{22}';
        let lifetime: &'static str = "";
        let _ = crate::logging::init;
    "####;

    assert_eq!(
        crate_targets(source),
        BTreeSet::from([
            "app".to_owned(),
            "display".to_owned(),
            "link".to_owned(),
            "logging".to_owned(),
            "model".to_owned(),
            "session".to_owned(),
            "transport".to_owned(),
        ])
    );
}

fn edge_is_allowed(source: &str, target: &str) -> bool {
    if BACKEND_MODULES.contains(&source) {
        BACKEND_ALLOWED_TARGETS.contains(&target)
    } else if source == "state" {
        STATE_ALLOWED_TARGETS.contains(&target)
    } else {
        TOP_LEVEL_MODULES.contains(&target)
    }
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
    {
        let path = entry.expect("failed to read directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

fn top_level_module<'a>(src: &Path, file: &'a Path) -> &'a str {
    let relative = file
        .strip_prefix(src)
        .unwrap_or_else(|_| panic!("{} is outside {}", file.display(), src.display()));
    let first = relative
        .components()
        .next()
        .expect("source file has no path component")
        .as_os_str()
        .to_str()
        .expect("source path is not UTF-8");
    first.strip_suffix(".rs").unwrap_or(first)
}

fn relative_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn format_violations(violations: &[&(String, String)]) -> String {
    if violations.is_empty() {
        return "  (none)".to_owned();
    }
    violations
        .iter()
        .map(|(file, target)| {
            let rule = if file.starts_with("src/state/") {
                "State Direction"
            } else {
                "Backend Direction"
            };
            format!("  {file} -> {target}: {rule}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn crate_targets(source: &str) -> BTreeSet<String> {
    let tokens = tokenize(&strip_comments_and_strings(source));
    let mut targets = BTreeSet::new();
    let mut index = 0;
    while index + 2 < tokens.len() {
        if tokens[index] == "crate" && tokens[index + 1] == "::" {
            if is_identifier(&tokens[index + 2]) {
                targets.insert(tokens[index + 2].clone());
            } else if tokens[index + 2] == "{" {
                collect_grouped_targets(&tokens, index + 3, &mut targets);
            }
        }
        index += 1;
    }
    targets
}

fn collect_grouped_targets(tokens: &[String], mut index: usize, targets: &mut BTreeSet<String>) {
    let mut depth = 1;
    let mut expect_target = true;
    while index < tokens.len() && depth > 0 {
        match tokens[index].as_str() {
            "{" => depth += 1,
            "}" => depth -= 1,
            "," if depth == 1 => expect_target = true,
            token if depth == 1 && expect_target && is_identifier(token) => {
                if token != "self" {
                    targets.insert(token.to_owned());
                }
                expect_target = false;
            }
            _ => {}
        }
        index += 1;
    }
}

fn tokenize(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if is_identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            tokens.push(source[start..index].to_owned());
        } else if index + 1 < bytes.len() && bytes[index] == b':' && bytes[index + 1] == b':' {
            tokens.push("::".to_owned());
            index += 2;
        } else {
            if matches!(bytes[index], b'{' | b'}' | b',' | b';') {
                tokens.push((bytes[index] as char).to_string());
            }
            index += 1;
        }
    }
    tokens
}

fn is_identifier(token: &str) -> bool {
    token
        .as_bytes()
        .first()
        .is_some_and(|byte| is_identifier_start(*byte))
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn strip_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut stripped = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            let start = index;
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            stripped[start..index].fill(b' ');
        } else if bytes[index..].starts_with(b"/*") {
            let start = index;
            index += 2;
            let mut depth = 1;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            stripped[start..index].fill(b' ');
        } else if let Some(end) = char_literal_end(source, index) {
            stripped[index..end].fill(b' ');
            index = end;
        } else if let Some((content_start, terminator)) = raw_string_start(bytes, index) {
            let start = index;
            index = content_start;
            while index + terminator.len() <= bytes.len()
                && &bytes[index..index + terminator.len()] != terminator.as_slice()
            {
                index += 1;
            }
            index = (index + terminator.len()).min(bytes.len());
            stripped[start..index].fill(b' ');
        } else if bytes[index] == b'"'
            || (bytes[index] == b'b' && bytes.get(index + 1) == Some(&b'"'))
        {
            let start = index;
            if bytes[index] == b'b' {
                index += 1;
            }
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            stripped[start..index].fill(b' ');
        } else {
            index += 1;
        }
    }
    String::from_utf8(stripped).expect("stripping preserved UTF-8")
}

fn char_literal_end(source: &str, index: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'\'') {
        return None;
    }
    cursor += 1;
    if bytes.get(cursor) == Some(&b'\\') {
        cursor += 1;
        match bytes.get(cursor)? {
            b'u' if bytes.get(cursor + 1) == Some(&b'{') => {
                cursor += 2;
                while bytes.get(cursor) != Some(&b'}') {
                    cursor += 1;
                    bytes.get(cursor)?;
                }
                cursor += 1;
            }
            b'x' => cursor += 3,
            _ => cursor += 1,
        }
    } else {
        cursor += source.get(cursor..)?.chars().next()?.len_utf8();
    }
    (bytes.get(cursor) == Some(&b'\'')).then_some(cursor + 1)
}

fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, Vec<u8>)> {
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hashes = cursor - hashes_start;
    cursor += 1;
    let mut terminator = vec![b'"'];
    terminator.extend(std::iter::repeat_n(b'#', hashes));
    Some((cursor, terminator))
}
