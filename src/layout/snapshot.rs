//! Flat, bounded split-tree records; disk data never supplies recursive Rust nodes.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedLayout {
    pub active: u64,
    pub zoomed: bool,
    pub nodes: Vec<SavedNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum SavedNode {
    Pane {
        id: u64,
    },
    Split {
        rows: bool,
        share: [u16; 2],
        first: usize,
        second: usize,
    },
}

impl Layout {
    pub(crate) fn saved(&self) -> SavedLayout {
        fn append(node: &Node, nodes: &mut Vec<SavedNode>) -> usize {
            let index = nodes.len();
            nodes.push(SavedNode::Pane { id: 0 });
            nodes[index] = match node {
                Node::Pane(id) => SavedNode::Pane { id: id.get() },
                Node::Split {
                    axis,
                    share,
                    first,
                    second,
                } => SavedNode::Split {
                    rows: *axis == SplitAxis::Rows,
                    share: [share.0, share.1],
                    first: append(first, nodes),
                    second: append(second, nodes),
                },
            };
            index
        }
        let mut nodes = Vec::new();
        append(&self.root, &mut nodes);
        SavedLayout {
            active: self.active.get(),
            zoomed: self.zoomed,
            nodes,
        }
    }

    pub(crate) fn from_saved(saved: &SavedLayout, rows: u16, columns: u16) -> io::Result<Self> {
        if saved.nodes.is_empty() || saved.nodes.len() > MAX_PANES * 2 - 1 {
            return Err(invalid("invalid saved pane count"));
        }
        fn build(
            index: usize,
            saved: &SavedLayout,
            visited: &mut HashSet<usize>,
            ids: &mut HashSet<u64>,
        ) -> io::Result<Node> {
            if !visited.insert(index) {
                return Err(invalid("saved split tree repeats a node"));
            }
            match saved
                .nodes
                .get(index)
                .ok_or_else(|| invalid("missing saved node"))?
            {
                SavedNode::Pane { id } => {
                    // TOML integers are signed; keep future IDs serializable too.
                    if *id >= i64::MAX as u64 || !ids.insert(*id) {
                        return Err(invalid("invalid or repeated saved pane ID"));
                    }
                    Ok(Node::Pane(PaneId(*id)))
                }
                SavedNode::Split {
                    rows,
                    share,
                    first,
                    second,
                } => {
                    if *first <= index || *second <= index || share[0] == 0 || share[0] >= share[1]
                    {
                        return Err(invalid("invalid saved split links or ratio"));
                    }
                    Ok(Node::Split {
                        axis: if *rows {
                            SplitAxis::Rows
                        } else {
                            SplitAxis::Columns
                        },
                        share: (share[0], share[1]),
                        first: Box::new(build(*first, saved, visited, ids)?),
                        second: Box::new(build(*second, saved, visited, ids)?),
                    })
                }
            }
        }
        let mut visited = HashSet::new();
        let mut ids = HashSet::new();
        let root = build(0, saved, &mut visited, &mut ids)?;
        if visited.len() != saved.nodes.len()
            || ids.len() > MAX_PANES
            || !ids.contains(&saved.active)
        {
            return Err(invalid(
                "saved layout has unreachable nodes or invalid focus",
            ));
        }
        let mut layout = Self {
            root,
            rows,
            columns,
            active: PaneId(saved.active),
            next_id: ids.iter().max().unwrap() + 1,
            count: ids.len(),
            zoomed: saved.zoomed && ids.len() > 1,
        };
        layout.resize(rows, columns)?;
        if layout
            .tiled_content_geometry()
            .panes
            .iter()
            .any(|(_, r)| r.rows == 0 || r.columns == 0)
        {
            return Err(invalid("saved panes do not fit inside their borders"));
        }
        Ok(layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_ratios_order_focus_zoom_and_allocates_fresh_ids() {
        let mut layout = Layout::new(40, 120).unwrap();
        layout.split_active(SplitAxis::Columns).unwrap();
        layout.split_active(SplitAxis::Rows).unwrap();
        layout.resize_active(Direction::Up);
        layout.swap_active_previous();
        layout.toggle_zoom();
        let saved = layout.saved();
        let mut restored = Layout::from_saved(&saved, 40, 120).unwrap();
        assert_eq!(restored.saved(), saved);
        assert_eq!(restored.tiled_geometry(), layout.tiled_geometry());
        restored.toggle_zoom();
        assert_eq!(restored.split_active(SplitAxis::Columns).unwrap().get(), 3);
        assert!(Layout::from_saved(&saved, 1, 1).is_err());
    }

    #[test]
    fn rejects_cycles_shared_nodes_duplicate_ids_and_bad_focus() {
        let mut layout = Layout::new(20, 80).unwrap();
        layout.split_active(SplitAxis::Columns).unwrap();
        for mutation in 0..6 {
            let mut saved = layout.saved();
            match mutation {
                0 => saved.active = 100,
                1 => saved.nodes[2] = SavedNode::Pane { id: 0 },
                2 => saved.nodes.push(SavedNode::Pane { id: 99 }),
                3..=5 => {
                    let SavedNode::Split {
                        first,
                        second,
                        share,
                        ..
                    } = &mut saved.nodes[0]
                    else {
                        unreachable!()
                    };
                    match mutation {
                        3 => *first = 0,
                        4 => *second = *first,
                        _ => *share = [2, 2],
                    }
                }
                _ => unreachable!(),
            }
            assert!(Layout::from_saved(&saved, 20, 80).is_err());
        }
    }
}
