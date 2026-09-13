use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const CSS_FILES: &[&str] = &[
    "src/style.css",
    "src/tokens-dark.css",
    "src/tokens-light.css",
];
const RULE_FILE: &str = "src/style.css";
const THEME_FILES: &[&str] = &["src/tokens-dark.css", "src/tokens-light.css"];

fn read(file_path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file_path);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn is_whitelisted_property(prop: &str) -> bool {
    let p = prop.trim();
    p == "border"
        || p == "border-top"
        || p == "border-bottom"
        || p == "border-left"
        || p == "border-right"
        || p == "border-width"
        || p == "border-top-width"
        || p == "border-bottom-width"
        || p == "border-left-width"
        || p == "border-right-width"
        || p == "outline"
        || p == "outline-width"
        || p == "box-shadow"
}

fn token_definitions(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("@define-color ") {
            if let Some(name) = rest.split_whitespace().next() {
                names.insert(name.to_string());
            }
        }
    }
    names
}

fn token_references(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'-')
            {
                end += 1;
            }
            let name = &content[start..end];
            if name.starts_with("oc_") {
                names.insert(name.to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    names
}

fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("/*") || trimmed.starts_with("*") || trimmed.starts_with("//")
}

fn has_raw_color(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit) {
            return true;
        }
    }
    let lower = line.to_ascii_lowercase();
    lower.contains("rgb(") || lower.contains("rgba(") || lower.contains("hsl(")
}

#[test]
fn css_rules_do_not_use_forbidden_fixed_pixel_units() {
    let mut violations = Vec::new();

    for file_path in CSS_FILES {
        let content = read(file_path);

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();

            if trimmed.contains("/* px-opt-out") || trimmed.contains("// px-opt-out") {
                continue;
            }

            if let Some(px_idx) = trimmed.find("px") {
                if px_idx > 0 && trimmed.as_bytes()[px_idx - 1].is_ascii_digit() {
                    if let Some(colon_idx) = trimmed.find(':') {
                        let prop = &trimmed[..colon_idx];
                        if is_whitelisted_property(prop) {
                            continue;
                        }
                    }
                    violations.push(format!("{}:{}: {}", file_path, line_no + 1, trimmed));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "\n=== CSS LINT FAILURE: Forbidden 'px' unit found ===\n\
            All dimensions (font-size, -gtk-icon-size, padding, margin, min-height, min-width, border-radius)\n\
            MUST use relative units ('em', '%', 'rem') to support UI zoom scaling.\n\
            Allowed exceptions: hairline borders ('border: 1px solid ...'), 'box-shadow', or explicit '/* px-opt-out: <reason> */'.\n\n\
            Violations ({} lines):\n{}\n",
            violations.len(),
            violations.join("\n")
        );
    }
}

#[test]
fn rule_stylesheet_uses_tokens_not_raw_colors() {
    let content = read(RULE_FILE);
    let mut violations = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        if is_comment_line(line) {
            continue;
        }
        if has_raw_color(line) {
            violations.push(format!("{}:{}: {}", RULE_FILE, line_no + 1, line.trim()));
        }
    }

    if !violations.is_empty() {
        panic!(
            "\n=== CSS LINT FAILURE: Raw color literal in {RULE_FILE} ===\n\
            Theme colors MUST be referenced through an @oc_* token so both themes are covered.\n\
            Add the value to src/tokens-dark.css and src/tokens-light.css, then reference @oc_*.\n\n\
            Violations ({} lines):\n{}\n",
            violations.len(),
            violations.join("\n")
        );
    }
}

#[test]
fn theme_token_files_define_only_tokens() {
    let mut violations = Vec::new();
    for file_path in THEME_FILES {
        for (line_no, line) in read(file_path).lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || is_comment_line(trimmed) {
                continue;
            }
            if !trimmed.starts_with("@define-color ") {
                violations.push(format!("{}:{}: {}", file_path, line_no + 1, trimmed));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "\n=== CSS LINT FAILURE: theme files may only contain @define-color lines ===\n{}\n",
        violations.join("\n")
    );
}

#[test]
fn token_names_match_across_themes() {
    let mut sets = THEME_FILES.iter().map(|f| (f, token_definitions(&read(f))));
    let (first_file, first) = sets.next().unwrap();
    for (file, set) in sets {
        let missing: Vec<_> = first.difference(&set).cloned().collect();
        let extra: Vec<_> = set.difference(&first).cloned().collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "\n=== CSS LINT FAILURE: token sets differ between {first_file} and {file} ===\n\
            only in {first_file}: {missing:?}\n\
            only in {file}: {extra:?}\n"
        );
    }
}

#[test]
fn referenced_tokens_are_defined_in_both_themes() {
    let rules = read(RULE_FILE);
    let references = token_references(&rules);
    assert!(
        !references.is_empty(),
        "style.css references no @oc_* tokens; tokenization regressed"
    );
    for file_path in THEME_FILES {
        let defined = token_definitions(&read(file_path));
        let undefined: Vec<_> = references.difference(&defined).cloned().collect();
        assert!(
            undefined.is_empty(),
            "\n=== CSS LINT FAILURE: tokens referenced but not defined in {file_path} ===\n{undefined:?}\n"
        );
    }
}
