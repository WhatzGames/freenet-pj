//! Fractional indexing for ordering tasks within a column.
//!
//! Dragging a card between two others has to produce an order key that no other
//! peer will ever collide with, without renumbering the neighbours — renumbering
//! would mean emitting an op per card and would fight with concurrent edits.
//!
//! A [`Rank`] is a string of base-62 digits read as the fraction `0.d1d2d3…`.
//! Because the digit alphabet is in ASCII order and no rank ends in the zero
//! digit, plain lexicographic string comparison is exactly numeric comparison,
//! so ranks sort correctly with a derived `Ord` and there is always room to
//! insert another value between any two of them.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Digits in ascending ASCII order, so lexicographic order matches digit value.
const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: usize = 62;

fn digit_value(c: u8) -> Option<usize> {
    DIGITS.iter().position(|&d| d == c)
}

fn digit_char(value: usize) -> u8 {
    DIGITS[value]
}

/// An order key for a task within a column, or for a column within a board.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Rank(String);

impl Rank {
    /// A rank in the middle of the range, for the first item in an empty column.
    pub fn middle() -> Self {
        Self::midpoint(b"", None)
    }

    /// A rank strictly between `lo` and `hi`.
    ///
    /// `None` for `lo` means "before everything", `None` for `hi` means "after
    /// everything", so this covers dropping a card at either end of a column as
    /// well as between two neighbours.
    pub fn between(lo: Option<&Rank>, hi: Option<&Rank>) -> Self {
        // Stay total if a caller passes a reversed or equal pair: fall back to
        // appending after `lo` rather than producing a nonsense key.
        if let (Some(l), Some(h)) = (lo, hi) {
            if l.0 >= h.0 {
                return Self::midpoint(l.0.as_bytes(), None);
            }
        }
        Self::midpoint(
            lo.map_or(b"".as_slice(), |r| r.0.as_bytes()),
            hi.map(|r| r.0.as_bytes()),
        )
    }

    pub fn before(hi: &Rank) -> Self {
        Self::between(None, Some(hi))
    }

    pub fn after(lo: &Rank) -> Self {
        Self::between(Some(lo), None)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses a rank received over the wire. Rejects anything non-canonical so
    /// that equal ranks always have equal bytes.
    pub fn parse(s: &str) -> Result<Self> {
        let bytes = s.as_bytes();
        let canonical = !bytes.is_empty()
            && bytes.iter().all(|&c| digit_value(c).is_some())
            && bytes[bytes.len() - 1] != DIGITS[0];
        if canonical {
            Ok(Rank(s.to_owned()))
        } else {
            Err(Error::BadRank(s.to_owned()))
        }
    }

    /// The core fractional-index step: the shortest base-62 fraction strictly
    /// between `a` (empty meaning 0) and `b` (`None` meaning 1).
    ///
    /// Walks both bounds digit by digit. As long as the bounds share a digit
    /// there is no room to split, so that digit is emitted and the walk
    /// continues. Once a gap of at least two opens up, the midpoint digit ends
    /// the rank. Once `a`'s digit is strictly below `b`'s, `b` can no longer
    /// constrain the suffix and the upper bound is released.
    fn midpoint(a: &[u8], b: Option<&[u8]>) -> Self {
        let mut out: Vec<u8> = Vec::new();
        let mut bounded_above = b.is_some();
        let mut i = 0usize;

        loop {
            // Past the end of `a` the remaining digits are zero.
            let lo = a.get(i).and_then(|&c| digit_value(c)).unwrap_or(0);
            let hi = if bounded_above {
                if let Some(&c) = b.expect("bounded_above implies b").get(i) {
                    digit_value(c).unwrap_or(0)
                } else {
                    // `b` ran out while still matching digit for digit, which
                    // means a >= b. `between` filters that out before we get
                    // here; stay total rather than spinning.
                    debug_assert!(false, "midpoint called with a >= b");
                    out.push(digit_char(BASE / 2));
                    break;
                }
            } else {
                BASE
            };

            if hi > lo + 1 {
                // Room to land between the bounds. The midpoint of a gap of two
                // or more is never the zero digit, so the result stays canonical.
                out.push(digit_char(lo.midpoint(hi)));
                break;
            }

            if hi == lo + 1 {
                // Taking `lo` here keeps us below `b` no matter what follows.
                bounded_above = false;
            }
            out.push(digit_char(lo));
            i += 1;
        }

        Rank(String::from_utf8(out).expect("digit alphabet is ascii"))
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rank({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_canonical(r: &Rank) {
        assert!(Rank::parse(r.as_str()).is_ok(), "not canonical: {r:?}");
    }

    #[test]
    fn middle_is_canonical_and_splittable_both_ways() {
        let m = Rank::middle();
        assert_canonical(&m);
        assert!(Rank::before(&m) < m);
        assert!(Rank::after(&m) > m);
    }

    #[test]
    fn between_is_strictly_between() {
        let a = Rank::middle();
        let b = Rank::after(&a);
        let mid = Rank::between(Some(&a), Some(&b));
        assert!(a < mid && mid < b, "{a:?} < {mid:?} < {b:?}");
        assert_canonical(&mid);
    }

    /// The property that matters for drag-and-drop: repeatedly dropping a card
    /// into the same gap must keep working, and must never run out of room.
    #[test]
    fn repeated_insertion_into_the_same_gap_never_collides() {
        let lo = Rank::middle();
        let mut hi = Rank::after(&lo);
        for step in 0..500 {
            let mid = Rank::between(Some(&lo), Some(&hi));
            assert!(lo < mid, "step {step}: {lo:?} !< {mid:?}");
            assert!(mid < hi, "step {step}: {mid:?} !< {hi:?}");
            assert_canonical(&mid);
            hi = mid;
        }
    }

    /// Same, but always inserting just after a fixed rank, which is the "drop at
    /// the top of the column" case.
    #[test]
    fn repeated_prepending_and_appending_stay_ordered() {
        let mut first = Rank::middle();
        let mut last = first.clone();
        for step in 0..500 {
            let before = Rank::before(&first);
            assert!(before < first, "step {step}: prepend went wrong");
            assert_canonical(&before);
            first = before;

            let after = Rank::after(&last);
            assert!(after > last, "step {step}: append went wrong");
            assert_canonical(&after);
            last = after;
        }
        assert!(first < last);
    }

    /// Ranks built by many interleaved inserts must still sort into the order
    /// they were logically placed in.
    #[test]
    fn ordering_survives_a_shuffled_build() {
        // Build a column of 60 ranks by always inserting into the widest-looking
        // gap (index 1), then check the sequence is ascending.
        let mut ranks = vec![Rank::middle()];
        ranks.push(Rank::after(&ranks[0]));
        for _ in 0..60 {
            let mid = Rank::between(Some(&ranks[0]), Some(&ranks[1]));
            ranks.insert(1, mid);
        }
        for pair in ranks.windows(2) {
            assert!(pair[0] < pair[1], "{:?} !< {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn parse_rejects_non_canonical_forms() {
        assert!(Rank::parse("").is_err(), "empty");
        assert!(Rank::parse("V0").is_err(), "trailing zero digit");
        assert!(Rank::parse("V!").is_err(), "digit outside the alphabet");
        assert!(Rank::parse("V").is_ok());
        assert!(Rank::parse("0V").is_ok(), "leading zero digit is fine");
    }

    #[test]
    fn reversed_bounds_do_not_panic_or_hang() {
        let a = Rank::middle();
        let b = Rank::after(&a);
        // Deliberately backwards.
        let r = Rank::between(Some(&b), Some(&a));
        assert_canonical(&r);
        assert!(r > b);
    }
}
