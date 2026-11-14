//! Minimal aligned-column text tables.

/// Render `rows` under `headers`, left-aligned, two spaces between columns.
/// Trailing whitespace is trimmed from each line.
pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let width = |col: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .chain([headers[col].len()])
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..headers.len()).map(width).collect();

    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let mut s = String::new();
        for (cell, w) in cells.zip(&widths) {
            s.push_str(cell);
            s.extend(std::iter::repeat_n(' ', w - cell.chars().count() + 2));
        }
        s.truncate(s.trim_end().len());
        s.push('\n');
        s
    };

    let mut out = line(&mut headers.iter().copied());
    for r in rows {
        out.push_str(&line(&mut r.iter().map(String::as_str)));
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn aligns() {
        let t = super::render(
            &["A", "BB"],
            &[
                vec!["xxx".into(), "1".into()],
                vec!["y".into(), String::new()],
            ],
        );
        assert_eq!(t, "A    BB\nxxx  1\ny\n");
    }
}
