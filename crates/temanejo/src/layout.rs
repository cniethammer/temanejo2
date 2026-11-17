//! Layered layout of the display graph (tasks as nodes).
//!
//! Layer = longest path from a root over creation and dependence edges, so
//! every edge points downwards. Within a layer nodes are ordered by the
//! barycenter of their predecessors. Layers wider than `max_row` are wrapped
//! into several rows to keep graphs with thousands of sibling tasks viewable.
//! Coordinates are in grid units; the GUI scales them.

use crate::model::Model;

pub fn layout(m: &Model) -> Vec<(f32, f32)> {
    let n = m.tasks.len();
    let preds = Adjacency::new(n, || m.edges().map(|e| (e.to, e.from)));
    let succs = Adjacency::new(n, || m.edges().map(|e| (e.from, e.to)));
    let mut indeg: Vec<u32> = (0..n).map(|i| preds.of(i).len() as u32).collect();

    let mut layer = vec![0u32; n];
    let mut queue: Vec<u32> = (0..n as u32).filter(|&i| indeg[i as usize] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head] as usize;
        head += 1;
        for &v in succs.of(u) {
            let v = v as usize;
            layer[v] = layer[v].max(layer[u] + 1);
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push(v as u32);
            }
        }
    }

    let n_layers = layer.iter().copied().max().map_or(0, |l| l as usize + 1);
    let mut layers: Vec<Vec<u32>> = vec![vec![]; n_layers];
    for i in 0..n {
        layers[layer[i] as usize].push(i as u32);
    }

    let max_row = ((n as f64).sqrt() * 2.0).ceil().max(48.0) as usize;
    let mut pos = vec![(0f32, 0f32); n];
    let mut row = 0f32;
    for nodes in &mut layers {
        let mut keyed: Vec<(f32, u32)> = nodes
            .iter()
            .map(|&v| {
                let p = preds.of(v as usize);
                let key = if p.is_empty() {
                    0.0
                } else {
                    p.iter().map(|&u| pos[u as usize].0).sum::<f32>() / p.len() as f32
                };
                (key, v)
            })
            .collect();
        // Ties keep creation order (task indices are assigned in creation order).
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for chunk in keyed.chunks(max_row) {
            let half = (chunk.len() as f32 - 1.0) / 2.0;
            for (i, &(_, v)) in chunk.iter().enumerate() {
                pos[v as usize] = (i as f32 - half, row);
            }
            row += 1.0;
        }
        // Extra space after a wrapped layer separates it from the next one.
        if keyed.len() > max_row {
            row += 0.5;
        }
    }
    pos
}

/// Lists of neighbours of every node in one array, each list in the order
/// of the pairs (`Vec<Vec<_>>` would cost 24 bytes per node more).
struct Adjacency {
    start: Vec<u32>,
    list: Vec<u32>,
}

impl Adjacency {
    fn new<I: Iterator<Item = (u32, u32)>>(n: usize, pairs: impl Fn() -> I) -> Adjacency {
        let mut start = vec![0u32; n + 1];
        for (a, _) in pairs() {
            start[a as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut list = vec![0u32; start[n] as usize];
        for (a, b) in pairs() {
            list[fill[a as usize] as usize] = b;
            fill[a as usize] += 1;
        }
        Adjacency { start, list }
    }

    fn of(&self, i: usize) -> &[u32] {
        &self.list[self.start[i] as usize..self.start[i + 1] as usize]
    }
}
