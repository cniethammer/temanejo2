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
    let mut preds: Vec<Vec<u32>> = vec![vec![]; n];
    let mut succs: Vec<Vec<u32>> = vec![vec![]; n];
    let mut indeg = vec![0u32; n];
    for e in &m.edges {
        preds[e.to as usize].push(e.from);
        succs[e.from as usize].push(e.to);
        indeg[e.to as usize] += 1;
    }

    let mut layer = vec![0u32; n];
    let mut queue: Vec<u32> = (0..n as u32).filter(|&i| indeg[i as usize] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head] as usize;
        head += 1;
        for &v in &succs[u] {
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
                let p = &preds[v as usize];
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
