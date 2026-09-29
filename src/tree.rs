//! Collapsible tree view over `/`-separated names (tags and runs).
//!
//! Chains of single-child groups are merged ("a/b/c" shows as one row), and a
//! group holding a single leaf becomes that leaf, so long common prefixes
//! don't eat the sidebar.

use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug)]
pub struct Row {
    pub depth: usize,
    /// text shown (one or more path segments)
    pub label: String,
    /// full name for leaves, group prefix for groups
    pub path: String,
    /// leaf: index into the item list
    pub leaf: Option<usize>,
    /// group: whether its children are shown
    pub expanded: bool,
    /// all leaf indices at or below this row
    pub leaves: Vec<usize>,
}

impl Row {
    pub fn is_group(&self) -> bool {
        self.leaf.is_none()
    }
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    leaf: Option<usize>,
}

impl Node {
    fn all_leaves(&self, out: &mut Vec<usize>) {
        out.extend(self.leaf);
        for c in self.children.values() {
            c.all_leaves(out);
        }
    }
}

/// Build visible rows for `items` (full names). `keep[i]` = item passes the filter.
pub fn build(items: &[String], keep: &[bool], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut root = Node::default();
    for (i, name) in items.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let mut n = &mut root;
        for seg in name.split('/') {
            n = n.children.entry(seg.to_string()).or_default();
        }
        n.leaf = Some(i);
    }
    let mut rows = Vec::new();
    emit(&root, "", "", 0, collapsed, &mut rows);
    rows
}

/// `label_prefix` is prepended to labels when a node is both a leaf and a
/// group ("loss" and "loss/train"): its children are shown flat beside it.
fn emit(node: &Node, prefix: &str, label_prefix: &str, depth: usize, collapsed: &HashSet<String>, rows: &mut Vec<Row>) {
    for (seg, child) in &node.children {
        let mut label = format!("{label_prefix}{seg}");
        let mut path = if prefix.is_empty() { seg.clone() } else { format!("{prefix}/{seg}") };
        let mut c = child;
        // merge single-child chains: "a" -> "b" -> leaf  ==>  "a/b"
        while c.leaf.is_none() && c.children.len() == 1 {
            let (s2, c2) = c.children.iter().next().unwrap();
            label = format!("{label}/{s2}");
            path = format!("{path}/{s2}");
            c = c2;
        }
        if let Some(i) = c.leaf {
            rows.push(Row {
                depth,
                label: label.clone(),
                path: path.clone(),
                leaf: Some(i),
                expanded: false,
                leaves: vec![i],
            });
        }
        if c.leaf.is_some() && !c.children.is_empty() {
            emit(c, &path, &format!("{label}/"), depth, collapsed, rows);
        } else if !c.children.is_empty() {
            let mut leaves = Vec::new();
            for cc in c.children.values() {
                cc.all_leaves(&mut leaves);
            }
            let expanded = !collapsed.contains(&path);
            rows.push(Row { depth, label, path: path.clone(), leaf: None, expanded, leaves });
            if expanded {
                emit(c, &path, "", depth + 1, collapsed, rows);
            }
        }
    }
}

/// Index of the parent group row of `rows[i]`, if any.
pub fn parent(rows: &[Row], i: usize) -> Option<usize> {
    let d = rows.get(i)?.depth;
    (0..i).rev().find(|&j| rows[j].depth < d && rows[j].is_group())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merges_chains_and_collapses() {
        let items = names(&["exp/2024/lr_1e-3/seed0", "exp/2024/lr_1e-3/seed1", "exp/2024/lr_3e-4", "loss"]);
        let keep = vec![true; 4];
        let rows = build(&items, &keep, &HashSet::new());
        let labels: Vec<_> = rows.iter().map(|r| (r.depth, r.label.as_str())).collect();
        assert_eq!(
            labels,
            vec![(0, "exp/2024"), (1, "lr_1e-3"), (2, "seed0"), (2, "seed1"), (1, "lr_3e-4"), (0, "loss")]
        );
        assert_eq!(rows[0].leaves.len(), 3);
        assert_eq!(parent(&rows, 3), Some(1));

        let collapsed: HashSet<String> = ["exp/2024/lr_1e-3".to_string()].into();
        let rows = build(&items, &keep, &collapsed);
        assert_eq!(rows.len(), 4);
        assert!(!rows[1].expanded);
    }

    #[test]
    fn filter_and_leaf_group_same_name() {
        let items = names(&["loss", "loss/train", "acc"]);
        let rows = build(&items, &[true, true, false], &HashSet::new());
        let labels: Vec<_> = rows.iter().map(|r| (r.label.as_str(), r.is_group())).collect();
        assert_eq!(labels, vec![("loss", false), ("loss/train", false)]);
    }
}
