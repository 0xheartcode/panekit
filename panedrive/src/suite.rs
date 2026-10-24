//! The suite runner's pure logic: axes, parameter sweeps, and the cells their
//! cartesian product expands to.
//!
//! A `run` is a *suite* when the script carries a `#@size:` / `#@env:` directive
//! or the CLI passes a `--sweep`. The orchestration (spawning a backend per cell,
//! running the script, emitting NDJSON) lives in the binary; the parsing and the
//! product live here so they can be unit-tested without a terminal. See
//! `docs/design/suite-runner.md`.

/// The axes declared in a script's `#@` directives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuiteAxes {
    /// Terminal sizes from `#@size: RxC, RxC` as `(rows, cols)`.
    pub sizes: Vec<(u16, u16)>,
    /// Child environment bindings from `#@env: K=V, K=V`, each an alternative.
    pub envs: Vec<(String, String)>,
}

impl SuiteAxes {
    /// Whether any axis was declared (so the run is a suite).
    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty() && self.envs.is_empty()
    }
}

/// Parse a script's `#@size:` and `#@env:` directives into suite axes. Scans
/// comment (`#`) lines anywhere in the text (the step parser drops them), but
/// only a comment whose body begins with the `@size:`/`@env:` marker is a
/// directive. An ordinary `# size:` comment (a plain note, or a convention some
/// other tool already reads) is left inert, so adding this feature never changes
/// the meaning of an existing script. A malformed `@size:`/`@env:` entry is an
/// error, so a typo surfaces rather than silently dropping a configuration.
pub fn parse_suite_header(text: &str) -> anyhow::Result<SuiteAxes> {
    let mut axes = SuiteAxes::default();
    for line in text.lines() {
        let line = line.trim();
        let Some(body) = line.strip_prefix('#') else {
            continue;
        };
        let body = body.trim();
        if let Some(rest) = body.strip_prefix("@size:") {
            for item in split_items(rest) {
                axes.sizes.push(parse_size(item)?);
            }
        } else if let Some(rest) = body.strip_prefix("@env:") {
            for item in split_items(rest) {
                axes.envs.push(parse_binding(item)?);
            }
        }
    }
    Ok(axes)
}

/// Parse a `--sweep VAR=RANGE` spec into a named axis of string values. `RANGE`
/// is either an inclusive integer range (`1..8`) or an explicit comma list
/// (`a,b,c`).
pub fn parse_sweep(spec: &str) -> anyhow::Result<(String, Vec<String>)> {
    let (name, range) = spec
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("--sweep needs VAR=RANGE, got {spec:?}"))?;
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("--sweep variable name is empty in {spec:?}");
    }
    let range = range.trim();
    let values = if let Some((lo, hi)) = range.split_once("..") {
        let lo: i64 = lo
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("--sweep range start is not an integer: {lo:?}"))?;
        let hi: i64 = hi
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("--sweep range end is not an integer: {hi:?}"))?;
        if hi < lo {
            anyhow::bail!("--sweep range {lo}..{hi} is empty (end before start)");
        }
        (lo..=hi).map(|n| n.to_string()).collect()
    } else {
        let vals: Vec<String> = split_items(range).map(str::to_string).collect();
        if vals.is_empty() {
            anyhow::bail!("--sweep list is empty in {spec:?}");
        }
        vals
    };
    Ok((name.to_string(), values))
}

/// One configuration to run: the terminal size, the child environment bindings,
/// and the id (axis bindings) that labels its result line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSpec {
    /// Rows of the spawned terminal.
    pub rows: u16,
    /// Columns of the spawned terminal.
    pub cols: u16,
    /// Environment variables to set on the child process for this cell.
    pub env: Vec<(String, String)>,
    /// The axis bindings labelling this cell, in a stable order, for the `id`
    /// field of the result line.
    pub id: Vec<(String, String)>,
}

/// Expand the axes and an optional sweep into the cartesian product of cells.
/// `base_rows`/`base_cols` are used when no size axis is declared. Always returns
/// at least one cell. The product order is size, then env, then sweep, so output
/// is deterministic.
pub fn build_cells(
    axes: &SuiteAxes,
    sweep: Option<&(String, Vec<String>)>,
    base_rows: u16,
    base_cols: u16,
) -> Vec<CellSpec> {
    // Each axis contributes, for a cell: an optional size, optional env binding,
    // and an optional id entry. An absent axis contributes a single "neutral"
    // choice so the product still yields one cell.
    let sizes: Vec<Option<(u16, u16)>> = if axes.sizes.is_empty() {
        vec![None]
    } else {
        axes.sizes.iter().map(|&s| Some(s)).collect()
    };
    let envs: Vec<Option<(String, String)>> = if axes.envs.is_empty() {
        vec![None]
    } else {
        axes.envs.iter().map(|b| Some(b.clone())).collect()
    };
    let sweeps: Vec<Option<String>> = match sweep {
        None => vec![None],
        Some((_, vals)) => vals.iter().map(|v| Some(v.clone())).collect(),
    };

    let mut cells = Vec::new();
    for size in &sizes {
        for env in &envs {
            for sw in &sweeps {
                let (rows, cols) = size.unwrap_or((base_rows, base_cols));
                let mut cell_env = Vec::new();
                let mut id = Vec::new();
                if let Some((r, c)) = size {
                    id.push(("size".to_string(), format!("{r}x{c}")));
                }
                if let Some((k, v)) = env {
                    cell_env.push((k.clone(), v.clone()));
                    id.push(("env".to_string(), format!("{k}={v}")));
                }
                if let (Some(v), Some((name, _))) = (sw, sweep) {
                    cell_env.push((name.clone(), v.clone()));
                    id.push((name.clone(), v.clone()));
                }
                cells.push(CellSpec {
                    rows,
                    cols,
                    env: cell_env,
                    id,
                });
            }
        }
    }
    cells
}

/// Split a comma-separated operand into trimmed, non-empty items.
fn split_items(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|p| !p.is_empty())
}

/// Parse `RxC` into `(rows, cols)`.
fn parse_size(item: &str) -> anyhow::Result<(u16, u16)> {
    let (r, c) = item
        .split_once(['x', 'X'])
        .ok_or_else(|| anyhow::anyhow!("size must be ROWSxCOLS (e.g. 24x80), got {item:?}"))?;
    let rows = r
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("size rows not a number in {item:?}"))?;
    let cols = c
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("size cols not a number in {item:?}"))?;
    Ok((rows, cols))
}

/// Parse `K=V` into a binding.
fn parse_binding(item: &str) -> anyhow::Result<(String, String)> {
    let (k, v) = item
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("env must be K=V, got {item:?}"))?;
    let k = k.trim();
    if k.is_empty() {
        anyhow::bail!("env key is empty in {item:?}");
    }
    Ok((k.to_string(), v.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_reads_size_and_env_axes() {
        let text = "#@size: 24x80, 40x120\n#@env: THEME=light, THEME=dark\npress Enter\n";
        let axes = parse_suite_header(text).unwrap();
        assert_eq!(axes.sizes, vec![(24, 80), (40, 120)]);
        assert_eq!(
            axes.envs,
            vec![
                ("THEME".to_string(), "light".to_string()),
                ("THEME".to_string(), "dark".to_string()),
            ]
        );
    }

    #[test]
    fn header_ignores_unrelated_comments_and_is_empty_without_axes() {
        let axes = parse_suite_header("# just a note\npress Enter\n").unwrap();
        assert!(axes.is_empty());
    }

    #[test]
    fn a_plain_size_or_env_comment_is_not_a_directive() {
        // The `@` marker is required. A bare `# size:`/`# env:` comment (which a
        // script may already carry for another tool) must stay inert, never turn
        // the run into a suite, and never be parsed (so it cannot even error).
        let text = "# size: 60x20\n# env: THEME=light\npress Enter\n";
        let axes = parse_suite_header(text).unwrap();
        assert!(axes.is_empty(), "a plain comment must not declare an axis");
    }

    #[test]
    fn header_rejects_a_malformed_size() {
        assert!(parse_suite_header("#@size: 24-80\n").is_err());
        assert!(parse_suite_header("#@size: wide x tall\n").is_err());
    }

    #[test]
    fn sweep_parses_ranges_and_lists() {
        assert_eq!(
            parse_sweep("SEED=1..3").unwrap(),
            ("SEED".to_string(), vec!["1".into(), "2".into(), "3".into()])
        );
        // A single-element range (start == end) is valid and yields one value;
        // only start > end is empty.
        assert_eq!(
            parse_sweep("N=5..5").unwrap(),
            ("N".to_string(), vec!["5".into()])
        );
        assert_eq!(
            parse_sweep("MODE=a, b ,c").unwrap(),
            ("MODE".to_string(), vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn sweep_rejects_bad_specs() {
        assert!(parse_sweep("noeq").is_err());
        assert!(parse_sweep("=1..3").is_err());
        assert!(parse_sweep("X=5..1").is_err());
        assert!(parse_sweep("X=a..b").is_err());
        assert!(parse_sweep("X=").is_err());
    }

    #[test]
    fn cells_are_the_cartesian_product_in_a_stable_order() {
        let axes = SuiteAxes {
            sizes: vec![(24, 80), (40, 120)],
            envs: vec![],
        };
        let sweep = ("SEED".to_string(), vec!["1".into(), "2".into()]);
        let cells = build_cells(&axes, Some(&sweep), 10, 10);
        // 2 sizes x 2 seeds = 4 cells, size-major then sweep.
        assert_eq!(cells.len(), 4);
        assert_eq!((cells[0].rows, cells[0].cols), (24, 80));
        assert_eq!(cells[0].env, vec![("SEED".to_string(), "1".to_string())]);
        assert_eq!(
            cells[0].id,
            vec![
                ("size".to_string(), "24x80".to_string()),
                ("SEED".to_string(), "1".to_string()),
            ]
        );
        assert_eq!((cells[3].rows, cells[3].cols), (40, 120));
        assert_eq!(cells[3].env, vec![("SEED".to_string(), "2".to_string())]);
    }

    #[test]
    fn no_axes_yields_one_neutral_cell_using_the_base_size() {
        let cells = build_cells(&SuiteAxes::default(), None, 24, 80);
        assert_eq!(cells.len(), 1);
        assert_eq!((cells[0].rows, cells[0].cols), (24, 80));
        assert!(cells[0].env.is_empty());
        assert!(cells[0].id.is_empty());
    }

    #[test]
    fn env_axis_sets_child_env_and_labels_the_id() {
        let axes = SuiteAxes {
            sizes: vec![],
            envs: vec![("THEME".to_string(), "dark".to_string())],
        };
        let cells = build_cells(&axes, None, 24, 80);
        assert_eq!(cells.len(), 1);
        assert_eq!(
            cells[0].env,
            vec![("THEME".to_string(), "dark".to_string())]
        );
        assert_eq!(
            cells[0].id,
            vec![("env".to_string(), "THEME=dark".to_string())]
        );
    }
}
