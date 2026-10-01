//! A bounded, lazy JSON tree. Only expanded branches become rows.
use std::collections::HashSet;
use serde_json::Value;
use crate::JsonRow;

#[derive(Debug)]
pub struct Tree {
    value: Value,
    open: HashSet<String>,
    pub truncated: bool,
}

impl Tree {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        Ok(Self { value: serde_json::from_str(text)?, open: HashSet::from([String::new()]), truncated: false })
    }
    pub fn toggle(&mut self, path: &str) {
        if !self.open.remove(path) { self.open.insert(path.into()); }
    }
    pub fn copy_value(&self, path: &str) -> Option<String> {
        let value = self.value.pointer(path)?;
        Some(match value { Value::String(s) => s.clone(), _ => serde_json::to_string_pretty(value).ok()? })
    }
    pub fn rows(&mut self) -> Vec<JsonRow> {
        let mut rows = Vec::new();
        self.truncated = false;
        visit(&self.value, "", "$", 0, &self.open, &mut rows, &mut self.truncated);
        rows
    }
}

fn visit(value: &Value, path: &str, key: &str, depth: i32, open: &HashSet<String>, rows: &mut Vec<JsonRow>, truncated: &mut bool) {
    if rows.len() >= 2000 { *truncated = true; return; }
    let container = value.is_object() || value.is_array();
    let expanded = open.contains(path);
    let text = match value {
        Value::Object(map) => format!("{{ {} }}", map.len()),
        Value::Array(array) => format!("[ {} ]", array.len()),
        _ => {
            let text = value.to_string();
            let mut short: String = text.chars().take(300).collect();
            if short.len() < text.len() { short.push('…'); }
            short
        }
    };
    rows.push(JsonRow { path: path.into(), key: key.into(), value: text.into(), depth, expandable: container, expanded });
    if !expanded { return; }
    let pointer = |key: &str| format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
    match value {
        Value::Object(map) => for (key, value) in map {
            if rows.len() >= 2000 { *truncated = true; break; }
            visit(value, &pointer(key), key, depth + 1, open, rows, truncated);
        },
        Value::Array(array) => for (i, value) in array.iter().enumerate() {
            if rows.len() >= 2000 { *truncated = true; break; }
            visit(value, &pointer(&i.to_string()), &i.to_string(), depth + 1, open, rows, truncated);
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_and_copies_exact_values_with_escaped_keys() {
        let mut tree = Tree::parse(r#"{"a/b":{"~key":["hello",42]},"ok":true}"#).unwrap();
        assert_eq!(tree.rows().len(), 3);
        tree.toggle("/a~1b");
        tree.toggle("/a~1b/~0key");
        assert_eq!(tree.copy_value("/a~1b/~0key/0").as_deref(), Some("hello"));
        assert_eq!(tree.copy_value("/a~1b/~0key/1").as_deref(), Some("42"));
        assert_eq!(tree.rows().len(), 6);
        tree.toggle("/a~1b");
        assert_eq!(tree.rows().len(), 3);
        assert!(Tree::parse("{\n invalid}").unwrap_err().line() == 2);
        let exact = Tree::parse("{\"id\":123456789012345678901234567890}").unwrap();
        assert_eq!(exact.copy_value("/id").unwrap(), "123456789012345678901234567890");
        let mut large = Tree::parse(&format!("[{}]", vec!["0"; 3000].join(","))).unwrap();
        assert_eq!(large.rows().len(), 2000);
        assert!(large.truncated);
        large.toggle("");
        assert_eq!(large.rows().len(), 1);
        assert!(!large.truncated);
    }
}
