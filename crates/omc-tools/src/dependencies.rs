//! Dependencies between the items of a list, such as the steps of a todo list or the assignments of a `task` batch: naming them, checking that they form no cycle, and telling which items wait on others.

use std::collections::HashMap;

use omc_wire::{PlanStep, Progress};

/// One item of a list as it takes part in dependencies: the names it answers to and the names of the items it depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// The names other items can depend on it by, such as its id and its text; empty names are ignored.
    pub names: Vec<String>,
    /// The names of the items it depends on.
    pub depends_on: Vec<String>,
}

/// The dependencies of a list, resolved to positions: for each item, the positions of the items it depends on, without repeats, in the order first named.
pub type Graph = Vec<Vec<usize>>;

/// Resolves the dependencies of `nodes` to positions of the list.
///
/// # Errors
///
/// Returns a message for the model when a name is given to two items, an item depends on a name no item has or on itself, or the dependencies form a cycle, which the message lays out.
pub fn resolve(nodes: &[Node]) -> Result<Graph, String> {
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (position, node) in nodes.iter().enumerate() {
        for name in node.names.iter().map(|name| name.trim()) {
            if name.is_empty() {
                continue;
            }
            match by_name.insert(name, position) {
                Some(other) if other != position => {
                    return Err(format!(
                        "`{name}` names both item {} and item {}; give each item a name of its own",
                        other + 1,
                        position + 1
                    ));
                }
                _ => {}
            }
        }
    }
    let mut graph = Graph::with_capacity(nodes.len());
    for (position, node) in nodes.iter().enumerate() {
        let mut edges = Vec::new();
        for name in node.depends_on.iter().map(|name| name.trim()) {
            let Some(&target) = by_name.get(name) else {
                return Err(format!(
                    "item {} depends on `{name}`, which names no item of the list",
                    position + 1
                ));
            };
            if target == position {
                return Err(format!("item {} depends on itself", position + 1));
            }
            if !edges.contains(&target) {
                edges.push(target);
            }
        }
        graph.push(edges);
    }
    if let Some(cycle) = find_cycle(&graph) {
        let label = |position: &usize| label_of(nodes, *position);
        let mut path: Vec<String> = cycle.iter().map(label).collect();
        path.extend(cycle.first().map(label));
        return Err(format!(
            "the dependencies form a cycle: {}",
            path.join(" -> ")
        ));
    }
    Ok(graph)
}

/// How an item at `position` of `nodes` is named in a message: its first non-empty name, or its place counted from one.
fn label_of(nodes: &[Node], position: usize) -> String {
    nodes[position]
        .names
        .iter()
        .map(|name| name.trim())
        .find(|name| !name.is_empty())
        .map_or_else(
            || format!("item {}", position + 1),
            |name| format!("`{name}`"),
        )
}

/// The state of an item while [`find_cycle`] walks the graph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    /// Not reached yet.
    Fresh,
    /// On the path being walked.
    Open,
    /// Walked, with no cycle through it.
    Closed,
}

/// The items of a cycle of `graph`, each depending on the next and the last on the first, when it has one.
pub fn find_cycle(graph: &Graph) -> Option<Vec<usize>> {
    let mut visits = vec![Visit::Fresh; graph.len()];
    for start in 0..graph.len() {
        if visits[start] != Visit::Fresh {
            continue;
        }
        let mut path: Vec<(usize, usize)> = vec![(start, 0)];
        visits[start] = Visit::Open;
        while let Some(&mut (node, ref mut next)) = path.last_mut() {
            let Some(&target) = graph[node].get(*next) else {
                visits[node] = Visit::Closed;
                path.pop();
                continue;
            };
            *next += 1;
            match visits.get(target).copied() {
                Some(Visit::Open) => {
                    let from = path.iter().position(|(item, _)| *item == target)?;
                    return Some(path[from..].iter().map(|(item, _)| *item).collect());
                }
                Some(Visit::Fresh) => {
                    visits[target] = Visit::Open;
                    path.push((target, 0));
                }
                _ => {}
            }
        }
    }
    None
}

/// For each item of `graph`, the positions of the items that depend on it directly.
pub fn dependents(graph: &Graph) -> Graph {
    let mut reverse = vec![Vec::new(); graph.len()];
    for (position, edges) in graph.iter().enumerate() {
        for &target in edges {
            reverse[target].push(position);
        }
    }
    reverse
}

/// The positions of every item that depends on `start`, directly or through others, in the order they are reached.
pub fn downstream(graph: &Graph, start: usize) -> Vec<usize> {
    let reverse = dependents(graph);
    let mut seen = vec![false; graph.len()];
    let mut queue = vec![start];
    let mut found = Vec::new();
    while let Some(node) = queue.pop() {
        for &next in &reverse[node] {
            if !seen[next] {
                seen[next] = true;
                found.push(next);
                queue.push(next);
            }
        }
    }
    found
}

/// Marks as blocked each step of `steps` that is not done and depends on a step that is not done, as `graph` says; every other step is marked not blocked.
pub fn mark_blocked(steps: &mut [PlanStep], graph: &Graph) {
    let done: Vec<bool> = steps
        .iter()
        .map(|step| step.progress == Progress::Done)
        .collect();
    for (position, step) in steps.iter_mut().enumerate() {
        step.blocked = !done[position]
            && graph
                .get(position)
                .is_some_and(|edges| edges.iter().any(|&target| !done[target]));
    }
}

/// The nodes of `steps`: each named by its id and its text, depending on what it lists.
pub fn nodes_of(steps: &[PlanStep]) -> Vec<Node> {
    steps
        .iter()
        .map(|step| Node {
            names: step
                .id
                .iter()
                .cloned()
                .chain(std::iter::once(step.text.clone()))
                .collect(),
            depends_on: step.depends_on.clone(),
        })
        .collect()
}

#[cfg(test)]
/// Tests for resolving dependencies.
mod tests {
    use super::*;

    /// A node named `name` depending on `depends_on`.
    fn node(name: &str, depends_on: &[&str]) -> Node {
        Node {
            names: vec![name.to_owned()],
            depends_on: depends_on.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// Names resolve to positions, repeats dropped.
    #[test]
    fn resolves_names() {
        let graph = resolve(&[
            node("a", &[]),
            node("b", &["a", "a"]),
            node("c", &["a", "b"]),
        ])
        .unwrap();
        assert_eq!(graph, vec![vec![], vec![0], vec![0, 1]]);
    }

    /// A cycle is refused and laid out.
    #[test]
    fn refuses_cycles() {
        let error =
            resolve(&[node("a", &["c"]), node("b", &["a"]), node("c", &["b"])]).unwrap_err();
        assert!(error.contains("cycle"), "{error}");
        assert!(error.contains("`a`") && error.contains("`b`") && error.contains("`c`"));
    }

    /// Unknown names, self-dependencies and shared names are refused.
    #[test]
    fn refuses_bad_names() {
        assert!(resolve(&[node("a", &["x"])]).unwrap_err().contains("`x`"));
        assert!(
            resolve(&[node("a", &["a"])])
                .unwrap_err()
                .contains("itself")
        );
        assert!(
            resolve(&[node("a", &[]), node("a", &[])])
                .unwrap_err()
                .contains("both")
        );
    }

    /// Everything downstream of an item is found, through chains.
    #[test]
    fn finds_downstream() {
        let graph = resolve(&[
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["b"]),
            node("d", &[]),
        ])
        .unwrap();
        let mut found = downstream(&graph, 0);
        found.sort_unstable();
        assert_eq!(found, vec![1, 2]);
        assert!(downstream(&graph, 3).is_empty());
    }

    /// A step waiting on an unfinished step is blocked; one whose dependencies are done is not.
    #[test]
    fn marks_blocked_steps() {
        let step = |text: &str, progress, depends_on: &[&str]| PlanStep {
            text: text.to_owned(),
            progress,
            id: None,
            depends_on: depends_on.iter().map(|name| (*name).to_owned()).collect(),
            blocked: false,
        };
        let mut steps = vec![
            step("a", Progress::Done, &[]),
            step("b", Progress::Pending, &["a"]),
            step("c", Progress::Pending, &["b"]),
        ];
        let graph = resolve(&nodes_of(&steps)).unwrap();
        mark_blocked(&mut steps, &graph);
        let blocked: Vec<bool> = steps.iter().map(|step| step.blocked).collect();
        assert_eq!(blocked, vec![false, false, true]);
    }
}
