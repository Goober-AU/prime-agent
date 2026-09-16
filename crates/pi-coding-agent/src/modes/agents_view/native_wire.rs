//! Adapt JavaScript-number wire values to the browser's integer counters.
use serde_json::Value;

pub fn normalize_browser_numbers(mut value: Value) -> Value {
    fn visit(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                // Exceptional worker state is carried outside the nested summary.
                for key in ["statusLabel", "lastHeardFromAt"] {
                    if let Some(value) = fields.get(key).filter(|v| !v.is_null()).cloned() {
                        if let Some(summary) = fields.get_mut("summary").and_then(Value::as_object_mut) {
                            summary.insert(key.into(), value);
                        }
                    }
                }
                fields.values_mut().for_each(visit);
                if is_background_only_fields(fields) {
                    // This is a UI-owned copy, never the daemon's lifecycle state.
                    fields.insert("rosterStatus".into(), Value::from("idle"));
                    fields.insert("statusLabel".into(), Value::from("background helper"));
                }
                if fields.get("summary").and_then(|s| s.get("statusLabel")).and_then(Value::as_str) == Some("background helper")
                    && fields.get("statusLabel").is_none_or(Value::is_null)
                    && fields.get("lastHeardFromAt").is_none_or(Value::is_null)
                {
                    fields.insert("status".into(), Value::from("idle"));
                }
            }
            Value::Array(items) => items.iter_mut().for_each(visit),
            Value::Number(number) if number.is_f64() => {
                if let Some(n) = number.as_f64() {
                    // Only exact safe integers: never round fractional counters
                    // or alter high-precision integers already encoded as integers.
                    if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 {
                        *value = Value::from(n as i64);
                    }
                }
            }
            _ => {}
        }
    }
    visit(&mut value);
    value
}

pub fn is_background_only(value: &Value) -> bool {
    value.as_object().is_some_and(is_background_only_fields)
}

fn is_background_only_fields(fields: &serde_json::Map<String, Value>) -> bool {
    let get = |key: &str| fields.get(key).unwrap_or(&Value::Null);
    get("runtimeKind") == "subagent"
        && get("activeSessionId").as_str().is_some_and(|s| !s.is_empty())
        && get("isSessionActive") == true
        && get("isStreaming") == false
        && get("isCompacting") == false
        && get("isRunningTools") != true
        && get("isBashRunning") != true
        && get("hasRunningRlmChildren") != true
        && get("sessionActions").get("active").is_none_or(Value::is_null)
        && get("sessionActions").get("queuedCount").and_then(Value::as_f64).unwrap_or(0.0) <= 0.0
        && get("statusLabel").is_null()
        && get("lastHeardFromAt").is_null()
        && (get("workerState").is_null() || get("workerState") == "ready")
}
