use std::fs;
use std::path::Path;

const CSS_FILES: &[&str] = &["src/style.css", "src/style-light.css"];

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

#[test]
fn css_rules_do_not_use_forbidden_fixed_pixel_units() {
    let mut violations = Vec::new();

    for file_path in CSS_FILES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file_path);
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));

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
