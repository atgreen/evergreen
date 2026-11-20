// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::native_cycle_vertices;
use crate::t2::ir::Block;
use std::collections::{HashMap, HashSet};

fn cycle_vertices(successors: &HashMap<Block, Vec<Block>>) -> HashSet<Block> {
    let blocks: Vec<_> = successors.keys().copied().collect();
    native_cycle_vertices(&blocks, |block| successors[&block].iter().copied())
}

// Exact pre-change definition: for every emitted edge source -> target,
// select target iff it can reach source. Missing layout keys are dead ends.
fn original_cycle_vertices(successors: &HashMap<Block, Vec<Block>>) -> HashSet<Block> {
    let reaches = |start, goal| {
        let mut pending = vec![start];
        let mut seen = HashSet::new();
        while let Some(block) = pending.pop() {
            if block == goal {
                return true;
            }
            if !seen.insert(block) {
                continue;
            }
            if let Some(next) = successors.get(&block) {
                pending.extend(next.iter().copied());
            }
        }
        false
    };
    let mut headers = HashSet::new();
    for (&block, targets) in successors {
        for &target in targets {
            if reaches(target, block) {
                headers.insert(target);
            }
        }
    }
    headers
}

#[test]
fn exhaustive_directed_graphs_match_original_poll_membership() {
    for n in 0..=4 {
        for mask in 0_u32..(1 << (n * n)) {
            let successors: HashMap<_, Vec<_>> = (0..n)
                .map(|source| {
                    let targets = (0..n)
                        .filter(|&target| mask & (1 << (source * n + target)) != 0)
                        .map(Block)
                        .collect();
                    (Block(source), targets)
                })
                .collect();
            assert_eq!(
                cycle_vertices(&successors),
                original_cycle_vertices(&successors),
                "n={n}, adjacency mask={mask:#x}"
            );
        }
    }
}

#[test]
fn sparse_disconnected_irreducible_and_missing_layout_targets_match() {
    let a = Block(7);
    let b = Block(900);
    let c = Block(30_000);
    let d = Block(31_000);
    let e = Block(32_000);
    let f = Block(u32::MAX - 1);
    let outside = Block(u32::MAX);
    // A cycle, an acyclic bridge to another cycle, an irreducible SCC with
    // multiple entries, a disconnected self-loop, and missing/removed targets.
    let mut successors = HashMap::from([
        (a, vec![b, b, c, outside]),
        (b, vec![a, c, d]),
        (c, vec![d]),
        (d, vec![e]),
        (e, vec![d]),
        (f, vec![f]),
        (Block(100), vec![]),
    ]);
    let expected = HashSet::from([a, b, d, e, f]);
    assert_eq!(cycle_vertices(&successors), expected);
    assert_eq!(original_cycle_vertices(&successors), expected);

    // Removing a layout block makes all edges to it dead ends. Its old
    // adjacency must not be followed even if its numeric handle remains valid.
    successors.remove(&d);
    let expected = HashSet::from([a, b, f]);
    assert_eq!(cycle_vertices(&successors), expected);
    assert_eq!(original_cycle_vertices(&successors), expected);

    for targets in [vec![], vec![outside], vec![outside, f, f]] {
        let single = HashMap::from([(f, targets)]);
        assert_eq!(cycle_vertices(&single), original_cycle_vertices(&single));
    }

    // A nontrivial SCC whose vertices have no self-loop must all be kept,
    // including vertices reached from different entries into the component.
    let irreducible = HashMap::from([(a, vec![b, c]), (b, vec![c]), (c, vec![b, d]), (d, vec![b])]);
    assert_eq!(cycle_vertices(&irreducible), HashSet::from([b, c, d]));
    assert_eq!(
        cycle_vertices(&irreducible),
        original_cycle_vertices(&irreducible)
    );
}

#[test]
fn long_graphs_use_an_iterative_traversal_and_keep_every_cycle_vertex() {
    const N: u32 = 100_000;
    let mut successors: HashMap<_, _> = (0..N)
        .map(|i| {
            (
                Block(i),
                if i + 1 < N {
                    vec![Block(i + 1)]
                } else {
                    vec![]
                },
            )
        })
        .collect();
    assert!(cycle_vertices(&successors).is_empty());
    successors.get_mut(&Block(N - 1)).unwrap().push(Block(0));
    let cyclic = cycle_vertices(&successors);
    assert_eq!(cyclic.len(), N as usize);
    assert!((0..N).all(|i| cyclic.contains(&Block(i))));
}
