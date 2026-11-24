use std::path::Path;

pub(crate) const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_INJECTED_CHARS: usize = 4_000;

#[derive(Default)]
pub(crate) struct Selection {
    pub ids: Vec<String>,
    pub instructions: String,
}

impl Selection {
    pub(crate) fn append_to(&self, goal: &mut String) {
        if !self.instructions.is_empty() {
            goal.push_str("\n\nExternally sourced procedure guidance (applicability matched against the original task; no learning claim):\n");
            goal.push_str(&self.instructions);
        }
    }
}

struct Procedure {
    id: String,
    cue: String,
    instruction: String,
    source_repositories: Vec<String>,
}

pub(crate) struct Catalog {
    procedures: Vec<Procedure>,
    pub file_sha256: Vec<String>,
}

pub(crate) fn sha256_hex(digest: ring::digest::Digest) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    sha256_hex(ring::digest::digest(&ring::digest::SHA256, bytes))
}

pub(crate) fn load(paths: &[impl AsRef<Path>]) -> Result<Catalog, String> {
    let mut catalog = Catalog {
        procedures: Vec::new(),
        file_sha256: Vec::new(),
    };
    for path in paths {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| format!("cannot read procedure file {}: {e}", path.display()))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(format!(
                "procedure file {} exceeds {MAX_FILE_BYTES} bytes",
                path.display()
            ));
        }
        catalog.file_sha256.push(sha256(&bytes));
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid procedure JSON {}: {e}", path.display()))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("procedure {} must be a JSON object", path.display()))?;
        if object.len() != 4
            || ["id", "cue", "instruction", "source_repositories"]
                .iter()
                .any(|k| !object.contains_key(*k))
        {
            return Err(format!(
                "procedure {} needs id, cue, instruction, source_repositories only",
                path.display()
            ));
        }
        let field = |key: &str| -> Result<&str, String> {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("procedure {} has empty or invalid {key}", path.display()))
        };
        let id = field("id")?;
        let cue = field("cue")?.to_lowercase();
        let instruction = field("instruction")?;
        let repos = object
            .get("source_repositories")
            .and_then(serde_json::Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| format!("procedure {} needs source_repositories", path.display()))?;
        let repos = repos
            .iter()
            .map(|r| {
                r.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "procedure {} has an empty source repository",
                            path.display()
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        catalog.procedures.push(Procedure {
            id: id.into(),
            cue,
            instruction: instruction.into(),
            source_repositories: repos.into_iter().map(str::to_owned).collect(),
        });
    }
    Ok(catalog)
}

impl Catalog {
    pub(crate) fn select(&self, original_task: &str) -> Result<Selection, String> {
        let task = original_task.to_lowercase();
        let mut selection = Selection::default();
        let mut injected_chars = 0;
        for procedure in &self.procedures {
            if !task.contains(&procedure.cue) {
                continue;
            }
            let chunk = format!(
                "Procedure {} (external sources: {}):\n{}",
                procedure.id,
                procedure.source_repositories.join(", "),
                procedure.instruction
            );
            let next = injected_chars
                + chunk.chars().count()
                + usize::from(!selection.instructions.is_empty()) * 2;
            if next > MAX_INJECTED_CHARS {
                return Err(format!(
                    "selected procedures exceed the {MAX_INJECTED_CHARS}-character injection cap"
                ));
            }
            if !selection.instructions.is_empty() {
                selection.instructions.push_str("\n\n");
            }
            selection.instructions.push_str(&chunk);
            injected_chars = next;
            selection.ids.push(procedure.id.clone());
        }
        Ok(selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_applicable_records_and_hashes_the_loaded_bytes() {
        let path = std::env::temp_dir().join(format!("rof-procedure-{}.json", std::process::id()));
        let bytes = br#"{"id":"rust-tests","cue":"cargo test","instruction":"Run focused tests first.","source_repositories":["example/rust-guide"]}"#;
        std::fs::write(&path, bytes).unwrap();
        let catalog = load(std::slice::from_ref(&path)).unwrap();
        let selected = catalog
            .select("Please run CARGO TEST before finalizing.")
            .unwrap();
        assert_eq!(selected.ids, vec!["rust-tests"]);
        assert_eq!(selected.instructions, "Procedure rust-tests (external sources: example/rust-guide):\nRun focused tests first.");
        assert_eq!(catalog.file_sha256, vec![sha256(bytes)]);
        assert!(catalog.select("unrelated").unwrap().ids.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_incomplete_or_oversized_procedure_records() {
        let path =
            std::env::temp_dir().join(format!("rof-procedure-invalid-{}.json", std::process::id()));
        std::fs::write(
            &path,
            br#"{"id":"x","cue":"y","instruction":"z","source_repositories":[]}"#,
        )
        .unwrap();
        assert!(load(std::slice::from_ref(&path)).is_err());
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        assert!(load(std::slice::from_ref(&path)).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
