//! Pure formatting for the companion's stdout (port of lib/render.mjs).
//!
//! Output is byte-identical to the Node runtime for every record either
//! runtime writes. Job records are untrusted JSON (a user or another tool can
//! edit `.agy-plugin/jobs/*.json`), so field access follows JS semantics —
//! `String(x)`, `x ?? d`, truthiness — instead of assuming types; that keeps
//! odd records rendering the way they used to, and never panics.

use crate::json::{Value, to_string_compact};
use crate::time::{iso_from_millis, parse_iso_millis};

const MS_PER_DAY: i64 = 86_400_000;
const JS_MAX_MS: i64 = 8_640_000_000_000_000;
const DASH: &str = "—";

/// How a cancel attempt ended (the JS `outcome` object, typed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    Canceled { reason: String },
    NotFound,
    NotCancelable { status: Option<String> },
}

pub(crate) fn status_badge(status: &str) -> String {
    match status {
        "pending" => "[pending]".to_string(),
        "running" => "[running]".to_string(),
        "completed" => "[done]".to_string(),
        "failed" => "[failed]".to_string(),
        "canceled" => "[cancel]".to_string(),
        other => format!("[{other}]"),
    }
}

/// Timestamp column: `HH:MM:SS` when it falls on the same UTC day as
/// `now_ms`, else `YYYY-MM-DD HH:MM:SS`; `—` when absent/empty; the raw
/// text when unparseable.
pub(crate) fn short_time(iso: Option<&str>, now_ms: i64) -> String {
    match iso {
        None | Some("") => DASH.to_string(),
        Some(s) => match parse_iso_millis(s) {
            Some(ms) => short_time_ms(ms, now_ms),
            None => s.to_string(),
        },
    }
}

fn short_time_ms(ms: i64, now_ms: i64) -> String {
    let iso = iso_from_millis(ms);
    // JS slices fixed positions of toISOString(); for extended years
    // (`+010000-…`) that yields a shifted slice, reproduced as-is. The ISO
    // string is ASCII, so byte slicing is char slicing.
    if ms.div_euclid(MS_PER_DAY) == now_ms.div_euclid(MS_PER_DAY) {
        iso.get(11..19).unwrap_or_default().to_string()
    } else {
        iso.get(0..19).unwrap_or_default().replacen('T', " ", 1)
    }
}

/// `shortTime(v)` for an arbitrary record field, mirroring `new Date(v)`
/// for the non-string values a hand-edited record may hold.
fn short_time_value(v: Option<&Value>, now_ms: i64) -> String {
    match v {
        None | Some(Value::Null) => DASH.to_string(),
        Some(Value::String(s)) => short_time(Some(s), now_ms),
        Some(Value::Bool(false)) => DASH.to_string(),
        Some(Value::Bool(true)) => short_time_ms(1, now_ms),
        Some(Value::Number(n)) => {
            if *n == 0.0 || n.is_nan() {
                DASH.to_string()
            } else if n.is_finite() && n.trunc().abs() <= JS_MAX_MS as f64 {
                // TimeClip truncates toward zero; the range check makes the
                // cast exact.
                short_time_ms(n.trunc() as i64, now_ms)
            } else {
                js_string(v)
            }
        }
        // new Date(array|object) goes through ToPrimitive -> string.
        Some(other) => {
            let s = js_string(Some(other));
            match parse_iso_millis(&s) {
                Some(ms) => short_time_ms(ms, now_ms),
                None => s,
            }
        }
    }
}

/// Length in UTF-16 code units, which is what JS `.length` counts.
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Clip to `max` UTF-16 units, ending in `…` when clipped (JS parity).
///
/// When JS's `slice` cuts an astral character in half it keeps a lone high
/// surrogate, which Node writes to stdout as U+FFFD; we emit U+FFFD in that
/// spot so the bytes on stdout match.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if utf16_len(s) <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out = String::new();
    let mut units = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if units + w > keep {
            if units < keep {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        units += w;
    }
    out.push('…');
    out
}

/// `String.prototype.padEnd(n)` with spaces, counting UTF-16 units.
fn pad_end(s: &str, n: usize) -> String {
    let mut out = s.to_string();
    for _ in utf16_len(s)..n {
        out.push(' ');
    }
    out
}

/// JS `String(v)`; `None` is `undefined`.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => to_string_compact(n),
        Some(Value::String(s)) => s.clone(),
        // Array.prototype.join: null/undefined elements become "".
        Some(Value::Array(a)) => a
            .iter()
            .map(|e| {
                if e.is_null() {
                    String::new()
                } else {
                    js_string(Some(e))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// JS `String(v ?? default)`.
fn or_default(v: Option<&Value>, default: &str) -> String {
    match v {
        None | Some(Value::Null) => default.to_string(),
        some => js_string(some),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

fn badge_of(r: &Value) -> String {
    status_badge(&js_string(r.get("status")))
}

/// Multi-job table for /agy:status.
pub(crate) fn render_status_list(records: &[Value], now_ms: i64) -> String {
    if records.is_empty() {
        return "No agy jobs recorded in this workspace yet.\n".to_string();
    }
    let mut lines = vec!["ID            STATUS    KIND               STARTED   TASK".to_string()];
    for r in records {
        let started = match r.get("startedAt") {
            None | Some(Value::Null) => r.get("createdAt"),
            some => some,
        };
        lines.push(
            [
                pad_end(&js_string(r.get("id")), 13),
                pad_end(&badge_of(r), 9),
                pad_end(&clip(&or_default(r.get("kind"), ""), 17), 18),
                pad_end(&short_time_value(started, now_ms), 9),
                clip(&or_default(r.get("task"), ""), 48),
            ]
            .join(" "),
        );
    }
    lines.join("\n") + "\n"
}

/// Single-job detail block for /agy:status <id>.
pub(crate) fn render_job_detail(record: Option<&Value>, now_ms: i64) -> String {
    let Some(r) = record.filter(|r| truthy(Some(r))) else {
        return "Job not found.\n".to_string();
    };
    let mut lines = vec![
        format!("Job:          {}", js_string(r.get("id"))),
        format!("Status:       {}", badge_of(r)),
        format!("Kind:         {}", or_default(r.get("kind"), DASH)),
        format!("Model:        {}", or_default(r.get("model"), "(default)")),
        format!(
            "Task:         {}",
            clip(&or_default(r.get("task"), ""), 200)
        ),
        format!(
            "Created:      {}",
            short_time_value(r.get("createdAt"), now_ms)
        ),
        format!(
            "Started:      {}",
            short_time_value(r.get("startedAt"), now_ms)
        ),
        format!(
            "Completed:    {}",
            short_time_value(r.get("completedAt"), now_ms)
        ),
        format!("Exit code:    {}", or_default(r.get("exitCode"), DASH)),
        format!("PID:          {}", or_default(r.get("pid"), DASH)),
        format!("Log file:     {}", or_default(r.get("logFile"), DASH)),
        format!("Workspace:    {}", or_default(r.get("workspaceRoot"), DASH)),
    ];
    if truthy(r.get("livenessDowngrade")) {
        lines.push(
            "Note:         PID is dead but the record claimed running; treated as failed."
                .to_string(),
        );
    }
    lines.join("\n") + "\n"
}

/// /agy:result output: the detail block, then the captured log.
pub(crate) fn render_result(record: Option<&Value>, log: &str, now_ms: i64) -> String {
    if !record.is_some_and(|r| truthy(Some(r))) {
        return "No job to report on.\n".to_string();
    }
    let head = render_job_detail(record, now_ms);
    let body = if log.is_empty() {
        "(log file is empty or missing)"
    } else {
        log
    };
    let nl = if body.ends_with('\n') { "" } else { "\n" };
    format!("{head}\n--- output ---\n{body}{nl}")
}

/// /agy:cancel report — one paragraph the user can paste into a ticket.
pub(crate) fn render_cancel_report(id: &str, outcome: &CancelOutcome) -> String {
    match outcome {
        CancelOutcome::Canceled { reason } => format!("Job {id}: canceled ({reason}).\n"),
        CancelOutcome::NotFound => format!("Job {id}: not found in this workspace.\n"),
        CancelOutcome::NotCancelable { status } => format!(
            "Job {id}: already finished (status: {}); nothing to cancel.\n",
            status.as_deref().unwrap_or("unknown")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    // Expected outputs below were generated by running the original
    // lib/render.mjs under Node with `Date` pinned to NOW (scratchpad
    // gen_render.mjs). Lone surrogates were passed through Node's UTF-8
    // encoder, i.e. they are the exact bytes Node writes to stdout.
    const NOW: i64 = 1779993000000;
    const JS_LIST_JSON: &str = r##"[{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-abcdef99","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"completed","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-27T23:59:59.999Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-pending1","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"pending","createdAt":"2026-05-28T00:00:00.000Z","startedAt":null,"completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-failed01","kind":"adversarial-review-extended","task":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx","model":"opus","status":"failed","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-cancel01","kind":"rescue","task":"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy😀😀","model":"opus","status":"canceled","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-cancel02","kind":"rescue","task":"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy😀😀","model":"opus","status":"canceled","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-weird-status-and-long-id","kind":null,"task":null,"model":"opus","status":"weird","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-bare"},{"id":"agy-ts","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"not a date","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-ts2","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T20:00:00+02:00","startedAt":"","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-ts3","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"+010000-01-01T00:00:00.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-ts4","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":1780000000000,"completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"},{"id":"agy-ts5","kind":"ünïcödé-kind-name-long","task":"привет мир","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"1969-12-31T23:59:59.999Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}]"##;
    const JS_LIST: &str = "ID            STATUS    KIND               STARTED   TASK\nagy-3a7b9c12  [running] rescue             15:00:01  investigate the flaky integration test\nagy-abcdef99  [done]    rescue             2026-05-27 23:59:59 investigate the flaky integration test\nagy-pending1  [pending] rescue             00:00:00  investigate the flaky integration test\nagy-failed01  [failed]  adversarial-revi\u{2026}  15:00:01  xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\u{2026}\nagy-cancel01  [cancel]  rescue             15:00:01  yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\u{fffd}\u{2026}\nagy-cancel02  [cancel]  rescue             15:00:01  yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\u{2026}\nagy-weird-status-and-long-id [weird]                      15:00:01  \nagy-bare      [undefined]                    \u{2014}         \nagy-ts        [running] rescue             not a date investigate the flaky integration test\nagy-ts2       [running] rescue             \u{2014}         investigate the flaky integration test\nagy-ts3       [running] rescue             +010000-01-01 00:00 investigate the flaky integration test\nagy-ts4       [running] rescue             20:26:40  investigate the flaky integration test\nagy-ts5       [running] ünïcödé-kind-nam\u{2026}  1969-12-31 23:59:59 привет мир\n";
    const JS_DETAIL_SAMPLE_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}"##;
    const JS_DETAIL_SAMPLE: &str = "Job:          agy-3a7b9c12\nStatus:       [running]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    \u{2014}\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n";
    const JS_DETAIL_MINIMAL_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":null,"status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":null,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}"##;
    const JS_DETAIL_MINIMAL: &str = "Job:          agy-3a7b9c12\nStatus:       [running]\nKind:         rescue\nModel:        (default)\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    \u{2014}\nPID:          \u{2014}\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n";
    const JS_DETAIL_DOWNGRADE_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"failed","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":"2026-05-28T15:10:00.000Z","exitCode":1,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo","livenessDowngrade":true}"##;
    const JS_DETAIL_DOWNGRADE: &str = "Job:          agy-3a7b9c12\nStatus:       [failed]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    15:10:00\nExit code:    1\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\nNote:         PID is dead but the record claimed running; treated as failed.\n";
    const JS_DETAIL_EMPTY_MODEL_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":"2025-12-31T23:59:59.000Z","exitCode":0,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}"##;
    const JS_DETAIL_EMPTY_MODEL: &str = "Job:          agy-3a7b9c12\nStatus:       [running]\nKind:         rescue\nModel:        \nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    2025-12-31 23:59:59\nExit code:    0\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n";
    const JS_DETAIL_LONG_TASK_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz😀tail","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}"##;
    const JS_DETAIL_LONG_TASK: &str = "Job:          agy-3a7b9c12\nStatus:       [running]\nKind:         rescue\nModel:        opus\nTask:         zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\u{2026}\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    \u{2014}\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n";
    const JS_DETAIL_ODD_TYPES_JSON: &str = r##"{"id":42,"status":true,"kind":7,"model":false,"task":["a",null,3],"exitCode":1.5,"pid":1e+21,"logFile":{},"workspaceRoot":[],"livenessDowngrade":"yes","createdAt":0,"startedAt":true,"completedAt":1780000000000.9}"##;
    const JS_DETAIL_ODD_TYPES: &str = "Job:          42\nStatus:       [true]\nKind:         7\nModel:        false\nTask:         a,,3\nCreated:      \u{2014}\nStarted:      1970-01-01 00:00:00\nCompleted:    20:26:40\nExit code:    1.5\nPID:          1e+21\nLog file:     [object Object]\nWorkspace:    \nNote:         PID is dead but the record claimed running; treated as failed.\n";
    const JS_DETAIL_EMPTY_OBJ_JSON: &str = r##"{}"##;
    const JS_DETAIL_EMPTY_OBJ: &str = "Job:          undefined\nStatus:       [undefined]\nKind:         \u{2014}\nModel:        (default)\nTask:         \nCreated:      \u{2014}\nStarted:      \u{2014}\nCompleted:    \u{2014}\nExit code:    \u{2014}\nPID:          \u{2014}\nLog file:     \u{2014}\nWorkspace:    \u{2014}\n";
    const JS_DETAIL_FALSY_DOWNGRADE_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"running","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":null,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo","livenessDowngrade":0}"##;
    const JS_DETAIL_FALSY_DOWNGRADE: &str = "Job:          agy-3a7b9c12\nStatus:       [running]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    \u{2014}\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n";
    const JS_RESULT_JSON: &str = r##"{"id":"agy-3a7b9c12","kind":"rescue","task":"investigate the flaky integration test","model":"opus","status":"completed","createdAt":"2026-05-28T15:00:00.000Z","startedAt":"2026-05-28T15:00:01.000Z","completedAt":null,"exitCode":0,"pid":12345,"logFile":".agy-plugin/logs/agy-3a7b9c12.log","workspaceRoot":"/repo"}"##;
    const JS_RESULT_LOG: &str = "Job:          agy-3a7b9c12\nStatus:       [done]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    0\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n\n--- output ---\nthe agy reply\n";
    const JS_RESULT_EMPTY: &str = "Job:          agy-3a7b9c12\nStatus:       [done]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    0\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n\n--- output ---\n(log file is empty or missing)\n";
    const JS_RESULT_NO_NL: &str = "Job:          agy-3a7b9c12\nStatus:       [done]\nKind:         rescue\nModel:        opus\nTask:         investigate the flaky integration test\nCreated:      15:00:00\nStarted:      15:00:01\nCompleted:    \u{2014}\nExit code:    0\nPID:          12345\nLog file:     .agy-plugin/logs/agy-3a7b9c12.log\nWorkspace:    /repo\n\n--- output ---\nno trailing newline here\n";
    const JS_RESULT_NULL: &str = "No job to report on.\n";
    const JS_DETAIL_NULL: &str = "Job not found.\n";
    const JS_LIST_EMPTY: &str = "No agy jobs recorded in this workspace yet.\n";
    const JS_CANCEL_OK: &str = "Job agy-12345678: canceled (killed).\n";
    const JS_CANCEL_NF: &str = "Job agy-deadbeef: not found in this workspace.\n";
    const JS_CANCEL_NC: &str =
        "Job agy-12345678: already finished (status: completed); nothing to cancel.\n";
    const JS_CANCEL_NC_UNKNOWN: &str =
        "Job agy-12345678: already finished (status: unknown); nothing to cancel.\n";

    fn v(json: &str) -> Value {
        match parse(json) {
            Ok(v) => v,
            Err(e) => panic!("bad test JSON: {e}"),
        }
    }

    fn detail(json: &str) -> String {
        render_job_detail(Some(&v(json)), NOW)
    }

    // --- byte-identity with Node ---

    #[test]
    fn js_status_list() {
        let list = v(JS_LIST_JSON);
        let records = list.as_array().unwrap_or_default();
        assert_eq!(records.len(), 13);
        assert_eq!(render_status_list(records, NOW), JS_LIST);
        assert_eq!(render_status_list(&[], NOW), JS_LIST_EMPTY);
    }

    #[test]
    fn js_job_details() {
        assert_eq!(detail(JS_DETAIL_SAMPLE_JSON), JS_DETAIL_SAMPLE);
        assert_eq!(detail(JS_DETAIL_MINIMAL_JSON), JS_DETAIL_MINIMAL);
        assert_eq!(detail(JS_DETAIL_DOWNGRADE_JSON), JS_DETAIL_DOWNGRADE);
        assert_eq!(detail(JS_DETAIL_EMPTY_MODEL_JSON), JS_DETAIL_EMPTY_MODEL);
        assert_eq!(detail(JS_DETAIL_LONG_TASK_JSON), JS_DETAIL_LONG_TASK);
        assert_eq!(detail(JS_DETAIL_ODD_TYPES_JSON), JS_DETAIL_ODD_TYPES);
        assert_eq!(detail(JS_DETAIL_EMPTY_OBJ_JSON), JS_DETAIL_EMPTY_OBJ);
        assert_eq!(
            detail(JS_DETAIL_FALSY_DOWNGRADE_JSON),
            JS_DETAIL_FALSY_DOWNGRADE
        );
        assert_eq!(render_job_detail(None, NOW), JS_DETAIL_NULL);
    }

    #[test]
    fn js_results() {
        let r = v(JS_RESULT_JSON);
        assert_eq!(
            render_result(Some(&r), "the agy reply\n", NOW),
            JS_RESULT_LOG
        );
        assert_eq!(render_result(Some(&r), "", NOW), JS_RESULT_EMPTY);
        assert_eq!(
            render_result(Some(&r), "no trailing newline here", NOW),
            JS_RESULT_NO_NL
        );
        assert_eq!(render_result(None, "anything", NOW), JS_RESULT_NULL);
    }

    #[test]
    fn js_cancel_reports() {
        let ok = CancelOutcome::Canceled {
            reason: "killed".to_string(),
        };
        assert_eq!(render_cancel_report("agy-12345678", &ok), JS_CANCEL_OK);
        assert_eq!(
            render_cancel_report("agy-deadbeef", &CancelOutcome::NotFound),
            JS_CANCEL_NF
        );
        let nc = CancelOutcome::NotCancelable {
            status: Some("completed".to_string()),
        };
        assert_eq!(render_cancel_report("agy-12345678", &nc), JS_CANCEL_NC);
        let nc = CancelOutcome::NotCancelable { status: None };
        assert_eq!(
            render_cancel_report("agy-12345678", &nc),
            JS_CANCEL_NC_UNKNOWN
        );
    }

    // --- ported from tests/lib.render.test.mjs ---

    #[test]
    fn list_empty_state() {
        assert!(
            render_status_list(&[], NOW)
                .to_lowercase()
                .contains("no agy jobs")
        );
    }

    #[test]
    fn list_header_plus_rows() {
        let a = v(JS_DETAIL_SAMPLE_JSON);
        let mut b = a.clone();
        if let Some(m) = b.as_object_mut() {
            m.set("id", "agy-abcdef99");
            m.set("status", "completed");
        }
        let out = render_status_list(&[a, b], NOW);
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("ID            STATUS    KIND"));
        assert!(lines[1].contains("agy-3a7b9c12"));
        assert!(lines[1].contains("[running]"));
        assert!(lines[2].contains("agy-abcdef99"));
        assert!(lines[2].contains("[done]"));
    }

    #[test]
    fn list_clips_long_task() {
        let mut r = v(JS_DETAIL_SAMPLE_JSON);
        if let Some(m) = r.as_object_mut() {
            m.set("task", "x".repeat(200));
        }
        let out = render_status_list(&[r], NOW);
        assert!(out.contains('…'));
        assert!(!out.contains(&"x".repeat(48)));
    }

    #[test]
    fn detail_headline_fields_and_nulls() {
        let out = detail(JS_DETAIL_SAMPLE_JSON);
        for s in [
            "agy-3a7b9c12",
            "[running]",
            "rescue",
            "opus",
            "investigate the flaky integration test",
            "12345",
            ".agy-plugin/logs/agy-3a7b9c12.log",
        ] {
            assert!(out.contains(s), "{s}");
        }
        let out = detail(JS_DETAIL_MINIMAL_JSON);
        assert!(out.contains("Model:        (default)\n"));
        assert!(out.contains("Completed:    —\n"));
        assert!(out.contains("Exit code:    —\n"));
        assert!(out.contains("PID:          —\n"));
        assert!(!out.contains("PID is dead"));
        assert!(detail(JS_DETAIL_DOWNGRADE_JSON).contains("PID is dead"));
    }

    #[test]
    fn detail_not_found_for_falsy_records() {
        for r in ["null", "false", "0", "\"\""] {
            assert_eq!(detail(r), "Job not found.\n", "{r}");
            assert_eq!(
                render_result(Some(&v(r)), "x", NOW),
                "No job to report on.\n"
            );
        }
    }

    #[test]
    fn result_layout() {
        let r = v(JS_RESULT_JSON);
        let out = render_result(Some(&r), "the agy reply\n", NOW);
        assert!(out.contains("agy-3a7b9c12"));
        assert!(out.contains("\n\n--- output ---\nthe agy reply\n"));
        assert!(out.ends_with("reply\n") && !out.ends_with("\n\n"));
        assert!(render_result(Some(&r), "", NOW).contains("(log file is empty"));
        assert!(render_result(Some(&r), "x", NOW).ends_with("x\n"));
    }

    #[test]
    fn cancel_reports_text() {
        let r = render_cancel_report(
            "agy-12345678",
            &CancelOutcome::Canceled {
                reason: "killed".to_string(),
            },
        );
        assert_eq!(r, "Job agy-12345678: canceled (killed).\n");
        assert!(render_cancel_report("x", &CancelOutcome::NotFound).contains("not found"));
    }

    // --- helpers ---

    #[test]
    fn badges() {
        assert_eq!(status_badge("pending"), "[pending]");
        assert_eq!(status_badge("running"), "[running]");
        assert_eq!(status_badge("completed"), "[done]");
        assert_eq!(status_badge("failed"), "[failed]");
        assert_eq!(status_badge("canceled"), "[cancel]");
        assert_eq!(status_badge(""), "[]");
        assert_eq!(status_badge("Running"), "[Running]");
        // JS looked these up on a plain object and crashed or printed a
        // function body; we just bracket them.
        assert_eq!(status_badge("constructor"), "[constructor]");
        assert_eq!(status_badge("__proto__"), "[__proto__]");
    }

    #[test]
    fn short_time_cases() {
        assert_eq!(short_time(None, NOW), "—");
        assert_eq!(short_time(Some(""), NOW), "—");
        assert_eq!(short_time(Some("garbage"), NOW), "garbage");
        assert_eq!(
            short_time(Some("2026-05-28T00:00:00.000Z"), NOW),
            "00:00:00"
        );
        assert_eq!(
            short_time(Some("2026-05-28T23:59:59.999Z"), NOW),
            "23:59:59"
        );
        assert_eq!(
            short_time(Some("2026-05-29T00:00:00.000Z"), NOW),
            "2026-05-29 00:00:00"
        );
        assert_eq!(
            short_time(Some("2026-05-27T23:59:59.999Z"), NOW),
            "2026-05-27 23:59:59"
        );
        // Offsets are normalised to UTC before comparing days.
        assert_eq!(
            short_time(Some("2026-05-29T01:00:00+02:00"), NOW),
            "23:00:00"
        );
        // Pre-1970 `now` and record.
        let pre = -86_400_000 + 5_000;
        assert_eq!(short_time(Some("1969-12-31T12:00:00Z"), pre), "12:00:00");
        assert_eq!(
            short_time(Some("1970-01-01T00:00:00Z"), pre),
            "1970-01-01 00:00:00"
        );
        // Extreme `now` must not overflow.
        assert_eq!(
            short_time(Some("2026-05-28T00:00:00Z"), i64::MIN),
            "2026-05-28 00:00:00"
        );
    }

    #[test]
    fn clip_cases() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcd", 3), "ab…");
        assert_eq!(clip("", 0), "");
        assert_eq!(clip("a", 0), "…");
        assert_eq!(clip("ab", 1), "…");
        // UTF-16 counting: 😀 is two units.
        assert_eq!(clip("😀", 2), "😀");
        assert_eq!(clip("😀a", 2), "\u{fffd}…");
        assert_eq!(clip("a😀b", 3), "a\u{fffd}…");
        assert_eq!(clip("a😀bc", 4), "a😀…");
        assert_eq!(clip("привет", 4), "при…");
    }

    #[test]
    fn pad_end_counts_utf16() {
        assert_eq!(pad_end("ab", 4), "ab  ");
        assert_eq!(pad_end("😀", 4), "😀  ");
        assert_eq!(pad_end("toolong", 3), "toolong");
    }

    #[test]
    fn js_string_conversions() {
        assert_eq!(js_string(None), "undefined");
        assert_eq!(js_string(Some(&v("null"))), "null");
        assert_eq!(
            js_string(Some(&v("[1,[2,null],\"x\",{}]"))),
            "1,2,,x,[object Object]"
        );
        assert_eq!(js_string(Some(&v("1e21"))), "1e+21");
        assert_eq!(js_string(Some(&v("-0"))), "0");
        assert_eq!(or_default(Some(&v("null")), "d"), "d");
        assert_eq!(or_default(Some(&v("\"\"")), "d"), "");
        assert!(!truthy(Some(&v("0"))));
        assert!(truthy(Some(&v("[]"))));
        assert!(truthy(Some(&v("\"0\""))));
    }

    #[test]
    fn hostile_records_do_not_panic() {
        let nasty = [
            "[]",
            "[1,2]",
            "\"str\"",
            "1e308",
            "{\"id\":null,\"status\":null,\"startedAt\":1e300,\"createdAt\":-1e300}",
            "{\"startedAt\":-9e15,\"completedAt\":9e15,\"createdAt\":\"+275760-09-13T00:00:00.001Z\"}",
            "{\"startedAt\":{\"a\":1},\"createdAt\":[\"2026-05-28T01:02:03Z\"]}",
            "{\"task\":\"\\ud83d\",\"id\":\"\\u0000\\u001b[31m\"}",
        ];
        for j in nasty {
            let r = v(j);
            let _ = render_job_detail(Some(&r), NOW);
            let _ = render_result(Some(&r), "log", i64::MAX);
            let _ = render_status_list(std::slice::from_ref(&r), i64::MIN);
        }
        // Array-valued timestamp goes through ToPrimitive like new Date([s]).
        let r = v("{\"id\":\"a\",\"startedAt\":[\"2026-05-28T01:02:03Z\"]}");
        assert!(render_status_list(&[r], NOW).contains("01:02:03 "));
    }
}
