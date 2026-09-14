//! Port of packages/ai/src/utils/diagnostics.ts

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// `string | number` in the TypeScript; `Value` keeps both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub type_: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

/// Rust counterpart of the TypeScript `unknown` thrown value.
pub enum ThrownValue<'a> {
    Error(&'a (dyn std::error::Error + 'static)),
    Text(&'a str),
    Json(&'a Value),
}

pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn format_thrown_value(value: &ThrownValue<'_>) -> String {
    match value {
        ThrownValue::Error(error) => {
            let message = error.to_string();
            if message.is_empty() {
                // `value.name` for a plain JS Error.
                "Error".to_string()
            } else {
                message
            }
        }
        ThrownValue::Text(text) => (*text).to_string(),
        ThrownValue::Json(value) => match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
    }
}

pub fn extract_diagnostic_error(error: &ThrownValue<'_>) -> DiagnosticErrorInfo {
    match error {
        ThrownValue::Error(err) => {
            let message = err.to_string();
            DiagnosticErrorInfo {
                // Rust errors do not carry a JS `name`; a real error type maps to "Error".
                name: Some("Error".to_string()),
                message: if message.is_empty() {
                    "Error".to_string()
                } else {
                    message
                },
                stack: None,
                code: None,
            }
        }
        other => DiagnosticErrorInfo {
            name: Some("ThrownValue".to_string()),
            message: format_thrown_value(other),
            stack: None,
            code: None,
        },
    }
}

/// `extractDiagnosticError` for a value that is not an `Error` instance.
pub fn extract_diagnostic_error_value(value: &Value) -> DiagnosticErrorInfo {
    DiagnosticErrorInfo {
        name: Some("ThrownValue".to_string()),
        message: format_thrown_value(&ThrownValue::Json(value)),
        stack: None,
        code: None,
    }
}

pub fn create_assistant_message_diagnostic(
    type_: &str,
    error: &ThrownValue<'_>,
    details: Option<Map<String, Value>>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        type_: type_.to_string(),
        timestamp: now_millis(),
        error: Some(extract_diagnostic_error(error)),
        details,
    }
}

pub fn create_assistant_message_diagnostic_from_value(
    type_: &str,
    error: &Value,
    details: Option<Map<String, Value>>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        type_: type_.to_string(),
        timestamp: now_millis(),
        error: Some(extract_diagnostic_error_value(error)),
        details,
    }
}

/// `appendAssistantMessageDiagnostic<T extends { diagnostics?: ... }>`.
pub trait HasDiagnostics {
    fn diagnostics_field(&mut self) -> &mut Option<Vec<AssistantMessageDiagnostic>>;
}

pub fn append_assistant_message_diagnostic<T: HasDiagnostics>(
    message: &mut T,
    diagnostic: AssistantMessageDiagnostic,
) {
    let mut next = message.diagnostics_field().clone().unwrap_or_default();
    next.push(diagnostic);
    *message.diagnostics_field() = Some(next);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct FakeMessage {
        diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    }

    impl HasDiagnostics for FakeMessage {
        fn diagnostics_field(&mut self) -> &mut Option<Vec<AssistantMessageDiagnostic>> {
            &mut self.diagnostics
        }
    }

    #[test]
    fn format_thrown_value_handles_strings_and_errors() {
        assert_eq!(format_thrown_value(&ThrownValue::Text("boom")), "boom");
        let error = std::io::Error::new(std::io::ErrorKind::Other, "io boom");
        assert_eq!(format_thrown_value(&ThrownValue::Error(&error)), "io boom");
    }

    #[test]
    fn extract_diagnostic_error_marks_non_errors_as_thrown_value() {
        let info = extract_diagnostic_error(&ThrownValue::Json(&json!(42)));
        assert_eq!(info.name.as_deref(), Some("ThrownValue"));
        assert_eq!(info.message, "42");
        assert!(info.stack.is_none());
        assert!(info.code.is_none());
    }

    #[test]
    fn append_keeps_order_and_creates_missing_list() {
        let mut message = FakeMessage::default();
        let first = create_assistant_message_diagnostic("http-error", &ThrownValue::Text("a"), None);
        let second = create_assistant_message_diagnostic("retry", &ThrownValue::Text("b"), None);
        append_assistant_message_diagnostic(&mut message, first);
        append_assistant_message_diagnostic(&mut message, second);
        let diagnostics = message.diagnostics.unwrap();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].type_, "http-error");
        assert_eq!(diagnostics[1].type_, "retry");
    }

    #[test]
    fn diagnostic_serialises_expected_field_names() {
        let diagnostic = AssistantMessageDiagnostic {
            type_: "stream-failure".to_string(),
            timestamp: 1,
            error: Some(DiagnosticErrorInfo {
                name: Some("Error".to_string()),
                message: "m".to_string(),
                stack: None,
                code: Some(json!(500)),
            }),
            details: None,
        };
        assert_eq!(
            serde_json::to_string(&diagnostic).unwrap(),
            "{\"type\":\"stream-failure\",\"timestamp\":1,\"error\":{\"name\":\"Error\",\"message\":\"m\",\"code\":500}}"
        );
    }
}
