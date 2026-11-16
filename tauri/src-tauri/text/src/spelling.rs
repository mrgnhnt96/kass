//! How close spelled letters are to a word, and the case they take.
//!
//! The parts of the backend's `spelling` module (and `dictionary._sound`)
//! that voice edits use. Joining spelled-out text stays in Python for now.

use crate::py::{self, Groups};
use crate::re;

/// The digraphs and letters spelled differently that sound the same.
const SOUNDS: &[(&str, &str)] = &[
    ("ph", "f"),
    ("ck", "k"),
    ("q", "k"),
    ("c", "k"),
    ("z", "s"),
    ("y", "i"),
];

/// `text`'s letters as they sound: no spaces or marks, one of each doubled letter.
///
/// Digits are kept as they are: "7" and "77" are different words.
pub fn sound(text: &str) -> String {
    let mut key = re!(r"[\W_]+").replace(&py::casefold(text), "");
    for (spelled, sounds) in SOUNDS {
        key = key.replace(spelled, sounds);
    }
    re!(r"([^\W\d_])\1+").sub(&key, |found| found.g(1).to_string())
}

/// Edit distance with swapped neighbours as one edit, or `limit + 1` past it.
pub fn distance(a: &str, b: &str, limit: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > limit {
        return limit + 1;
    }
    let mut before: Vec<usize> = Vec::new();
    let mut previous: Vec<usize>;
    let mut current: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        previous = std::mem::replace(&mut current, vec![0; b.len() + 1]);
        current[0] = i;
        for j in 1..=b.len() {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(before[j - 2] + 1);
            }
        }
        if current.iter().min().copied().unwrap_or(0) > limit {
            return limit + 1;
        }
        before = previous;
    }
    *current.last().unwrap()
}

/// How many sounds spelled `letters` are off `word`, when few enough for
/// `word` to be them misheard: the first the same, and two in five of the
/// longer of the two ("VOVA" for "vulva", "MEGHAN" for "Megan"); `None`
/// when `word` is another word.
pub fn sounds_like(letters: &str, word: &str) -> Option<usize> {
    let (a, b) = (sound(letters), sound(word));
    if a.is_empty() || b.is_empty() || py::first(&a) != py::first(&b) {
        return None;
    }
    let limit = 2 * py::len(&a).max(py::len(&b)) / 5;
    let off = distance(&a, &b, limit);
    (off <= limit).then_some(off)
}

/// Spelled letters in the case of the word they replace: they carry none.
pub fn shaped(letters: &str, like: &str) -> String {
    if py::isupper(like) && py::len(like) > 1 {
        return py::upper(letters);
    }
    if py::isupper(py::head(like, 1)) {
        return py::upper(py::head(letters, 1)) + &py::lower(py::tail_from(letters, 1));
    }
    py::lower(letters)
}

/// Whether `text` reads as letters spelled aloud: three or more joined in
/// capitals ("MEGHAN"), or two Whisper left dashed ("B-O").
pub fn is_spelled(text: &str) -> bool {
    let letters = text.replace('-', "");
    if letters.is_empty() || !letters.chars().all(py::is_alpha) || !py::isupper(&letters) {
        return false;
    }
    let count = py::len(&letters);
    count >= 3 || (count == 2 && text.contains('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_sounds() {
        assert_eq!(sound("Phillip"), "filip");
        assert_eq!(distance("abcd", "abdc", 2), 1);
        assert_eq!(sounds_like("MEGHAN", "Megan"), Some(1));
        assert_eq!(sounds_like("VOVA", "vulva"), Some(2));
        assert_eq!(sounds_like("MEGHAN", "Tuesday"), None);
        assert_eq!(shaped("meghan", "Megan"), "Meghan");
        assert!(
            is_spelled("MEGHAN") && is_spelled("B-O") && !is_spelled("BO") && !is_spelled("Megan")
        );
    }
}
