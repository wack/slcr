//! How `slcr query` writes the rows it finds: an aligned table, TSV, or
//! JSON.
//!
//! In a table or TSV, a string is written as it is and null as nothing.
//! A line break or tab in a cell would break the layout, so it is escaped as
//! `\n`, `\r`, or `\t`; TSV also escapes a backslash as `\\`, so that every
//! cell can be read back exactly.

use console::measure_text_width;
use serde::Serialize;

use crate::query::Rows;

/// The rows as aligned columns under a header and a rule, then the number
/// of rows, e.g.
///
/// ```text
/// id       modality
/// -------  --------
/// REQ-001  MUST
/// (1 row)
/// ```
pub(super) fn table(rows: &Rows) -> String {
    let header: Vec<String> = rows.columns.iter().map(|c| escape(c, false)).collect();
    let body: Vec<Vec<String>> = rows
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| escape(&cell.text(), false)).collect())
        .collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            body.iter()
                .map(|row| measure_text_width(&row[i]))
                .chain([measure_text_width(&header[i])])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, &width)| {
                let padding = width - measure_text_width(cell);
                format!("{cell}{}", " ".repeat(padding))
            })
            .collect();
        let mut line = padded.join("  ").trim_end().to_owned();
        line.push('\n');
        line
    };
    let rule: Vec<String> = widths.iter().map(|&width| "-".repeat(width)).collect();

    let mut table = line(&header);
    table.push_str(&line(&rule));
    for row in &body {
        table.push_str(&line(row));
    }
    let count = match body.len() {
        1 => "(1 row)\n".to_owned(),
        n => format!("({n} rows)\n"),
    };
    table.push_str(&count);
    table
}

/// The rows as tab-separated values: a header line, then one line per row.
pub(super) fn tsv(rows: &Rows) -> String {
    let line = |cells: Vec<String>| {
        let mut line = cells.join("\t");
        line.push('\n');
        line
    };
    let mut tsv = line(rows.columns.iter().map(|c| escape(c, true)).collect());
    for row in &rows.rows {
        tsv.push_str(&line(
            row.iter().map(|cell| escape(&cell.text(), true)).collect(),
        ));
    }
    tsv
}

/// The rows as one JSON document:
///
/// ```json
/// {
///   "columns": ["r", "e"],
///   "rows": [
///     ["REQ-004", {"label": "REFINES", "source": "REQ-004", "target": "REQ-002"}]
///   ]
/// }
/// ```
///
/// Nodes are their IDs, edges are objects naming their label and endpoints,
/// and a float that isn't finite is `null`.
pub(super) fn json(rows: &Rows) -> String {
    #[derive(Serialize)]
    struct Document<'a> {
        columns: &'a [String],
        rows: &'a [Vec<crate::query::Cell>],
    }

    let document = Document {
        columns: &rows.columns,
        rows: &rows.rows,
    };
    let mut json =
        serde_json::to_string_pretty(&document).expect("query results serialize to JSON");
    json.push('\n');
    json
}

/// `text`, with line breaks and tabs escaped, and backslashes too if
/// `backslash`.
fn escape(text: &str, backslash: bool) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\\' if backslash => escaped.push_str(r"\\"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Cell;

    fn rows() -> Rows {
        Rows {
            columns: vec!["id".to_owned(), "body".to_owned(), "n".to_owned()],
            rows: vec![
                vec![
                    Cell::Node("REQ-001".to_owned()),
                    Cell::String("Two\tlines\nwith a \\".to_owned()),
                    Cell::Int(1),
                ],
                vec![
                    Cell::Node("REQ-1000".to_owned()),
                    Cell::String("Héllo".to_owned()),
                    Cell::Null,
                ],
            ],
        }
    }

    #[test]
    fn a_table_aligns_its_columns_and_counts_its_rows() {
        assert_eq!(
            table(&rows()),
            "\
id        body                  n
--------  --------------------  -
REQ-001   Two\\tlines\\nwith a \\  1
REQ-1000  Héllo
(2 rows)
"
        );
    }

    #[test]
    fn a_table_counts_one_row_and_none() {
        let mut one = rows();
        one.rows.truncate(1);
        assert!(table(&one).ends_with("\n(1 row)\n"));
        let mut none = rows();
        none.rows.clear();
        assert_eq!(table(&none), "id  body  n\n--  ----  -\n(0 rows)\n");
    }

    #[test]
    fn tsv_escapes_what_would_break_a_line_or_a_column() {
        assert_eq!(
            tsv(&rows()),
            "id\tbody\tn\nREQ-001\tTwo\\tlines\\nwith a \\\\\t1\nREQ-1000\tHéllo\t\n"
        );
    }

    #[test]
    fn json_has_the_columns_and_the_rows() {
        let document: serde_json::Value = serde_json::from_str(&json(&rows())).unwrap();
        assert_eq!(
            document,
            serde_json::json!({
                "columns": ["id", "body", "n"],
                "rows": [
                    ["REQ-001", "Two\tlines\nwith a \\", 1],
                    ["REQ-1000", "Héllo", null]
                ]
            })
        );
    }
}
