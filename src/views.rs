//! k9s-compatible data-only `views.yaml`: per-resource table columns and
//! default sort. Custom columns read a JSONPath-like field path; nothing is
//! evaluated as code.
use crate::{Result, safety};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_VIEWS: usize = 128;
const MAX_COLUMNS: usize = 24;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    views: BTreeMap<String, Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    sort_column: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Field(String),
    Index(usize),
    All,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub title: String,
    /// `None` for a built-in column named by `title`.
    pub path: Option<Vec<Segment>>,
    /// `|W`: shown only in wide mode (Ctrl-W).
    pub wide: bool,
    /// `|H`: hidden.
    pub hidden: bool,
    /// `|N`: sort numerically.
    pub numeric: bool,
    /// `|T`: the value is a timestamp shown as an age.
    pub time: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub columns: Vec<ColumnSpec>,
    /// Default sort: column title and descending flag.
    pub sort: Option<(String, bool)>,
}
pub type Views = BTreeMap<String, View>;

fn parse_path(text: &str) -> Result<Vec<Segment>> {
    let invalid = || format!("invalid column path {:?}", safety::safe_text(text, 120));
    let mut chars = text
        .strip_prefix('.')
        .ok_or_else(invalid)?
        .chars()
        .peekable();
    let mut segments = Vec::new();
    let mut field = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => field.push(chars.next().ok_or_else(invalid)?),
            '.' => {
                if !field.is_empty() {
                    segments.push(Segment::Field(std::mem::take(&mut field)));
                }
            }
            '[' => {
                if !field.is_empty() {
                    segments.push(Segment::Field(std::mem::take(&mut field)));
                }
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                let inner = inner.trim();
                segments.push(if inner == "*" {
                    Segment::All
                } else if let Some(quoted) = inner
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .or_else(|| inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                {
                    Segment::Field(quoted.into())
                } else {
                    Segment::Index(inner.parse().map_err(|_| invalid())?)
                });
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() {
        segments.push(Segment::Field(field));
    }
    if segments.is_empty() || segments.len() > 16 {
        return Err(invalid());
    }
    Ok(segments)
}

/// Values at `path`; `[*]` fans out over arrays.
pub fn eval(path: &[Segment], value: &Value) -> Vec<String> {
    let mut current = vec![value];
    for segment in path {
        current = current
            .into_iter()
            .flat_map(|value| match segment {
                Segment::Field(name) => value.get(name).into_iter().collect::<Vec<_>>(),
                Segment::Index(index) => value.get(*index).into_iter().collect(),
                Segment::All => value
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default(),
            })
            .collect();
    }
    current
        .into_iter()
        .filter(|value| !value.is_null())
        .map(|value| match value {
            Value::String(text) => text.clone(),
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                    )
                })
                .collect::<Vec<_>>()
                .join(","),
            other => other.to_string(),
        })
        .collect()
}

fn parse_column(text: &str) -> Result<ColumnSpec> {
    let (main, attributes) = text.split_once('|').unwrap_or((text, ""));
    let (title, path) = match main.split_once(':') {
        Some((title, path)) => (title.trim(), Some(parse_path(path.trim())?)),
        None => (main.trim(), None),
    };
    if title.is_empty()
        || title.len() > 32
        || !title
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_%()/".contains(&b))
    {
        return Err(format!(
            "column titles are 1..32 letters, digits or -_%()/: {:?}",
            safety::safe_text(title, 40)
        ));
    }
    let attributes = attributes.to_ascii_uppercase();
    Ok(ColumnSpec {
        title: title.to_ascii_uppercase(),
        path,
        wide: attributes.contains('W'),
        hidden: attributes.contains('H'),
        numeric: attributes.contains('N'),
        time: attributes.contains('T'),
    })
}

fn parse_sort(text: &str) -> Result<(String, bool)> {
    let (title, order) = text.split_once(':').unwrap_or((text, "asc"));
    let descending = match order.trim().to_ascii_lowercase().as_str() {
        "asc" => false,
        "desc" => true,
        _ => return Err("sortColumn order must be asc or desc".into()),
    };
    Ok((title.trim().to_ascii_uppercase(), descending))
}

pub fn parse(text: &str) -> Result<Views> {
    if text.len() > 64 * 1024 {
        return Err("views.yaml exceeds 64 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(64*1024), max_events: 20_000, max_aliases: 0, max_anchors: 0,
            max_depth: 5, max_inclusion_depth: 0, max_documents: 1, max_nodes: 8192,
            max_total_scalar_bytes: 64*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: File = serde_saphyr::from_str_with_options(text, options).map_err(|error| {
        format!(
            "invalid views.yaml{}; expected views: RESOURCE: {{columns: [...], sortColumn: NAME:asc}}",
            safety::yaml_location(&error)
        )
    })?;
    if file.views.len() > MAX_VIEWS {
        return Err(format!("views.yaml exceeds {MAX_VIEWS} views"));
    }
    let mut views = Views::new();
    for (resource, entry) in file.views {
        if entry.columns.len() > MAX_COLUMNS {
            return Err(format!("a view has more than {MAX_COLUMNS} columns"));
        }
        let view = View {
            columns: entry
                .columns
                .iter()
                .map(|column| parse_column(column))
                .collect::<Result<_>>()?,
            sort: entry.sort_column.as_deref().map(parse_sort).transpose()?,
        };
        views.insert(resource.trim().to_ascii_lowercase(), view);
    }
    Ok(views)
}

pub fn load() -> Result<Views> {
    load_from(&safety::config_dir()?.join("views.yaml"))
}

/// Load from an explicit path (live reload watches the config folder).
pub fn load_from(path: &std::path::Path) -> Result<Views> {
    safety::checked_path(path)?;
    if !path.exists() {
        return Ok(Views::new());
    }
    let bytes = safety::read_small(path, 64 * 1024)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "views.yaml must be UTF-8")?)
}

/// The view for a resource under any of its k9s names: `v1/pods`,
/// `apps/v1/deployments`, `deployments.apps`, `deployments`.
pub fn find<'a>(views: &'a Views, group: &str, version: &str, plural: &str) -> Option<&'a View> {
    let qualified = if group.is_empty() {
        format!("{version}/{plural}")
    } else {
        format!("{group}/{version}/{plural}")
    };
    let dotted = if group.is_empty() {
        plural.to_owned()
    } else {
        format!("{plural}.{group}")
    };
    [qualified, dotted, plural.to_owned()]
        .iter()
        .find_map(|key| views.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn k9s_views_parse_columns_paths_attributes_and_sort() -> Result<()> {
        let views = parse(
            "views:\n  v1/pods:\n    sortColumn: AGE:desc\n    columns:\n      - NAMESPACE\n      - NAME\n      - STATUS\n      - IMAGE:.spec.containers[0].image\n      - APP:.metadata.labels['app.kubernetes.io/name']|W\n      - PORTS:.spec.containers[*].ports[*].containerPort|N\n      - AGE\n",
        )?;
        let view = find(&views, "", "v1", "pods").ok_or("view")?;
        assert_eq!(view.sort, Some(("AGE".into(), true)));
        assert_eq!(view.columns.len(), 7);
        assert!(view.columns[4].wide && view.columns[5].numeric);
        let pod = json!({"metadata":{"labels":{"app.kubernetes.io/name":"api"}},"spec":{"containers":[{"image":"api:1","ports":[{"containerPort":80},{"containerPort":9090}]},{"image":"side:2"}]}});
        assert_eq!(
            eval(view.columns[3].path.as_deref().ok_or("path")?, &pod),
            vec!["api:1"]
        );
        assert_eq!(
            eval(view.columns[4].path.as_deref().ok_or("path")?, &pod),
            vec!["api"]
        );
        assert_eq!(
            eval(view.columns[5].path.as_deref().ok_or("path")?, &pod),
            vec!["80", "9090"]
        );
        assert!(find(&views, "apps", "v1", "deployments").is_none());
        for bad in [
            "views:\n  pods:\n    columns: [\"BAD TITLE\"]\n",
            "views:\n  pods:\n    columns: [\"X:spec\"]\n",
            "views:\n  pods:\n    sortColumn: AGE:sideways\n",
            "views:\n  pods:\n    columns: [NAME]\n    script: rm\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        Ok(())
    }
}
