//! Trace profiling and starter format generation for the CLI.
//!
//! These helpers intentionally inspect only the top-level JSON object. That is
//! enough to infer the discriminator and common trace keys while leaving the
//! actual format file in the user's control.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Profile {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub records: usize,
    pub objects: usize,
    pub invalid_lines: usize,
    pub fields: BTreeMap<String, FieldProfile>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FieldProfile {
    pub present: usize,
    pub kinds: BTreeMap<String, usize>,
    pub examples: Vec<String>,
    pub values: BTreeMap<String, usize>,
}

impl Profile {
    pub fn scan(paths: &[std::path::PathBuf]) -> Self {
        let mut profile = Self::default();
        for path in paths {
            match scan_file(path, &mut profile) {
                Ok(()) => profile.files_scanned += 1,
                Err(_) => profile.files_skipped += 1,
            }
        }
        profile
    }

    pub fn infer_field(&self, preferred: &[&str], fallback: &str) -> String {
        if let Some(name) = preferred.iter().find(|name| {
            self.fields
                .get(**name)
                .map(|field| field.kinds.contains_key("string"))
                .unwrap_or(false)
        }) {
            return (*name).to_string();
        }

        // For unfamiliar schemas, prefer a common string field with multiple
        // observed values over the fallback `t` discriminator.
        self.fields
            .iter()
            .filter(|(_, field)| {
                field.kinds.contains_key("string")
                    && field.present * 2 >= self.objects
                    && field.values.len() > 1
            })
            .max_by_key(|(_, field)| (field.values.len(), field.present))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    pub fn type_field(&self) -> String {
        self.infer_field(&["t", "type", "event", "kind", "event_type"], "t")
    }

    pub fn timestamp_field(&self) -> String {
        ["ts", "timestamp", "time", "created_at", "timestamp_ms"]
            .iter()
            .find(|name| {
                self.fields.get(**name).is_some_and(|field| {
                    field.kinds.contains_key("string") || field.kinds.contains_key("number")
                })
            })
            .map(|name| (*name).to_string())
            .unwrap_or_else(|| "ts".into())
    }

    pub fn optional_field(&self, preferred: &[&str]) -> Option<String> {
        preferred
            .iter()
            .find(|name| self.fields.contains_key(**name))
            .map(|name| (*name).to_string())
    }

    pub fn event_types(&self, field: &str) -> Vec<(String, usize)> {
        let Some(summary) = self.fields.get(field) else {
            return Vec::new();
        };
        let mut values: Vec<_> = summary
            .values
            .iter()
            .map(|(value, count)| (value.clone(), *count))
            .collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        values
    }
}

fn scan_file(path: &Path, profile: &mut Profile) -> io::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        profile.records += 1;
        let value = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(_) => {
                profile.invalid_lines += 1;
                continue;
            }
        };
        let Value::Object(object) = value else {
            profile.invalid_lines += 1;
            continue;
        };
        profile.objects += 1;
        for (name, value) in object {
            let summary = profile.fields.entry(name).or_default();
            summary.present += 1;
            let kind = json_kind(&value).to_string();
            *summary.kinds.entry(kind).or_default() += 1;
            if summary.examples.len() < 3 {
                let example = compact_json(&value);
                if !summary.examples.contains(&example) {
                    summary.examples.push(example);
                }
            }
            if let Some(value) = value.as_str() {
                // Keep profile output bounded for high-cardinality fields while
                // retaining enough values to identify event discriminators.
                if summary.values.contains_key(value) || summary.values.len() < 128 {
                    *summary.values.entry(value.to_string()).or_default() += 1;
                }
            }
        }
    }
    Ok(())
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn compact_json(value: &Value) -> String {
    let mut text = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".into());
    if text.len() > 120 {
        text.truncate(117);
        text.push_str("...");
    }
    text
}

/// Produce a valid, intentionally conservative format file. The generic
/// fallback renders all unlisted fields, so this remains useful even when the
/// inferred schema is incomplete.
pub fn starter_format(profile: &Profile) -> String {
    let type_field = profile.type_field();
    let timestamp_field = profile.timestamp_field();
    let sequence_field = profile.optional_field(&["sequence", "seq", "index", "event_index"]);
    let run_field = profile.optional_field(&["run_id", "run", "trace_id", "session_id"]);
    let duration_field = profile.optional_field(&["duration_ms", "latency_ms", "elapsed_ms"]);
    let types = profile.event_types(&type_field);

    let mut out = String::new();
    out.push_str("# Generated by `traciium init`. Edit this file to describe your trace.\n");
    out.push_str("# `traciium inspect <path> --json` gives agents a machine-readable profile.\n");
    out.push_str("# Unlisted fields are still shown automatically by the generic fallback.\n\n");
    out.push_str("trace:\n");
    out.push_str(&format!("  type_field: {}\n", yaml_scalar(&type_field)));
    out.push_str(&format!(
        "  timestamp_field: {}\n",
        yaml_scalar(&timestamp_field)
    ));
    if let Some(field) = sequence_field.as_deref() {
        out.push_str(&format!("  sequence_field: {}\n", yaml_scalar(field)));
    } else {
        out.push_str("  # sequence_field: sequence\n");
    }
    if let Some(field) = run_field.as_deref() {
        out.push_str(&format!("  run_field: {}\n", yaml_scalar(field)));
    } else {
        out.push_str("  # run_field: run_id\n");
    }
    if let Some(field) = duration_field.as_deref() {
        out.push_str(&format!("  duration_field: {}\n", yaml_scalar(field)));
    }

    out.push_str("\n# Detected event types (counts from the profile):\n");
    if types.is_empty() {
        out.push_str("#   (none — check type_field above)\n");
    } else {
        for (kind, count) in types.iter().take(40) {
            out.push_str(&format!("#   {}: {}\n", yaml_scalar(kind), count));
        }
    }
    if types.len() > 40 {
        out.push_str(&format!("#   ... and {} more\n", types.len() - 40));
    }

    out.push_str(
        r#"
# Add presentation rules as needed. Supported body renderers:
# markdown, code, output, text, kv, tokens, messages, tool_args,
# tool_calls, tool_calls_brief, list, json.
#
# events:
#   tool_call:
#     label: Tool call
#     icon: "▶"
#     tone: sky
#     title_from: name
#     body:
#       arguments: tool_args
#   assistant:
#     label: Assistant
#     tone: emerald
#     body:
#       content: markdown

events: {}

defaults:
  ignore: []
  open: false
  max_text: 20000
  header_fields: []

# Optional table columns. Empty means infer columns from the data.
table:
  columns: []
  max_cell: 140
"#,
    );
    out
}

fn yaml_scalar(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        value.to_string()
    } else {
        serde_yaml::to_string(value)
            .unwrap_or_else(|_| format!("'{}'", value.replace('\'', "''")))
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_fields_and_event_values() {
        let dir = std::env::temp_dir().join(format!("traciium-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"start","timestamp":1000,"run":"a","message":"go"}"#,
                "\n",
                r#"{"type":"finish","timestamp":1001,"run":"a","ok":true}"#,
                "\n",
                "not json\n",
            ),
        )
        .unwrap();

        let profile = Profile::scan(std::slice::from_ref(&path));
        assert_eq!(profile.files_scanned, 1);
        assert_eq!(profile.objects, 2);
        assert_eq!(profile.invalid_lines, 1);
        assert_eq!(profile.type_field(), "type");
        assert_eq!(profile.timestamp_field(), "timestamp");
        assert_eq!(profile.event_types("type")[0], ("finish".into(), 1));
        assert!(profile.fields["ok"].kinds.contains_key("boolean"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn starter_format_is_valid_yaml() {
        let profile = Profile {
            objects: 1,
            fields: BTreeMap::from([
                (
                    "t".into(),
                    FieldProfile {
                        present: 1,
                        kinds: BTreeMap::from([(String::from("string"), 1)]),
                        values: BTreeMap::from([(String::from("event"), 1)]),
                        ..Default::default()
                    },
                ),
                (
                    "ts".into(),
                    FieldProfile {
                        present: 1,
                        kinds: BTreeMap::from([(String::from("number"), 1)]),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        };
        let yaml = starter_format(&profile);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed["trace"]["type_field"], "t");
        assert_eq!(parsed["trace"]["timestamp_field"], "ts");
    }
}
