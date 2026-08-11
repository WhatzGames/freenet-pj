//! The ops that turn an empty contract state into a usable board.
//!
//! A freshly published contract instance holds no state at all, so the client
//! that creates a board has to write its own genesis: name the owner and lay out
//! the starting columns. Keeping that here rather than in the frontend means the
//! shape of a new board is testable without a browser.

use crate::ids::{ColumnId, MemberId};
use crate::op::Op;
use crate::rank::Rank;

/// Columns a new board starts with.
pub const DEFAULT_COLUMNS: [&str; 4] = ["Backlog", "Todo", "In Progress", "Done"];

/// Builds the genesis ops for a new board, left to right.
///
/// `column_ids` supplies the randomness — this crate deliberately has no RNG so
/// that it compiles unchanged into the contract, where there is no entropy source
/// to speak of. Pairs each id with a default title and stops at the shorter of
/// the two.
pub fn genesis_ops(owner: MemberId, owner_name: &str, column_ids: &[ColumnId]) -> Vec<Op> {
    let mut ops = Vec::with_capacity(column_ids.len() + 1);

    // Naming the owner is all this does. Their *authority* comes from the contract
    // parameters, so there is no grant to write — a board cannot exist without an
    // owner, and no op can demote one.
    ops.push(Op::SetMemberName {
        member: owner,
        name: owner_name.to_owned(),
    });

    let mut rank = Rank::middle();
    for (column, title) in column_ids.iter().zip(DEFAULT_COLUMNS) {
        ops.push(Op::SetColumn {
            column: *column,
            title: title.to_owned(),
            rank: rank.clone(),
        });
        rank = Rank::after(&rank);
    }

    ops
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::board::Board;
    use crate::envelope::Stamp;
    use crate::envelope_state::EnvelopeState;
    use crate::params::BoardParameters;

    #[test]
    fn a_genesis_board_renders_with_named_owner_and_ordered_columns() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let owner = MemberId(key.verifying_key().to_bytes());
        let params = BoardParameters::new(owner, "Roadmap", [0; 16]);

        let column_ids: Vec<ColumnId> = (0..4u8).map(|i| ColumnId([i; 16])).collect();
        let signed: Vec<_> = genesis_ops(owner, "Ada", &column_ids)
            .into_iter()
            .enumerate()
            .map(|(i, op)| {
                let index = u8::try_from(i).expect("a genesis board has few enough ops");
                let stamp = Stamp::new(
                    params.scope(),
                    owner,
                    u64::from(index) + 1,
                    1_700_000_000_000,
                    [index; 16],
                );
                op.envelope(stamp).sign(&key)
            })
            .collect();

        let state = EnvelopeState::from_ops(signed);
        assert!(state.validate(params.scope(), params.owner).is_ok());

        let board = Board::from_state(&state, &params);
        assert_eq!(board.member_name(&owner), "Ada");
        assert_eq!(
            board
                .columns
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            DEFAULT_COLUMNS.to_vec(),
            "columns must render in the order they were laid out"
        );
    }
}
