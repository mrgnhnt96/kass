//! Take one round of corrections back out of the rounds made after it.
//!
//! A capture's corrections stack: each holds the whole text as the user left
//! it, starting from the one before. Removing a round that newer ones were
//! built on takes back only its own changes and keeps theirs, a three-way
//! merge by words. Where a newer round changed the same words, the newer
//! round wins.

use crate::difflib::{Opcode, SequenceMatcher, Tag};
use crate::re;

fn tokens(text: &str) -> Vec<&str> {
    re!(r"\w+|\s+|[^\w\s]").findall(text)
}

/// `newer` without the changes that turned `before` into `removed`.
///
/// `newer` was made from `removed`; its own changes are kept.
pub fn take_back(before: &str, removed: &str, newer: &str) -> String {
    merge(&tokens(removed), &tokens(newer), &tokens(before)).concat()
}

/// `base` with the changes of both sides; `ours` wins where they touch.
fn merge<'t>(base: &[&'t str], ours: &[&'t str], theirs: &[&'t str]) -> Vec<&'t str> {
    let sides: Vec<(&[&str], Vec<Opcode>)> = [ours, theirs]
        .into_iter()
        .map(|side| (side, SequenceMatcher::new(base, side, false).get_opcodes()))
        .collect();
    let mut changes: Vec<(usize, usize, usize)> = sides
        .iter()
        .enumerate()
        .flat_map(|(which, (_, ops))| {
            ops.iter().filter(|op| op.tag != Tag::Equal).map(move |op| (op.i1, op.i2, which))
        })
        .collect();
    changes.sort();
    let mut out = Vec::new();
    let mut at = 0;
    let mut k = 0;
    while k < changes.len() {
        let (lo, mut hi, which) = changes[k];
        let mut involved = vec![which];
        k += 1;
        // Changes that overlap are one chunk; ones that only meet both apply.
        while k < changes.len() && changes[k].0 < hi {
            hi = hi.max(changes[k].1);
            involved.push(changes[k].2);
            k += 1;
        }
        out.extend_from_slice(&base[at..lo]);
        let (side, ops) = if involved.contains(&0) { &sides[0] } else { &sides[1] };
        out.extend(slice(side, ops, lo, hi));
        at = hi;
    }
    out.extend_from_slice(&base[at..]);
    out
}

/// What `side` has where `base` has `lo..hi`, whose ends no change of `side` crosses.
fn slice<'t>(side: &[&'t str], ops: &[Opcode], lo: usize, hi: usize) -> Vec<&'t str> {
    let mut out = Vec::new();
    for op in ops {
        if op.tag == Tag::Equal {
            let (start, end) = (op.i1.max(lo), op.i2.min(hi));
            if start < end {
                out.extend_from_slice(&side[op.j1 + start - op.i1..op.j1 + end - op.i1]);
            }
        } else if lo <= op.i1 && op.i2 <= hi {
            out.extend_from_slice(&side[op.j1..op.j2]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newer_round() {
        let before = "Hi Megan, see you at five.";
        let removed = "Hi Morgan, see you at five.";
        let newer = "Hi Morgan, see you at six.";
        assert_eq!(take_back(before, removed, newer), "Hi Megan, see you at six.");
    }
}
