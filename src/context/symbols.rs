use std::path::Path;

/// One definition: a named item and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub path: String,
    pub line: usize,
}

/// Deterministic definition index over a workdir. No embeddings, no model
/// calls: line-regex for Rust (`struct/enum/trait/fn/impl/type/const/static`)
/// and Python (`def/class`). Capped at `max_files` files; skips dot-dirs,
/// target, .git, node_modules.
pub fn index_workdir(root: &Path, max_files: usize) -> Vec<Symbol> {
    let mut files = Vec::new();
    walk_rs_py(root, 0, &mut files, max_files);
    let mut out = Vec::new();
    for f in files {
        let rel = rel(root, &f);
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let is_py = f.extension().and_then(|e| e.to_str()) == Some("py");
        for (i, line) in text.lines().enumerate() {
            if let Some(name) = def_name(line, is_py) {
                out.push(Symbol {
                    name,
                    path: rel.clone(),
                    line: i + 1,
                });
            }
        }
    }
    out
}

/// Files defining a goal-named symbol + 1-hop files that mention it in a
/// use/import line. Returns deduped relative paths, capped at 5.
pub fn expand(root: &Path, query: &str, symbols: &[Symbol]) -> Vec<String> {
    let idents = query_idents(query);
    let mut out: Vec<String> = Vec::new();
    for id in &idents {
        for s in symbols.iter().filter(|s| &s.name == id) {
            if !out.iter().any(|p| p == &s.path) {
                out.push(s.path.clone());
            }
            if out.len() >= 5 {
                return out;
            }
        }
    }
    // 1-hop: files with a use/import line mentioning the symbol.
    if out.is_empty() {
        return out;
    }
    let mut hop: Vec<String> = Vec::new();
    let mut files = Vec::new();
    walk_rs_py(root, 0, &mut files, 500);
    for f in files {
        let rel = rel(root, &f);
        if out.iter().any(|p| p == &rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let hit = text.lines().take(60).any(|l| {
            let t = l.trim();
            (t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from "))
                && idents.iter().any(|id| t.contains(id.as_str()))
        });
        if hit {
            hop.push(rel);
            if out.len() + hop.len() >= 5 {
                break;
            }
        }
    }
    out.extend(hop);
    out
}

fn query_idents(query: &str) -> Vec<String> {
    let mut v = Vec::new();
    for w in query.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if w.len() < 4 || v.iter().any(|e: &String| e == w) {
            continue;
        }
        let camel = w.chars().skip(1).any(|c| c.is_ascii_uppercase());
        if w.contains('_') || camel {
            v.push(w.to_string());
        }
    }
    v.sort_by_key(|w| std::cmp::Reverse(w.len()));
    v.truncate(8);
    v
}

fn def_name(line: &str, is_py: bool) -> Option<String> {
    let t = line.trim();
    if is_py {
        for kw in ["def ", "class "] {
            if let Some(rest) = t.strip_prefix(kw) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name.len() >= 3 {
                    return Some(name);
                }
            }
        }
        return None;
    }
    const KW: [&str; 8] = [
        "struct", "enum", "trait", "fn", "impl", "type", "const", "static",
    ];
    // Strip visibility qualifiers: `pub(...) struct Foo`.
    let mut rest = t;
    if let Some(r) = rest.strip_prefix("pub") {
        rest = r.trim_start_matches(['(', ')', ' ']).trim_start();
    }
    for kw in KW {
        if let Some(r) = rest.strip_prefix(kw) {
            // Keyword must be followed by a boundary (space, `<`, `(`).
            // `structFoo` must not match; `fn helper` must.
            if !(r.starts_with(' ') || r.starts_with('<') || r.starts_with('(')) {
                continue;
            }
            let r = r.trim_start();
            // impl Trait for Type / impl Type: take the last ident.
            let cands: Vec<String> = r
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|w| w.len() >= 3)
                .map(str::to_string)
                .collect();
            if kw == "impl" {
                if let Some(last) = cands.last() {
                    return Some(last.clone());
                }
            } else if let Some(first) = cands.first() {
                return Some(first.clone());
            }
        }
    }
    None
}

fn walk_rs_py(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>, cap: usize) {
    if depth > 4 || out.len() >= cap {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if matches!(
                name.as_str(),
                "target" | ".git" | "node_modules" | "__pycache__"
            ) {
                continue;
            }
            walk_rs_py(&p, depth + 1, out, cap);
        } else if matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("rs") | Some("py")
        ) {
            out.push(p);
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}
