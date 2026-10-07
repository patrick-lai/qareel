use crate::failure::fixable;
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;

pub struct Spec {
    pub command: &'static str,
    pub positional: &'static [&'static str],
    pub strings: &'static [&'static str],
    pub values: &'static [&'static str],
    pub pointing: bool,
}

impl Spec {
    fn known(&self, key: &str) -> bool {
        self.positional.contains(&key) || self.strings.contains(&key) || self.values.contains(&key)
    }
}

pub fn looks_like_ref(value: &str) -> bool {
    let short = value.len() > 1 && (value.starts_with('e') || value.starts_with('n')) && value[1..].bytes().all(|byte| byte.is_ascii_digit());
    let native = !value.contains('/') && value.rsplit_once(':').is_some_and(|(token, index)| token.len() >= 8 && !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()));
    short || native
}

pub fn selectorize(value: &str) -> String {
    let typed = ["text/", "aria/", "xpath/", "pierce/", "css/"].iter().any(|prefix| value.starts_with(prefix));
    let syntax = value.chars().any(|character| "[]#.>:=*()/~+,".contains(character));
    let tag = !value.is_empty() && value.starts_with(|first: char| first.is_ascii_lowercase()) && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if typed || syntax || tag { value.to_owned() } else { format!("text/{value}") }
}

fn usage(spec: &Spec, detail: String) -> anyhow::Error {
    fixable("args.invalid", detail, format!("run `qareel {} --help` for its fields", spec.command))
}

pub fn parse(spec: &Spec, params: &[String]) -> Result<Map<String, Value>> {
    if let [single] = params
        && let Ok(Value::Object(object)) = serde_json::from_str::<Value>(single)
    {
        return Ok(object);
    }
    let mut positional: VecDeque<&'static str> = spec.positional.iter().copied().collect();
    let mut map = Map::new();
    let bare: Vec<&String> = params.iter().filter(|param| !param.contains('=') && !param.starts_with("--")).collect();
    let point = match bare.as_slice() {
        [x, y] if spec.pointing => x.trim().parse::<f64>().ok().zip(y.trim().parse::<f64>().ok()),
        _ => None,
    };
    if let Some((x, y)) = point {
        map.insert("x".to_owned(), json!(x));
        map.insert("y".to_owned(), json!(y));
        positional.clear();
    }
    for param in params {
        if point.is_some() && !param.contains('=') && !param.starts_with("--") {
            continue;
        }
        if let Some(flag) = param.strip_prefix("--").filter(|flag| !flag.contains('=')) {
            let key = flag.replace('-', "_");
            if !spec.known(&key) {
                return Err(usage(spec, format!("unknown flag --{flag}")));
            }
            if map.insert(key, Value::Bool(true)).is_some() {
                return Err(usage(spec, format!("--{flag} is given twice")));
            }
            continue;
        }
        let named = param
            .split_once('=')
            .map(|(key, raw)| (key.trim_start_matches("--").replace('-', "_"), raw.to_owned()))
            .filter(|(key, _)| spec.known(key) || (positional.is_empty() && !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')));
        let (key, raw) = match named {
            Some(pair) => pair,
            None => {
                let field = positional.pop_front().ok_or_else(|| usage(spec, format!("expected key=value, got `{param}`")))?;
                if field == "ref" && !looks_like_ref(param) { ("selector".to_owned(), selectorize(param)) } else { (field.to_owned(), param.clone()) }
            }
        };
        if key.is_empty() || map.contains_key(&key) {
            return Err(usage(spec, format!("`{key}` is given twice")));
        }
        if matches!(key.as_str(), "ref" | "selector") {
            positional.retain(|field| *field != "ref");
        }
        let parsed = serde_json::from_str::<Value>(&raw).ok().filter(|value| !value.is_string() || raw.starts_with('"'));
        let value = match parsed {
            Some(Value::Number(_) | Value::Bool(_)) if spec.strings.contains(&key.as_str()) => Value::String(raw),
            Some(value) => value,
            None => Value::String(raw),
        };
        map.insert(key, value);
    }
    Ok(map)
}
