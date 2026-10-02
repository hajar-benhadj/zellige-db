//! ASCII result tables — the REPL's face and the differential tests' eyes.

use crate::Output;
use crate::types::Value;

/// Render an [`Output`] as an ASCII table (psql-flavored).
pub fn render(output: &Output) -> String {
    match output {
        Output::Command { tag } => format!("{tag}\n"),
        Output::Query { columns, rows } => {
            let rendered: Vec<Vec<String>> = rows
                .iter()
                .map(|r| r.iter().map(Value::display).collect())
                .collect();
            let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
            for row in &rendered {
                for (i, cell) in row.iter().enumerate() {
                    widths[i] = widths[i].max(cell.chars().count());
                }
            }
            let bar = |l: char, m: char, r: char| {
                let mut line = String::new();
                line.push(l);
                for (i, w) in widths.iter().enumerate() {
                    line.push_str(&"─".repeat(w + 2));
                    line.push(if i + 1 == widths.len() { r } else { m });
                }
                line
            };
            let mut out = String::new();
            out.push_str(&bar('┌', '┬', '┐'));
            out.push('\n');
            out.push_str(&row_text(columns, &widths));
            out.push('\n');
            out.push_str(&bar('├', '┼', '┤'));
            out.push('\n');
            for row in &rendered {
                out.push_str(&row_text(row, &widths));
                out.push('\n');
            }
            out.push_str(&bar('└', '┴', '┘'));
            out.push_str(&format!(
                "\n({} row{})\n",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            ));
            out
        }
    }
}

fn row_text(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::from("│");
    for (i, w) in widths.iter().enumerate() {
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        line.push(' ');
        line.push_str(cell);
        let pad = w - cell.chars().count();
        line.push_str(&" ".repeat(pad));
        line.push_str(" │");
    }
    line
}
