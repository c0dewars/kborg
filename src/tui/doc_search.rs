//! `/` search in the read-only document views: describe, YAML, JSON, metrics
//! text, Helm documents and Copilot answers. The search runs over the lines
//! as they are drawn (after wrapping), ignores ASCII case, jumps with `n` and
//! `N`, and highlights every match on screen.
use super::*;

/// Longest search text.
const MAX_QUERY: usize = 100;

#[derive(Default)]
pub(super) struct DocSearch {
    /// The applied search, lowercase; empty when none.
    query: String,
    /// The text being typed after `/`.
    draft: Option<String>,
}

/// The text of a searchable document view, as its renderer wraps it.
pub(super) fn document_text<'a>(app: &App, modal: &'a Modal) -> Option<std::borrow::Cow<'a, str>> {
    use std::borrow::Cow;
    match modal {
        Modal::Metrics(..) if app.metrics_chart => None,
        Modal::Detail(text, _)
        | Modal::Metrics(text, _)
        | Modal::HelmDocument(_, text, _)
        | Modal::AdviceDocument(_, text, _) => Some(Cow::Borrowed(text)),
        Modal::Overview(object, _) => Some(Cow::Owned(overview_document(app, object))),
        _ => None,
    }
}

/// The object overview with its measured usage, when the table has a sample.
pub(super) fn overview_document(app: &App, object: &Value) -> String {
    let usage = app
        .table_metrics
        .usage(object, Instant::now())
        .map(|usage| {
            format!(
                "\n\nMEASURED USAGE\nCPU {}m | Memory {}B ({})\nTimestamp {} | Window {}\nBound UID {} | {}",
                crate::metrics::format_millicores(usage.cpu_nanocores),
                usage.memory_bytes,
                crate::metrics::format_memory(usage.memory_bytes),
                usage.timestamp,
                usage.window,
                usage.identity_uid,
                if usage.identity_verified {
                    "Metrics API UID verified"
                } else {
                    "Metrics API UID absent; entire sample window checked against current object creation"
                }
            )
        })
        .unwrap_or_default();
    format!("{}{usage}", overview_text(object, &app.context))
}

fn offset_mut(modal: &mut Modal) -> Option<&mut usize> {
    match modal {
        Modal::Detail(_, offset)
        | Modal::Metrics(_, offset)
        | Modal::Overview(_, offset)
        | Modal::HelmDocument(_, _, offset)
        | Modal::AdviceDocument(_, _, offset) => Some(offset),
        _ => None,
    }
}

/// Byte ranges of `query` (lowercase) in `line`, ignoring ASCII case.
pub(super) fn matches(line: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    // ASCII lowercasing keeps byte offsets, so ranges index `line` itself.
    line.to_ascii_lowercase()
        .match_indices(query)
        .map(|(start, found)| (start, start + found.len()))
        .collect()
}

impl DocSearch {
    pub(super) fn query(&self) -> &str {
        &self.query
    }
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
    /// The hint row while a search is typed or applied. `lines` are the
    /// wrapped document lines and `offset` the top line shown.
    pub(super) fn hint(&self, lines: &[&str], offset: usize) -> Option<String> {
        if let Some(draft) = &self.draft {
            return Some(format!(
                "Search > {draft}_   Enter find | Esc cancel | ignores case"
            ));
        }
        if self.query.is_empty() {
            return None;
        }
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !matches(line, &self.query).is_empty())
            .map(|(index, _)| index)
            .collect();
        let current = hits.iter().position(|line| *line >= offset).unwrap_or(0) + 1;
        Some(format!(
            "/{}   line {current} of {} matching | n next | N previous | Esc clear",
            self.query,
            hits.len()
        ))
    }
}

impl App {
    /// Search keys of a document view. True when the key was used.
    pub(super) fn document_search_key(&mut self, code: KeyCode) -> bool {
        let (width, _) = terminal::size().unwrap_or((80, 24));
        let Some(wrapped) = document_text(self, &self.modal)
            .map(|text| wrap_text(&text, usize::from(width.saturating_sub(2))))
        else {
            self.doc_search.clear();
            return false;
        };
        let offset = offset_mut(&mut self.modal).map_or(0, |offset| *offset);
        let hits = |query: &str| -> Vec<usize> {
            wrapped
                .lines()
                .enumerate()
                .filter(|(_, line)| !matches(line, query).is_empty())
                .map(|(index, _)| index)
                .collect()
        };
        let target = if let Some(mut draft) = self.doc_search.draft.take() {
            match code {
                KeyCode::Esc => return true,
                KeyCode::Enter => {
                    let query = draft.trim().to_ascii_lowercase();
                    let hits = hits(&query);
                    if query.is_empty() {
                        self.doc_search.clear();
                        return true;
                    }
                    if hits.is_empty() {
                        self.status = format!(
                            "No match for {}.",
                            safety::safe_text(draft.trim(), MAX_QUERY)
                        );
                        self.doc_search.clear();
                        return true;
                    }
                    self.doc_search.query = query;
                    // The first match at or below the top line, else wrap.
                    hits.iter()
                        .find(|line| **line >= offset)
                        .or(hits.first())
                        .copied()
                }
                KeyCode::Backspace => {
                    draft.pop();
                    self.doc_search.draft = Some(draft);
                    return true;
                }
                KeyCode::Char(c) if !c.is_control() && draft.len() + c.len_utf8() <= MAX_QUERY => {
                    draft.push(c);
                    self.doc_search.draft = Some(draft);
                    return true;
                }
                _ => {
                    self.doc_search.draft = Some(draft);
                    return true;
                }
            }
        } else {
            let active = !self.doc_search.query.is_empty();
            match code {
                KeyCode::Char('/') => {
                    self.doc_search.draft = Some(String::new());
                    return true;
                }
                KeyCode::Esc if active => {
                    self.doc_search.clear();
                    return true;
                }
                KeyCode::Char('n') if active => {
                    let hits = hits(&self.doc_search.query);
                    hits.iter()
                        .find(|line| **line > offset)
                        .or(hits.first())
                        .copied()
                }
                KeyCode::Char('N') if active => {
                    let hits = hits(&self.doc_search.query);
                    hits.iter()
                        .rev()
                        .find(|line| **line < offset)
                        .or(hits.last())
                        .copied()
                }
                _ => return false,
            }
        };
        if let (Some(line), Some(offset)) = (target, offset_mut(&mut self.modal)) {
            *offset = line;
        }
        true
    }
}
