//! format.yaml — declarative styling/presentation rules for traces.
//!
//! The spec lets a user point traciium at *any* JSONL event schema and
//! describe: which key discriminates event types, how to render each event
//! type, which fields to ignore, how to colour values, and how to lay out
//! the table view. Everything has sensible defaults so a minimal file like
//! `trace: {type_field: t}` already produces a usable viewer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FormatSpec {
    /// How to locate the core keys inside each JSONL record.
    #[serde(default)]
    pub trace: TraceKeys,
    /// Per-event-type presentation rules, keyed by the value of `type_field`.
    #[serde(default)]
    pub events: BTreeMap<String, EventSpec>,
    /// Field rendering / hiding defaults applied to every event.
    #[serde(default)]
    pub defaults: Defaults,
    /// Flat table view configuration.
    #[serde(default)]
    pub table: TableSpec,
    /// Value-level styling, e.g. `success: {true: {tone: emerald}}`.
    #[serde(default)]
    pub value_styles: BTreeMap<String, BTreeMap<String, ValueStyle>>,
    /// Whole-card conditional styles, evaluated in order, first match wins.
    #[serde(default)]
    pub conditional_styles: Vec<ConditionalStyle>,
    /// Event type -> millisecond field, used for timeline time accounting.
    #[serde(default)]
    pub timeline: BTreeMap<String, String>,
    /// Run-level outcome inference, used for the runs list chips.
    #[serde(default)]
    pub outcome: OutcomeSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TraceKeys {
    /// Record key holding the event type discriminator.
    pub type_field: String,
    /// Record key holding the timestamp (unix seconds or ISO-8601 string).
    pub timestamp_field: String,
    /// Optional key used as a tie-break ordering field.
    pub sequence_field: Option<String>,
    /// Optional key used to group a file's records into runs.
    pub run_field: Option<String>,
    /// Optional key holding an explicit duration in milliseconds.
    pub duration_field: Option<String>,
}

impl Default for TraceKeys {
    fn default() -> Self {
        TraceKeys {
            type_field: "t".into(),
            timestamp_field: "ts".into(),
            sequence_field: None,
            run_field: Some("run_id".into()),
            duration_field: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct EventSpec {
    /// Display label in the stream header. Defaults to the raw type value.
    pub label: Option<String>,
    /// Single glyph or emoji shown before the label.
    pub icon: Option<String>,
    /// Palette tone name (e.g. `sky`) or a raw `#rrggbb` colour.
    pub tone: Option<String>,
    /// Whether the card body starts expanded.
    pub open: Option<bool>,
    /// Field whose value becomes the card title.
    pub title_from: Option<String>,
    /// Extra fields rendered as small chips in the card header.
    pub badges: Vec<String>,
    /// Field -> renderer mapping for the card body, in display order.
    pub body: BTreeMap<String, String>,
    /// Milliseconds field used to draw this event as a timeline span.
    pub duration_field: Option<String>,
    /// Treat this event as the run's terminal marker (success/failure card).
    pub terminal: Option<bool>,
    /// Render this event as a compact separator line instead of a card.
    pub compact: Option<bool>,
    /// Render as a slim one-line band (header only, tone-tinted).
    pub slim: Option<bool>,
    /// Left border emphasis: `normal | bold`.
    pub weight: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    /// Field names never rendered anywhere in the stream view.
    pub ignore: Vec<String>,
    /// Default collapsed/expanded state for cards without an explicit one.
    pub open: bool,
    /// Soft cap for text rendered inline before truncation.
    pub max_text: usize,
    /// Fields always shown as chips in headers even if not in `badges`.
    pub header_fields: Vec<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            ignore: vec![
                "schema_version".into(),
                "monotonic_ns".into(),
                "event_id".into(),
                "block_id".into(),
                "provider_response_id".into(),
                "response_fingerprint".into(),
                "output_limit_bytes".into(),
            ],
            open: false,
            max_text: 20_000,
            header_fields: vec!["origin".into(), "substrate".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TableSpec {
    /// Dot-separated field paths to show as columns, in order.
    pub columns: Vec<String>,
    /// Column header labels (defaults to the path itself).
    pub labels: BTreeMap<String, String>,
    /// Truncate cell text to this many characters.
    pub max_cell: usize,
}

impl TableSpec {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ValueStyle {
    /// Tone / colour for the chip.
    pub tone: Option<String>,
    /// Prefix string prepended to the rendered value.
    pub prefix: Option<String>,
    /// Optional replacement label (e.g. map `false` -> `FAIL`).
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ConditionalStyle {
    /// All listed field paths must deeply equal these values.
    pub when: BTreeMap<String, serde_json::Value>,
    /// Style applied when `when` matches.
    pub style: CardStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CardStyle {
    /// Tone override for the card.
    pub tone: Option<String>,
    /// Left border weight: `none | normal | bold`.
    pub weight: Option<String>,
    /// Optional label prefix shown in the header.
    pub label_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutcomeSpec {
    /// Field paths that indicate success, e.g. `{t: terminal}` — the last
    /// event matching all `when` fields decides the chip.
    pub when: BTreeMap<String, serde_json::Value>,
    /// Field on the matched event holding the boolean outcome.
    pub field: Option<String>,
}

impl Default for OutcomeSpec {
    fn default() -> Self {
        // Sensible default for agentic traces: a `terminal` event with a
        // boolean `success` field decides the run outcome.
        let mut when = BTreeMap::new();
        when.insert(
            "t".to_string(),
            serde_json::Value::String("terminal".into()),
        );
        OutcomeSpec {
            when,
            field: Some("success".into()),
        }
    }
}

impl FormatSpec {
    pub fn event(&self, ty: &str) -> EventSpec {
        self.events.get(ty).cloned().unwrap_or_default()
    }

    pub fn label(&self, ty: &str) -> String {
        self.event(ty)
            .label
            .clone()
            .unwrap_or_else(|| ty.replace('_', " "))
    }

    /// Resolve a tone: palette names pass through, hex values pass through,
    /// unknown names fall back to a stable hash-based tone.
    pub fn tone(&self, ty: &str) -> String {
        self.event(ty).tone.unwrap_or_else(|| default_tone(ty))
    }

    /// Duration field (milliseconds) for timeline accounting, if any.
    pub fn duration_field(&self, ty: &str) -> Option<String> {
        self.event(ty)
            .duration_field
            .clone()
            .or_else(|| self.timeline.get(ty).cloned())
            .or_else(|| self.trace.duration_field.clone())
    }
}

/// Default palette assignment for unseen event types so every distinct type
/// gets a stable, distinguishable colour even without format config.
pub fn default_tone(ty: &str) -> String {
    match ty {
        "llm_request" => "violet",
        "llm_response" => "emerald",
        "assistant" | "message" => "emerald",
        "tool_call" => "sky",
        "tool_result" => "slate",
        "instrument_action" => "cyan",
        "progress" => "amber",
        "finish" => "lime",
        "terminal" | "critical_stop" => "red",
        "run_audited" => "violet",
        _ => TONES[fnv(ty) % TONES.len()],
    }
    .to_string()
}

pub const TONES: [&str; 10] = [
    "violet", "sky", "emerald", "amber", "cyan", "pink", "lime", "orange", "blue", "rose",
];

fn fnv(s: &str) -> usize {
    s.bytes().fold(0xcbf29ce484222325usize, |h, b| {
        (h ^ b as usize).wrapping_mul(0x100000001b3)
    })
}
