//! The scraped items and their exporters, a port of `ItemList` in `scrapling/spiders/result.py`.

use std::path::Path;

use serde_json::Value;

use crate::error::{Error, Result};

/// The scraped items, with the exporters Python's `ItemList` has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Items(Vec<Value>);

impl Items {
    /// An empty list.
    pub fn new() -> Items {
        Items(Vec::new())
    }

    /// Append an item.
    pub fn push(&mut self, item: Value) {
        self.0.push(item);
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no items.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over the items.
    pub fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.0.iter()
    }

    /// Drop every item.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Write a JSON array; `indent` pretty-prints it.
    pub fn to_json(&self, path: impl AsRef<Path>, indent: bool) -> Result<()> {
        let path = path.as_ref();
        create_parent(path)?;
        let serialized = if indent {
            serde_json::to_vec_pretty(&self.0)?
        } else {
            serde_json::to_vec(&self.0)?
        };
        std::fs::write(path, serialized)?;
        tracing::info!(items = self.0.len(), path = %path.display(), "saved items");
        Ok(())
    }

    /// Write one JSON object per line.
    pub fn to_jsonl(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        create_parent(path)?;
        let mut out: Vec<u8> = Vec::new();
        for item in &self.0 {
            out.extend_from_slice(&serde_json::to_vec(item)?);
            out.push(b'\n');
        }
        std::fs::write(path, out)?;
        tracing::info!(items = self.0.len(), path = %path.display(), "saved items");
        Ok(())
    }

    /// Write a CSV; the columns default to every key seen, in first-seen order, and
    /// non-scalar values are written as JSON.
    pub fn to_csv(&self, path: impl AsRef<Path>, fields: Option<&[&str]>) -> Result<()> {
        let path = path.as_ref();
        create_parent(path)?;

        let columns: Vec<String> = match fields {
            Some(fields) => fields.iter().map(|field| field.to_string()).collect(),
            None => {
                let mut columns: Vec<String> = Vec::new();
                for item in &self.0 {
                    if let Value::Object(map) = item {
                        for key in map.keys() {
                            if !columns.iter().any(|column| column == key) {
                                columns.push(key.clone());
                            }
                        }
                    }
                }
                columns
            }
        };

        let mut writer = csv::Writer::from_path(path)
            .map_err(|error| Error::Other(format!("could not open {}: {error}", path.display())))?;
        writer
            .write_record(&columns)
            .map_err(|error| Error::Other(format!("could not write the CSV header: {error}")))?;

        for item in &self.0 {
            let row: Vec<String> = columns
                .iter()
                .map(|column| match item {
                    Value::Object(map) => stringify(map.get(column)),
                    _ => String::new(),
                })
                .collect();
            writer
                .write_record(&row)
                .map_err(|error| Error::Other(format!("could not write a CSV row: {error}")))?;
        }
        writer
            .flush()
            .map_err(|error| Error::Other(format!("could not flush the CSV: {error}")))?;

        tracing::info!(items = self.0.len(), path = %path.display(), "saved items");
        Ok(())
    }

    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<Value> {
        self.0
    }
}

/// Turn one value into a CSV cell: scalars as themselves, containers as JSON.
fn stringify(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

impl std::ops::Deref for Items {
    type Target = [Value];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for Items {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Items {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<Value> for Items {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Items(iter.into_iter().collect())
    }
}

impl From<Vec<Value>> for Items {
    fn from(items: Vec<Value>) -> Self {
        Items(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Items {
        let mut items = Items::new();
        items.push(json!({"name": "one", "price": 5, "tags": ["a", "b"]}));
        items.push(json!({"name": "two", "extra": null, "price": 7.5}));
        items
    }

    #[test]
    fn exports_json_and_jsonl() {
        let dir = tempfile::tempdir().expect("temp dir");
        let items = sample();

        let json_path = dir.path().join("nested/items.json");
        items.to_json(&json_path, true).expect("json export");
        let written = std::fs::read_to_string(&json_path).expect("read back");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&written).expect("valid json");
        assert_eq!(parsed.len(), 2);

        let jsonl_path = dir.path().join("items.jsonl");
        items.to_jsonl(&jsonl_path).expect("jsonl export");
        let written = std::fs::read_to_string(&jsonl_path).expect("read back");
        assert_eq!(written.lines().count(), 2);
    }

    #[test]
    fn csv_flattens_containers_and_unions_the_columns() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("items.csv");
        sample().to_csv(&path, None).expect("csv export");

        let written = std::fs::read_to_string(&path).expect("read back");
        let mut lines = written.lines();
        let mut header: Vec<&str> = lines.next().expect("a header").split(',').collect();
        header.sort_unstable();
        assert_eq!(header, vec!["extra", "name", "price", "tags"]);
        assert_eq!(lines.count(), 2);

        // The nested list is written as JSON and the missing key as an empty cell.
        assert!(written.contains(r#""[""a"",""b""]""#));
        assert!(written.contains("7.5"));
    }

    #[test]
    fn csv_honours_an_explicit_column_list() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("items.csv");
        sample()
            .to_csv(&path, Some(&["price", "name"]))
            .expect("csv export");

        let written = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(written.lines().next(), Some("price,name"));
        assert_eq!(written.lines().nth(1), Some("5,one"));
    }

    #[test]
    fn behaves_like_a_slice() {
        let items = sample();
        assert_eq!(items.len(), 2);
        assert!(!items.is_empty());
        assert_eq!(
            items.first().and_then(|item| item.get("name")),
            Some(&json!("one"))
        );
        assert_eq!(items.iter().count(), 2);
        assert_eq!(items.into_vec().len(), 2);
    }
}
