//! Port of packages/coding-agent/src/utils/frontmatter.ts

use serde_json::Value;

pub struct ParsedFrontmatter {
    pub frontmatter: Value,
    pub body: String,
}

fn normalize_newlines(value: &str) -> String {
    let without_bom = value.strip_prefix('\u{feff}').unwrap_or(value);
    without_bom.replace("\r\n", "\n").replace('\r', "\n")
}

fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = normalize_newlines(content);

    if !normalized.starts_with("---") {
        return (None, normalized);
    }

    let end_index = match normalized[3..].find("\n---") {
        Some(index) => 3 + index,
        None => return (None, normalized),
    };

    (
        Some(normalized[4..end_index].to_string()),
        normalized[end_index + 4..].trim().to_string(),
    )
}

/// Parse YAML frontmatter. Invalid YAML is an error, exactly like the TypeScript
/// `yaml.parse` throw.
pub fn parse_frontmatter(content: &str) -> Result<ParsedFrontmatter, serde_yaml::Error> {
    let (yaml_string, body) = extract_frontmatter(content);
    let Some(yaml_string) = yaml_string else {
        return Ok(ParsedFrontmatter {
            frontmatter: Value::Object(serde_json::Map::new()),
            body,
        });
    };

    let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_string)?;
    let frontmatter = yaml_to_json(parsed);
    let frontmatter = match frontmatter {
        Value::Null => Value::Object(serde_json::Map::new()),
        other => other,
    };
    Ok(ParsedFrontmatter { frontmatter, body })
}

pub fn strip_frontmatter(content: &str) -> Result<String, serde_yaml::Error> {
    Ok(parse_frontmatter(content)?.body)
}

fn yaml_to_json(value: serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(boolean) => Value::Bool(boolean),
        serde_yaml::Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                Value::Number(unsigned.into())
            } else if let Some(signed) = number.as_i64() {
                Value::Number(signed.into())
            } else {
                serde_json::Number::from_f64(number.as_f64().unwrap_or(f64::NAN))
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
        serde_yaml::Value::String(string) => Value::String(string),
        serde_yaml::Value::Sequence(sequence) => {
            Value::Array(sequence.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(mapping) => {
            let mut map = serde_json::Map::new();
            for (key, item) in mapping {
                let key = match key {
                    serde_yaml::Value::String(string) => string,
                    other => match yaml_to_json(other) {
                        Value::String(string) => string,
                        other => other.to_string(),
                    },
                };
                map.insert(key, yaml_to_json(item));
            }
            Value::Object(map)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_behind_a_utf8_bom() {
        let input = "\u{feff}---\nname: bom-skill\n---\nBody";
        let result = parse_frontmatter(input).unwrap();
        assert_eq!(result.frontmatter["name"], Value::String("bom-skill".to_string()));
        assert_eq!(result.body, "Body");
    }

    #[test]
    fn parses_keys_and_returns_the_body() {
        let input = "---\nname: \"skill-name\"\ndescription: 'A desc'\nfoo-bar: value\n---\n\nBody text";
        let result = parse_frontmatter(input).unwrap();
        assert_eq!(result.frontmatter["name"], Value::String("skill-name".to_string()));
        assert_eq!(result.frontmatter["description"], Value::String("A desc".to_string()));
        assert_eq!(result.frontmatter["foo-bar"], Value::String("value".to_string()));
        assert_eq!(result.body, "Body text");
    }

    #[test]
    fn normalizes_crlf_newlines() {
        let input = "---\r\nname: test\r\n---\r\nLine one\r\nLine two";
        let result = parse_frontmatter(input).unwrap();
        assert_eq!(result.body, "Line one\nLine two");
    }

    #[test]
    fn throws_on_invalid_yaml() {
        let input = "---\nfoo: [bar\n---\nBody";
        assert!(parse_frontmatter(input).is_err());
    }

    #[test]
    fn parses_block_scalar_yaml() {
        let input = "---\ndescription: |\n  Line one\n  Line two\n---\n\nBody";
        let result = parse_frontmatter(input).unwrap();
        assert_eq!(
            result.frontmatter["description"],
            Value::String("Line one\nLine two\n".to_string())
        );
        assert_eq!(result.body, "Body");
    }

    #[test]
    fn returns_the_original_content_without_frontmatter() {
        let result = parse_frontmatter("Just text\nsecond line").unwrap();
        assert_eq!(result.body, "Just text\nsecond line");

        let missing_end = parse_frontmatter("---\nname: test\nBody without terminator").unwrap();
        assert_eq!(missing_end.body, "---\nname: test\nBody without terminator");
    }

    #[test]
    fn returns_an_empty_object_for_comment_only_frontmatter() {
        let result = parse_frontmatter("---\n# just a comment\n---\nBody").unwrap();
        assert_eq!(result.frontmatter, Value::Object(serde_json::Map::new()));
    }

    #[test]
    fn strip_frontmatter_trims_the_body() {
        assert_eq!(strip_frontmatter("---\nkey: value\n---\n\nBody\n").unwrap(), "Body");
        assert_eq!(strip_frontmatter("\n  No frontmatter body  \n").unwrap(), "\n  No frontmatter body  \n");
    }
}
