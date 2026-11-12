//! Where each part's tile goes on the canvas (`grid`, `grid_cells` and
//! `ordered_tiles` in `app/ytpmv/render.py`).

pub const GRID_LAYOUTS: [&str; 6] = ["balanced", "classic", "two-rows", "two-columns", "strips", "featured"];

pub type Cell = (usize, usize, usize, usize);

/// The original grid's row/column choice, with equal widescreen tiles.
pub fn classic_grid_shape(n: usize) -> (usize, usize) {
    let n = n.max(1);
    let mut best = (1usize, n);
    let mut best_key = f64::INFINITY;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let key = (cols as f64 / rows as f64).ln().abs() + 0.3 * ((cols * rows) as f64 - n as f64);
        if key < best_key {
            best_key = key;
            best = (cols, rows);
        }
    }
    best
}

fn square_rows(n: usize) -> usize {
    let cols = (n.max(1) as f64).sqrt().ceil() as usize;
    n.max(1).div_ceil(cols)
}

/// `(x, y, width, height)` of each of `n` tiles in the canvas.
///
/// Balance the row counts, putting the extra cells in the upper rows. Three
/// parts are two on top and one centred across the full lower row. Fill
/// layouts partition every pixel, including console-sized canvases. Classic
/// keeps equal 16:9 cells, leaving unused space black and optionally
/// centring incomplete rows.
pub fn grid_cells(n: usize, width: usize, height: usize, layout: &str, centered: bool) -> Vec<Cell> {
    let n = n.max(1);
    if layout == "classic" {
        let (cols, rows) = classic_grid_shape(n);
        let scale = (width as f64 / (cols * 16) as f64).min(height as f64 / (rows * 9) as f64);
        let tw = ((16.0 * scale).floor() as usize).max(1);
        let th = ((9.0 * scale).floor() as usize).max(1);
        let top = if centered { height.saturating_sub(rows * th) / 2 } else { 0 };
        let mut cells = Vec::new();
        for r in 0..rows {
            let count = cols.min(n - r * cols);
            let left = if centered { width.saturating_sub(count * tw) / 2 } else { 0 };
            cells.extend((0..count).map(|c| (left + c * tw, top + r * th, tw, th)));
        }
        return cells;
    }
    if layout == "featured" && n > 1 {
        let split = width * 2 / 3;
        let mut cells = vec![(0, 0, split, height)];
        cells.extend(
            grid_cells(n - 1, width - split, height, "sidebar", true).into_iter().map(|(x, y, w, h)| (x + split, y, w, h)),
        );
        return cells;
    }
    let rows = match layout {
        "two-rows" => n.min(2),
        "two-columns" => n.div_ceil(2),
        "strips" => 1,
        "sidebar" => n.min(((n as f64 * height as f64 / width as f64).sqrt().round_ties_even() as usize).max(1)),
        _ => square_rows(n),
    };
    let mut cells = Vec::new();
    for row in 0..rows {
        let count = n / rows + usize::from(row < n % rows);
        let (y0, y1) = (row * height / rows, (row + 1) * height / rows);
        for col in 0..count {
            let (x0, x1) = (col * width / count, (col + 1) * width / count);
            cells.push((x0, y0, x1 - x0, y1 - y0));
        }
    }
    cells
}

/// Ignore stale/duplicate ids; unspecified visible tracks keep MIDI order.
pub fn ordered_ids(ids: Vec<String>, order: Option<&[String]>) -> Vec<String> {
    let Some(order) = order else { return ids };
    let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(order.len());
    let mut ids = ids;
    ids.sort_by_key(|id| rank(id));
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_partition_the_canvas() {
        for layout in ["balanced", "two-rows", "two-columns", "strips", "featured"] {
            for n in 1..=12 {
                let cells = grid_cells(n, 1920, 1080, layout, true);
                assert_eq!(cells.len(), n, "{layout} {n}");
                let area: usize = cells.iter().map(|c| c.2 * c.3).sum();
                assert_eq!(area, 1920 * 1080, "{layout} {n} fills the canvas");
            }
        }
    }

    #[test]
    fn three_parts_are_two_over_one() {
        let c = grid_cells(3, 1920, 1080, "balanced", true);
        assert_eq!(c, vec![(0, 0, 960, 540), (960, 0, 960, 540), (0, 540, 1920, 540)]);
    }

    #[test]
    fn classic_keeps_sixteen_by_nine() {
        let c = grid_cells(5, 1920, 1080, "classic", true);
        assert_eq!(c.len(), 5);
        assert!(c.iter().all(|(_, _, w, h)| w * 9 / 16 == *h || (*w as f64 / *h as f64 - 16.0 / 9.0).abs() < 0.05));
    }

    #[test]
    fn order_ignores_unknown_ids() {
        let ids = vec!["a".to_string(), "b".into(), "c".into()];
        let order = vec!["c".to_string(), "zzz".into(), "a".into()];
        assert_eq!(ordered_ids(ids, Some(&order)), vec!["c", "a", "b"]);
    }
}
