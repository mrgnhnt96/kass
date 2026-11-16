//! Python's `difflib.SequenceMatcher`, step for step, so the stages that
//! align texts with it line up the same words.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// A run of `size` equal items at `a[a..]` and `b[b..]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Block {
    pub a: usize,
    pub b: usize,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

/// `(tag, i1, i2, j1, j2)`: `a[i1..i2]` becomes `b[j1..j2]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opcode {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

pub struct SequenceMatcher<'s, T> {
    a: &'s [T],
    b: &'s [T],
    b2j: HashMap<&'s T, Vec<usize>>,
}

impl<'s, T: Hash + Eq> SequenceMatcher<'s, T> {
    /// `SequenceMatcher(None, a, b, autojunk=autojunk)`.
    pub fn new(a: &'s [T], b: &'s [T], autojunk: bool) -> Self {
        let mut b2j: HashMap<&T, Vec<usize>> = HashMap::new();
        for (index, item) in b.iter().enumerate() {
            b2j.entry(item).or_default().push(index);
        }
        let n = b.len();
        if autojunk && n >= 200 {
            let ntest = n / 100 + 1;
            let popular: HashSet<&T> = b2j
                .iter()
                .filter(|(_, indices)| indices.len() > ntest)
                .map(|(item, _)| *item)
                .collect();
            for item in popular {
                b2j.remove(item);
            }
        }
        SequenceMatcher { a, b, b2j }
    }

    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Block {
        let (a, b) = (self.a, self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, item) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len = HashMap::new();
            if let Some(indices) = self.b2j.get(item) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = if j == 0 {
                        0
                    } else {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    } + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // No junk is set, so only the first two extensions can apply; they
        // reach over items autojunk dropped as popular.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        Block {
            a: besti,
            b: bestj,
            size: bestsize,
        }
    }

    pub fn get_matching_blocks(&self) -> Vec<Block> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let found = self.find_longest_match(alo, ahi, blo, bhi);
            let (i, j, k) = (found.a, found.b, found.size);
            if k > 0 {
                blocks.push(found);
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut joined = Vec::new();
        for Block {
            a: i2,
            b: j2,
            size: k2,
        } in blocks
        {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    joined.push(Block {
                        a: i1,
                        b: j1,
                        size: k1,
                    });
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            joined.push(Block {
                a: i1,
                b: j1,
                size: k1,
            });
        }
        joined.push(Block {
            a: la,
            b: lb,
            size: 0,
        });
        joined
    }

    pub fn get_opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0, 0);
        let mut answer = Vec::new();
        for Block { a: ai, b: bj, size } in self.get_matching_blocks() {
            let tag = if i < ai && j < bj {
                Some(Tag::Replace)
            } else if i < ai {
                Some(Tag::Delete)
            } else if j < bj {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(tag) = tag {
                answer.push(Opcode {
                    tag,
                    i1: i,
                    i2: ai,
                    j1: j,
                    j2: bj,
                });
            }
            (i, j) = (ai + size, bj + size);
            if size > 0 {
                answer.push(Opcode {
                    tag: Tag::Equal,
                    i1: ai,
                    i2: i,
                    j1: bj,
                    j2: j,
                });
            }
        }
        answer
    }

    pub fn ratio(&self) -> f64 {
        let matches: usize = self
            .get_matching_blocks()
            .iter()
            .map(|block| block.size)
            .sum();
        let total = self.a.len() + self.b.len();
        if total == 0 {
            1.0
        } else {
            2.0 * matches as f64 / total as f64
        }
    }
}

/// `SequenceMatcher(None, a, b).ratio()` for two strings, by character.
pub fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    SequenceMatcher::new(&a, &b, true).ratio()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcodes_match_python() {
        let a: Vec<char> = "qabxcd".chars().collect();
        let b: Vec<char> = "abycdf".chars().collect();
        let codes = SequenceMatcher::new(&a, &b, false).get_opcodes();
        let tags: Vec<_> = codes
            .iter()
            .map(|c| (c.tag, c.i1, c.i2, c.j1, c.j2))
            .collect();
        assert_eq!(
            tags,
            vec![
                (Tag::Delete, 0, 1, 0, 0),
                (Tag::Equal, 1, 3, 0, 2),
                (Tag::Replace, 3, 4, 2, 3),
                (Tag::Equal, 4, 6, 3, 5),
                (Tag::Insert, 6, 6, 5, 6),
            ]
        );
        assert!((ratio("abcd", "bcde") - 0.75).abs() < 1e-9);
    }
}
