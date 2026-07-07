use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

pub fn load<T>(path: &Path, kind: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create Pkl evaluation runtime")?;

    runtime
        .block_on(pklx::eval_to_typed(
            path,
            pklx::pklr::EvalOptions::default(),
        ))
        .map_err(|e| anyhow::anyhow!("failed to evaluate {kind} Pkl {}: {}", path.display(), e))
}

pub fn to_pkl<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    let value = serde_json::to_value(value).context("failed to convert value to JSON")?;
    match value {
        Value::Object(values) => object_to_pkl_module(&values),
        value => Ok(format!("{}\n", json_to_pkl(&value, 0)?)),
    }
}

fn object_to_pkl_module(values: &serde_json::Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    for (key, value) in values {
        anyhow::ensure!(
            is_pkl_identifier(key),
            "cannot render unsupported Pkl property name '{}'",
            key
        );
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(&json_to_pkl(value, 0)?);
        out.push('\n');
    }
    Ok(out)
}

fn json_to_pkl(value: &Value, indent: usize) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(pklx::pkl_string_literal(value)),
        Value::Array(values) => {
            if values.is_empty() {
                return Ok("new Listing {}".to_string());
            }

            let child_indent = indent + 2;
            let mut out = "new Listing {\n".to_string();
            for value in values {
                out.push_str(&" ".repeat(child_indent));
                out.push_str(&json_to_pkl(value, child_indent)?);
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
            Ok(out)
        }
        Value::Object(values) => {
            if values.is_empty() {
                return Ok("new {}".to_string());
            }

            let child_indent = indent + 2;
            let mut out = "new {\n".to_string();
            for (key, value) in values {
                anyhow::ensure!(
                    is_pkl_identifier(key),
                    "cannot render unsupported Pkl property name '{}'",
                    key
                );
                out.push_str(&" ".repeat(child_indent));
                out.push_str(key);
                out.push_str(" = ");
                out.push_str(&json_to_pkl(value, child_indent)?);
                out.push('\n');
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
            Ok(out)
        }
    }
}

fn is_pkl_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_lists_and_objects_as_pkl() {
        let value = serde_json::json!({
            "name": "fan",
            "curve": [
                {"temp": 45, "pwm": 20},
                {"temp": 85, "pwm": 200}
            ]
        });

        let pkl = json_to_pkl(&value, 0).unwrap();
        assert!(pkl.contains("new Listing"));
        assert!(pkl.contains("name = \"fan\""));
        assert!(pkl.contains("temp = 45"));
    }

    #[test]
    fn serializes_root_object_as_module_properties() {
        let value = serde_json::json!({"poll_interval_ms": 2000});
        let pkl = to_pkl(&value).unwrap();
        assert_eq!(pkl, "poll_interval_ms = 2000\n");
    }

    #[test]
    fn rejects_non_identifier_keys() {
        let value = serde_json::json!({"bad-key": true});
        let err = json_to_pkl(&value, 0).unwrap_err().to_string();
        assert!(err.contains("unsupported Pkl property name"));
    }
}
