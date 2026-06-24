//! Renders a structured document model into a professionally formatted Google Doc.
//!
//! The agent's LLM emits a `DocumentModel` (JSON) describing the document's logical
//! structure — title, headings, paragraphs, bullet lists, tables — NOT raw text. This
//! module translates that model into native Google Docs API `batchUpdate` requests so
//! the result uses real heading styles, spacing, and a consistent typeface rather than
//! a wall of plain text. The file is created directly inside the company's Shared Drive
//! (Option B), so the organization owns the artifact.
//!
//! Why a structured model instead of Markdown: Docs has no Markdown import that preserves
//! styling fidelity, and applying real named heading styles + table structure is the
//! difference between a polished deliverable and an obviously-AI text dump.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const DOCS_BASE: &str = "https://docs.googleapis.com/v1/documents";
const DRIVE_FILES: &str = "https://www.googleapis.com/drive/v3/files";

/// System instructions appended to an LLM node running in document-author mode. Teaches
/// the model to emit a `DocumentModel` as strict JSON instead of prose, so the Google Doc
/// node can render it natively. Kept next to the `DocumentModel`/`Block` types so the
/// schema and the prompt that targets it stay in lockstep — update both together.
///
/// The professional's persona/voice (and LoRA) still drive the *content*; this only
/// constrains the *structure and output format*.
pub const DOCUMENT_AUTHOR_INSTRUCTIONS: &str = r#"
## Output format: structured document JSON

You are producing a professional document that will be rendered into Google Docs with
native styling. Do NOT write Markdown or plain prose. Respond with a SINGLE JSON object
matching this exact schema — no commentary, no code fences, nothing before or after:

{
  "title": "string — the document title",
  "subtitle": "string or null — optional one-line subtitle/byline",
  "blocks": [
    { "type": "heading", "level": 1, "text": "Section heading" },
    { "type": "paragraph", "runs": [
        { "text": "Plain sentence. " },
        { "text": "Emphasised phrase.", "bold": true }
    ] },
    { "type": "bullet_list", "items": ["First point", "Second point"] },
    { "type": "numbered_list", "items": ["Step one", "Step two"] },
    { "type": "table", "rows": [["Header A", "Header B"], ["a1", "b1"]] }
  ]
}

Rules:
- `heading.level` is 1, 2, or 3 (document section hierarchy).
- A `paragraph` is an array of `runs`; each run is `{ "text": ... }` with optional
  `"bold": true` / `"italic": true`. Use runs to emphasise key terms, not whole paragraphs.
- The first row of a `table` is treated as a header row and rendered bold.
- Structure the document like a polished deliverable: clear title, logical sections with
  headings, concise paragraphs, lists where they aid scanability, tables for tabular data.
- Output ONLY the JSON object. It must be valid, parseable JSON.
"#;

/// Build the system prompt for a document-author LLM call: the professional's persona
/// (so the writing sounds like them) followed by the structural/format instructions.
pub fn document_author_system(persona: &str) -> String {
    format!("{persona}\n{DOCUMENT_AUTHOR_INSTRUCTIONS}")
}

/// The logical document the agent produces. Deserialized from the LLM's JSON output.
#[derive(Debug, Deserialize)]
pub struct DocumentModel {
    pub title: String,
    /// Optional subtitle / byline under the title.
    #[serde(default)]
    pub subtitle: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Section heading. `level` 1–3 maps to HEADING_1..HEADING_3.
    Heading { level: u8, text: String },
    /// Body paragraph. Inline `runs` carry styling (bold/italic).
    Paragraph { runs: Vec<TextRun> },
    /// Unordered bullet list.
    BulletList { items: Vec<String> },
    /// Ordered numbered list.
    NumberedList { items: Vec<String> },
    /// Simple table: first row is treated as a header row (bold).
    Table { rows: Vec<Vec<String>> },
}

#[derive(Debug, Deserialize)]
pub struct TextRun {
    pub text: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

/// Where the rendered doc should land and what to call it.
pub struct CreateTarget {
    /// Google Shared Drive id the company granted the agent access to.
    pub shared_drive_id: String,
    /// Optional folder within the shared drive; defaults to the drive root.
    pub parent_folder_id: Option<String>,
    /// File name in Drive (independent of the in-doc title).
    pub file_name: String,
}

pub struct CreatedDoc {
    pub document_id: String,
    pub url: String,
}

/// Create a formatted Google Doc from `model` inside the company's Shared Drive.
///
/// Flow:
///   1. Drive `files.create` — make an empty Doc in the target Shared Drive (org-owned).
///   2. Docs `batchUpdate` — insert the structured content with real styling.
pub async fn create_doc(
    access_token: &str,
    model: &DocumentModel,
    target: &CreateTarget,
) -> Result<CreatedDoc> {
    let http = reqwest::Client::new();

    // 1. Create the empty Doc directly in the Shared Drive so the org owns it.
    let mut parents = vec![target
        .parent_folder_id
        .clone()
        .unwrap_or_else(|| target.shared_drive_id.clone())];
    parents.retain(|p| !p.is_empty());

    let create_body = json!({
        "name": target.file_name,
        "mimeType": "application/vnd.google-apps.document",
        "parents": parents,
    });

    let created: Value = http
        .post(format!("{}?supportsAllDrives=true", DRIVE_FILES))
        .bearer_auth(access_token)
        .json(&create_body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow!("Drive files.create failed: {}", e))?
        .json()
        .await?;

    let document_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Drive files.create returned no id"))?
        .to_string();

    // 2 & 3. Fill the (empty) doc with styled body content + tables.
    fill_document(&http, access_token, &document_id, model).await?;

    Ok(CreatedDoc {
        url: format!("https://docs.google.com/document/d/{}/edit", document_id),
        document_id,
    })
}

/// Overwrite an existing Google Doc's content in place with `model` (used on a rerun when
/// the owner chooses "update existing"). Clears the current body, then refills it. The
/// document id, URL, sharing, and Drive location are preserved — only the content changes.
pub async fn overwrite_doc(
    access_token: &str,
    document_id: &str,
    model: &DocumentModel,
) -> Result<CreatedDoc> {
    let http = reqwest::Client::new();
    clear_document_body(&http, access_token, document_id).await?;
    fill_document(&http, access_token, document_id, model).await?;
    Ok(CreatedDoc {
        url: format!("https://docs.google.com/document/d/{}/edit", document_id),
        document_id: document_id.to_string(),
    })
}

/// Insert the structured model into an empty document (body styling pass, then tables).
async fn fill_document(
    http: &reqwest::Client,
    access_token: &str,
    document_id: &str,
    model: &DocumentModel,
) -> Result<()> {
    // Body styling pass. Tables are handled separately because their cell indices are
    // only known after Docs has materialized the table structure.
    let body = build_requests(model);
    if !body.is_empty() {
        http.post(format!("{}/{}:batchUpdate", DOCS_BASE, document_id))
            .bearer_auth(access_token)
            .json(&json!({ "requests": body }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| anyhow!("Docs batchUpdate (body) failed: {}", e))?;
    }

    let tables: Vec<&Vec<Vec<String>>> = model
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table { rows } => Some(rows),
            _ => None,
        })
        .collect();

    for rows in tables {
        insert_filled_table(http, access_token, document_id, rows).await?;
    }
    Ok(())
}

/// Delete the entire body content of a document, leaving an empty doc ready to refill.
/// The Docs body always ends with a trailing newline at the last index; we delete the
/// range [1, endIndex-1) which removes all content without hitting the segment-end error.
async fn clear_document_body(
    http: &reqwest::Client,
    access_token: &str,
    document_id: &str,
) -> Result<()> {
    let doc: Value = http
        .get(format!("{}/{}", DOCS_BASE, document_id))
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // The last structural element's endIndex is the document end.
    let end_index = doc
        .pointer("/body/content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.last())
        .and_then(|el| el.get("endIndex"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1);

    // Nothing to delete if the doc is already empty (only the trailing newline).
    if end_index <= 2 {
        return Ok(());
    }

    http.post(format!("{}/{}:batchUpdate", DOCS_BASE, document_id))
        .bearer_auth(access_token)
        .json(&json!({
            "requests": [{
                "deleteContentRange": {
                    "range": { "startIndex": 1, "endIndex": end_index - 1 }
                }
            }]
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow!("Docs deleteContentRange failed: {}", e))?;
    Ok(())
}

/// Translate the document model into ordered Docs API requests.
///
/// We build the document top-down, always inserting at index 1 of a running cursor.
/// Because each insert shifts following content, we instead accumulate the full text
/// once with recorded style ranges, then emit: one big insertText, followed by style
/// updates addressed by absolute index. This avoids fragile incremental-index math.
fn build_requests(model: &DocumentModel) -> Vec<Value> {
    let mut b = DocBuilder::new();

    // Title — TITLE named style, then subtitle if present.
    b.line(&model.title, Some("TITLE"), &[]);
    if let Some(sub) = &model.subtitle {
        b.line(sub, Some("SUBTITLE"), &[]);
    }

    for block in &model.blocks {
        match block {
            Block::Heading { level, text } => {
                let style = match level {
                    1 => "HEADING_1",
                    2 => "HEADING_2",
                    _ => "HEADING_3",
                };
                b.line(text, Some(style), &[]);
            }
            Block::Paragraph { runs } => {
                b.paragraph(runs);
            }
            Block::BulletList { items } => {
                b.list(items, false);
            }
            Block::NumberedList { items } => {
                b.list(items, true);
            }
            // Tables are inserted in a separate pass after the body (see create_doc),
            // so they're skipped here.
            Block::Table { .. } => {}
        }
    }

    b.into_requests()
}

/// Accumulates the document body as a single string plus the style/structure operations
/// that target absolute character ranges within it. Indices are tracked as we append.
struct DocBuilder {
    text: String,
    /// Style requests emitted after the initial insertText (named paragraph styles,
    /// bold/italic text runs, bullet ranges).
    styles: Vec<Value>,
    /// Bullet/number ranges to apply after text insertion: (start, end, ordered).
    bullets: Vec<(usize, usize, bool)>,
}

impl DocBuilder {
    fn new() -> Self {
        Self {
            text: String::new(),
            styles: Vec::new(),
            bullets: Vec::new(),
        }
    }

    /// Current insertion index. Docs body content begins at index 1.
    fn cursor(&self) -> usize {
        1 + self.text.chars().count()
    }

    /// Append one line with an optional named paragraph style.
    fn line(&mut self, text: &str, named_style: Option<&str>, runs: &[(usize, usize, bool, bool)]) {
        let start = self.cursor();
        self.text.push_str(text);
        self.text.push('\n');
        let end = self.cursor();

        if let Some(style) = named_style {
            self.styles.push(json!({
                "updateParagraphStyle": {
                    "range": { "startIndex": start, "endIndex": end },
                    "paragraphStyle": { "namedStyleType": style },
                    "fields": "namedStyleType"
                }
            }));
        }

        // Inline run styling, offsets relative to `start`.
        for (run_start, run_end, bold, italic) in runs {
            self.styles.push(json!({
                "updateTextStyle": {
                    "range": { "startIndex": start + run_start, "endIndex": start + run_end },
                    "textStyle": { "bold": bold, "italic": italic },
                    "fields": "bold,italic"
                }
            }));
        }
    }

    /// A body paragraph composed of inline runs with mixed styling.
    fn paragraph(&mut self, runs: &[TextRun]) {
        let mut combined = String::new();
        let mut run_ranges: Vec<(usize, usize, bool, bool)> = Vec::new();
        for run in runs {
            let s = combined.chars().count();
            combined.push_str(&run.text);
            let e = combined.chars().count();
            if run.bold || run.italic {
                run_ranges.push((s, e, run.bold, run.italic));
            }
        }
        self.line(&combined, Some("NORMAL_TEXT"), &run_ranges);
    }

    /// A bullet or numbered list; each item is its own paragraph, bulleted as a range.
    fn list(&mut self, items: &[String], ordered: bool) {
        if items.is_empty() {
            return;
        }
        let start = self.cursor();
        for item in items {
            self.line(item, Some("NORMAL_TEXT"), &[]);
        }
        let end = self.cursor();
        self.bullets.push((start, end, ordered));
    }

    /// Emit the ordered request list: insertText, then paragraph/text styles, then
    /// bullet ranges.
    fn into_requests(self) -> Vec<Value> {
        let mut requests = Vec::new();

        if !self.text.is_empty() {
            requests.push(json!({
                "insertText": { "location": { "index": 1 }, "text": self.text }
            }));
        }

        requests.extend(self.styles);

        for (start, end, ordered) in &self.bullets {
            let preset = if *ordered {
                "NUMBERED_DECIMAL_ALPHA_ROMAN"
            } else {
                "BULLET_DISC_CIRCLE_SQUARE"
            };
            requests.push(json!({
                "createParagraphBullets": {
                    "range": { "startIndex": start, "endIndex": end },
                    "bulletPreset": preset
                }
            }));
        }

        requests
    }
}

/// Insert a table at the end of the document and fill its cells.
///
/// Docs assigns cell paragraph indices only once the table exists, so we:
///   1. insertTable (empty grid) at end of segment,
///   2. GET the document and locate the just-inserted table's cell start indices,
///   3. insert each cell's text back-to-front (descending index) so each insert
///      doesn't invalidate the indices of cells we haven't filled yet,
///   4. bold the header row.
async fn insert_filled_table(
    http: &reqwest::Client,
    access_token: &str,
    document_id: &str,
    rows: &[Vec<String>],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let row_count = rows.len();
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return Ok(());
    }

    // 1. Insert the empty table at the end of the body.
    http.post(format!("{}/{}:batchUpdate", DOCS_BASE, document_id))
        .bearer_auth(access_token)
        .json(&json!({
            "requests": [{
                "insertTable": {
                    "endOfSegmentLocation": {},
                    "rows": row_count,
                    "columns": col_count
                }
            }]
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow!("Docs insertTable failed: {}", e))?;

    // 2. Fetch the doc and find the last table's cell start indices.
    let doc: Value = http
        .get(format!("{}/{}", DOCS_BASE, document_id))
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let cells = locate_last_table_cells(&doc)
        .ok_or_else(|| anyhow!("Could not locate inserted table in document"))?;

    // 3. Build cell-text inserts. Apply descending by index so earlier inserts don't
    //    shift the indices of cells we process later.
    let mut fills: Vec<(usize, String, bool)> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            if let Some(idx) = cells.get(&(r, c)) {
                let is_header = r == 0;
                fills.push((*idx, text.clone(), is_header));
            }
        }
    }
    fills.sort_by(|a, b| b.0.cmp(&a.0));

    let mut requests = Vec::new();
    for (idx, text, is_header) in &fills {
        requests.push(json!({
            "insertText": { "location": { "index": idx }, "text": text }
        }));
        if *is_header {
            requests.push(json!({
                "updateTextStyle": {
                    "range": { "startIndex": idx, "endIndex": idx + text.chars().count() },
                    "textStyle": { "bold": true },
                    "fields": "bold"
                }
            }));
        }
    }

    if !requests.is_empty() {
        http.post(format!("{}/{}:batchUpdate", DOCS_BASE, document_id))
            .bearer_auth(access_token)
            .json(&json!({ "requests": requests }))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| anyhow!("Docs table fill failed: {}", e))?;
    }

    Ok(())
}

/// Walk the document body for the LAST table element and return a map of
/// (row, col) -> the index at which that cell's text should be inserted.
fn locate_last_table_cells(
    doc: &Value,
) -> Option<std::collections::HashMap<(usize, usize), usize>> {
    let content = doc.pointer("/body/content")?.as_array()?;
    // Find the last element that has a "table" field.
    let table = content.iter().rev().find_map(|el| el.get("table"))?;

    let mut map = std::collections::HashMap::new();
    let table_rows = table.get("tableRows")?.as_array()?;
    for (r, row) in table_rows.iter().enumerate() {
        let cells = row.get("tableCells")?.as_array()?;
        for (c, cell) in cells.iter().enumerate() {
            // Each cell's first paragraph has a startIndex; text inserts at that index.
            let idx = cell
                .pointer("/content/0/startIndex")
                .and_then(|v| v.as_u64())?;
            map.insert((r, c), idx as usize);
        }
    }
    Some(map)
}
