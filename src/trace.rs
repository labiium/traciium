//! JSONL trace parsing, run grouping, per-run statistics and timeline spans.

use crate::format::FormatSpec;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// One parsed JSONL record plus the core keys extracted for sorting/grouping.
#[derive(Debug, Clone)]
pub struct Event {
    pub ty: String,
    pub ts: Option<f64>,
    pub seq: Option<f64>,
    pub run: Option<String>,
    pub record: Value,
}

/// A contiguous timeline segment used by the "where did the time go" strip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Span {
    pub kind: String, // "llm" | "tool" | "gap" | "point"
    pub ty: String,
    pub tone: String,
    pub label: String,
    pub start: f64,
    pub end: f64,
    pub seq: Option<f64>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RunStats {
    pub events: usize,
    pub duration_s: f64,
    pub llm_time_s: f64,
    pub tool_time_s: f64,
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub success: Option<bool>,
    pub outcome_label: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Run {
    pub id: String,
    pub label: String,
    pub file: usize,
    pub stats: RunStats,
    pub t_min: f64,
    pub t_max: f64,
    pub spans: Vec<Span>,
    pub events: Vec<Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceFile {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub bytes: u64,
    pub runs: Vec<Run>,
}

/// Read a field via a dot-separated path, e.g. `usage.total` or `items.0`.
pub fn get_path<'a>(record: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(record, |v, seg| match v.get(seg) {
        Some(x) => Some(x),
        // fall back to numeric index for arrays
        None => v.get(seg.parse::<usize>().ok()?),
    })
}

/// Parse a timestamp that is unix seconds, unix milliseconds (heuristic:
/// magnitude > 1e12), or an ISO-8601 string. Returns unix seconds.
pub fn parse_ts(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => {
            let s = n.as_f64()?;
            Some(if s > 1e12 { s / 1000.0 } else { s })
        }
        Some(Value::String(s)) => parse_iso(s),
        _ => None,
    }
}

/// Minimal ISO-8601 / RFC-3339 parser (no external chrono dependency).
/// Handles `2026-07-26T13:58:16.590512+00:00` and `...Z` variants.
pub fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let (time, offset) = match rest.rsplit_once('+') {
        Some((t, o)) if o.len() == 5 => (t, Some((o, 1.0))),
        _ => match rest.rsplit_once('-') {
            // A minus could belong to the fraction; only treat as offset if
            // it appears after a time portion with hh:mm shape.
            Some((t, o)) if t.contains(':') && o.len() == 5 => (t, Some((o, -1.0))),
            _ => (rest, None),
        },
    };
    let mut tp = time.split(':');
    let h: f64 = tp.next().unwrap_or("0").parse().ok()?;
    let mi: f64 = tp.next().unwrap_or("0").parse().ok()?;
    let sec: f64 = tp.next().unwrap_or("0").parse().ok()?;
    let days = days_from_civil(y, mo, d);
    let mut unix = days * 86400.0 + h * 3600.0 + mi * 60.0 + sec;
    if let Some((o, sign)) = offset {
        let mut op = o.split(':');
        let oh: f64 = op.next().unwrap_or("0").parse().ok()?;
        let om: f64 = op.next().unwrap_or("0").parse().ok()?;
        unix -= sign * (oh * 3600.0 + om * 60.0);
    }
    Some(unix)
}

/// Howard Hinnant's days_from_civil algorithm (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> f64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) as f64
}

/// Parse one .jsonl file into grouped runs using the format spec.
pub fn load_file(path: &str, index: usize, spec: &FormatSpec) -> std::io::Result<TraceFile> {
    let reader = BufReader::new(File::open(path)?);
    let meta = std::fs::metadata(path)?;
    let mut events: Vec<Event> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Tolerate concatenation artifacts: parse the longest valid prefix
        // object by attempting decreasing slices on failure.
        let parsed = serde_json::from_str::<Value>(trimmed)
            .ok()
            .or_else(|| recover_prefix(trimmed));
        if let Some(Value::Object(map)) = parsed {
            events.push(extract(map, spec));
        }
    }
    events.sort_by(|a, b| {
        a.ts.unwrap_or(0.)
            .total_cmp(&b.ts.unwrap_or(0.))
            .then(a.seq.unwrap_or(0.).total_cmp(&b.seq.unwrap_or(0.)))
    });

    let file_name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let dir_name = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Group into runs (preserve first-seen order).
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    for ev in events {
        let key = ev.run.clone().unwrap_or_else(|| "all".into());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(ev);
    }
    if order.is_empty() {
        order.push("all".into());
        groups.entry("all".into()).or_default();
    }

    let runs = order
        .into_iter()
        .map(|id| {
            let events = groups.remove(&id).unwrap_or_default();
            build_run(id, events, index, spec)
        })
        .collect();

    Ok(TraceFile {
        path: path.to_string(),
        name: file_name,
        dir: dir_name,
        bytes: meta.len(),
        runs,
    })
}

fn recover_prefix(s: &str) -> Option<Value> {
    // Handles accidental `}{` joins: try to find an object boundary.
    if let Some(pos) = s.find("}{") {
        if let Ok(v) = serde_json::from_str::<Value>(&s[..pos + 1]) {
            return Some(v);
        }
    }
    None
}

fn extract(map: Map<String, Value>, spec: &FormatSpec) -> Event {
    let root = Value::Object(map);
    let ty = get_path(&root, &spec.trace.type_field)
        .and_then(|v| v.as_str())
        .unwrap_or("event")
        .to_string();
    Event {
        ty,
        ts: parse_ts(get_path(&root, &spec.trace.timestamp_field)),
        seq: spec
            .trace
            .sequence_field
            .as_deref()
            .and_then(|f| get_path(&root, f))
            .and_then(|v| v.as_f64()),
        run: spec
            .trace
            .run_field
            .as_deref()
            .and_then(|f| get_path(&root, f))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        record: root,
    }
}

fn build_run(id: String, events: Vec<Event>, file: usize, spec: &FormatSpec) -> Run {
    let t_min = events
        .iter()
        .filter_map(|e| e.ts)
        .fold(f64::INFINITY, f64::min);
    let t_max = events
        .iter()
        .filter_map(|e| e.ts)
        .fold(f64::NEG_INFINITY, f64::max);
    let duration = if t_max > t_min { t_max - t_min } else { 0.0 };

    let mut stats = RunStats {
        events: events.len(),
        duration_s: duration,
        ..Default::default()
    };

    // Outcome: last event matching the outcome spec (or terminal-flagged).
    let mut outcome_ev: Option<&Event> = None;
    for ev in &events {
        let es = spec.event(&ev.ty);
        let is_outcome = es.terminal.unwrap_or(false)
            || (!spec.outcome.when.is_empty()
                && spec
                    .outcome
                    .when
                    .iter()
                    .all(|(k, v)| get_path(&ev.record, k).map(|x| x == v).unwrap_or(false)));
        if is_outcome {
            outcome_ev = Some(ev);
        }
    }
    if let Some(ev) = outcome_ev {
        if let Some(field) = &spec.outcome.field {
            stats.success = get_path(&ev.record, field).and_then(|v| v.as_bool());
            if stats.success.is_none() {
                stats.outcome_label = get_path(&ev.record, field).map(short_value);
            }
        }
    }

    // Aggregate time + token usage.
    let mut spans: Vec<Span> = Vec::new();
    for ev in &events {
        let es = spec.event(&ev.ty);
        let ts = ev.ts.unwrap_or(0.0);
        let dur_field = spec.duration_field(&ev.ty).map(|s| s.to_string());
        let dur_ms = dur_field
            .as_deref()
            .and_then(|f| get_path(&ev.record, f))
            .and_then(|v| v.as_f64());

        if let Some(ms) = dur_ms {
            let secs = ms / 1000.0;
            if ev.ty.contains("llm") || ev.ty.contains("response") {
                stats.llm_time_s += secs;
            } else if ev.ty.contains("tool") || ev.ty.contains("instrument") {
                stats.tool_time_s += secs;
            }
            spans.push(Span {
                kind: "span".into(),
                ty: ev.ty.clone(),
                tone: spec.tone(&ev.ty),
                label: spec.label(&ev.ty),
                start: ts - secs,
                end: ts,
                seq: ev.seq,
            });
        } else if !es.compact.unwrap_or(false) {
            spans.push(Span {
                kind: "point".into(),
                ty: ev.ty.clone(),
                tone: spec.tone(&ev.ty),
                label: spec.label(&ev.ty),
                start: ts,
                end: ts,
                seq: ev.seq,
            });
        }

        if ev.ty.contains("tool_call") {
            stats.tool_calls += 1;
        }
        if (ev.ty.contains("tool") || ev.ty.contains("instrument"))
            && get_path(&ev.record, "ok") == Some(&Value::Bool(false))
        {
            stats.tool_errors += 1;
        }
        if let Some(usage) = get_path(&ev.record, "usage") {
            stats.tokens_prompt += usage.get("prompt").and_then(|v| v.as_u64()).unwrap_or(0);
            stats.tokens_completion += usage
                .get("completion")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }
    }

    // Idle gaps = the complement of busy spans within [t_min, t_max], so
    // latency already accounted by an llm/tool span is never double-counted.
    if duration > 0.0 {
        let mut busy: Vec<(f64, f64)> = spans
            .iter()
            .filter(|s| s.kind != "point")
            .map(|s| (s.start.max(t_min), s.end.min(t_max)))
            .filter(|(a, b)| b > a)
            .collect();
        busy.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cursor = t_min;
        for (a, b) in busy {
            if a - cursor > 0.05 {
                spans.push(Span {
                    kind: "gap".into(),
                    ty: "gap".into(),
                    tone: "gap".into(),
                    label: format!("idle {:.1}s", a - cursor),
                    start: cursor,
                    end: a,
                    seq: None,
                });
            }
            cursor = cursor.max(b);
        }
        if t_max - cursor > 0.05 {
            spans.push(Span {
                kind: "gap".into(),
                ty: "gap".into(),
                tone: "gap".into(),
                label: format!("idle {:.1}s", t_max - cursor),
                start: cursor,
                end: t_max,
                seq: None,
            });
        }
    }
    spans.sort_by(|a, b| a.start.total_cmp(&b.start));

    // Human label: prefer block/task-ish ids over giant run ids.
    let label = if id.len() > 58 {
        format!("{}…", &id[..57])
    } else {
        id.clone()
    };

    Run {
        id,
        label,
        file,
        stats,
        t_min: if t_min.is_finite() { t_min } else { 0.0 },
        t_max: if t_max.is_finite() { t_max } else { 0.0 },
        spans,
        events: events.into_iter().map(|e| e.record).collect(),
    }
}

fn short_value(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamps() {
        assert_eq!(
            parse_iso("2026-07-26T13:58:16.590512+00:00"),
            Some(1785074296.590512)
        );
        assert_eq!(parse_iso("2026-07-26T13:58:16Z"), Some(1785074296.0));
        assert_eq!(parse_iso("2026-07-26T09:58:16-04:00"), Some(1785074296.0));
        assert_eq!(
            parse_ts(Some(&Value::from(1785074296.25))),
            Some(1785074296.25)
        );
    }

    #[test]
    fn dot_paths() {
        let v: Value = serde_json::from_str(r#"{"usage":{"total":9},"a":[1,2]}"#).unwrap();
        assert_eq!(get_path(&v, "usage.total"), Some(&Value::from(9)));
        assert_eq!(get_path(&v, "a.1"), Some(&Value::from(2)));
        assert_eq!(get_path(&v, "missing.x"), None);
    }

    #[test]
    fn run_grouping_and_stats() {
        let spec: FormatSpec = serde_yaml::from_str(
            "trace:\n  type_field: t\ntimeline:\n  llm_response: latency_ms\n  tool_result: duration_ms",
        )
        .unwrap();
        let dir = std::env::temp_dir().join("traciium-test-run");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"t":"llm_request","ts":98.0,"sequence":0,"run_id":"a"}"#,
                "\n",
                r#"{"t":"llm_response","ts":100.0,"sequence":1,"run_id":"a","latency_ms":2000,"usage":{"prompt":10,"completion":5}}"#,
                "\n",
                r#"{"t":"tool_call","ts":100.1,"sequence":2,"run_id":"a","name":"Bash","call_id":"c1"}"#,
                "\n",
                r#"{"t":"tool_result","ts":100.5,"sequence":3,"run_id":"a","call_id":"c1","duration_ms":400,"ok":false}"#,
                "\n",
                r#"{"t":"terminal","ts":101.0,"sequence":4,"run_id":"a","success":false}"#,
                "\n"
            ),
        )
        .unwrap();
        let f = load_file(path.to_str().unwrap(), 0, &spec).unwrap();
        assert_eq!(f.runs.len(), 1);
        let s = &f.runs[0].stats;
        assert_eq!(s.events, 5);
        assert_eq!(s.success, Some(false));
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.tool_errors, 1);
        assert!((s.llm_time_s - 2.0).abs() < 1e-9);
        assert!((s.tool_time_s - 0.4).abs() < 1e-9);
        assert_eq!(s.tokens_prompt, 10);
        // gaps must not double-count busy spans
        let busy: f64 = f.runs[0]
            .spans
            .iter()
            .filter(|s| s.kind != "gap" && s.kind != "point")
            .map(|s| s.end - s.start)
            .sum();
        let gaps: f64 = f.runs[0]
            .spans
            .iter()
            .filter(|s| s.kind == "gap")
            .map(|s| s.end - s.start)
            .sum();
        assert!((busy + gaps - 3.0).abs() < 1e-6);
    }
}
