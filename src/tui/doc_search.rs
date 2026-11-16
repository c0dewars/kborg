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
    // The view's key hint goes last, after the sections added below.
    let full = overview_text(object, &app.context);
    let overview: Vec<&str> = full
        .lines()
        .filter(|line| !line.starts_with("J JSON"))
        .collect();
    let uid = object["metadata"]["uid"].as_str().unwrap_or_default();
    let events = app
        .overview_events
        .as_ref()
        .filter(|(events_uid, _)| events_uid == uid)
        .map(|(_, events)| events.as_deref().map_err(String::as_str));
    let text = crate::describe::render(
        &format!("{}{usage}", overview.join("\n").trim_end()),
        object,
        events,
    );
    format!("{text}\n\nJ JSON   y YAML   Esc close")
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
    /// True while the search text is being typed.
    pub(super) fn typing(&self) -> bool {
        self.draft.is_some()
    }
    pub(super) fn draft(&self) -> Option<&str> {
        self.draft.as_deref()
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
        if hits.is_empty() {
            return Some(format!("No match for /{} | Esc clear", self.query));
        }
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
        let (width, height) = terminal::size().unwrap_or((80, 24));
        if !self.doc_search.typing()
            && !matches!(code, KeyCode::Char('/'))
            && (self.doc_search.query().is_empty()
                || !matches!(code, KeyCode::Esc | KeyCode::Char('n' | 'N')))
        {
            if !self.doc_search.query().is_empty()
                && !matches!(self.modal, Modal::AdviceChat(..))
                && document_text(self, &self.modal).is_none()
            {
                self.doc_search.clear();
            }
            return false;
        }
        let (wrapped, chat_offset) = match (&self.modal, &self.ask_thread) {
            (Modal::AdviceChat(scroll), Some(thread)) => {
                let (lines, offset) = advice_chat::search_document(
                    thread,
                    scroll,
                    width,
                    height,
                    self.view_top(width, height),
                );
                (lines, Some(offset))
            }
            _ => {
                let Some(text) = document_text(self, &self.modal) else {
                    self.doc_search.clear();
                    return false;
                };
                (wrap_text(&text, usize::from(width.saturating_sub(2))), None)
            }
        };
        let offset =
            chat_offset.unwrap_or_else(|| offset_mut(&mut self.modal).map_or(0, |offset| *offset));
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
                        if matches!(self.modal, Modal::AdviceChat(..)) {
                            self.ask_notice = Some(self.status.clone());
                        }
                        self.doc_search.clear();
                        return true;
                    }
                    self.doc_search.query = query;
                    if matches!(self.modal, Modal::AdviceChat(..)) {
                        self.ask_notice = None;
                    }
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
                    if matches!(self.modal, Modal::AdviceChat(..)) {
                        self.ask_notice = None;
                    }
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
        if let Some(line) = target {
            if let Modal::AdviceChat(scroll) = &mut self.modal {
                scroll.jump_to_match(line);
            } else if let Some(offset) = offset_mut(&mut self.modal) {
                *offset = line;
            }
        }
        true
    }
}
