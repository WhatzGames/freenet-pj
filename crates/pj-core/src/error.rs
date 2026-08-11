//! Error type shared by the domain model and the contract.

use crate::ids::{MemberId, OpId};

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not decode {what}: {detail}")]
    Decode { what: &'static str, detail: String },

    #[error("op {id} has an invalid signature for author {author}")]
    BadSignature { id: OpId, author: MemberId },

    #[error("op {id} was signed by {author}, who is not a member of this board")]
    Unauthorized { id: OpId, author: MemberId },

    #[error("op {id} was signed for a different contract and does not belong here")]
    MisdirectedOp { id: OpId },

    #[error("{0} is not a valid ed25519 public key")]
    BadKey(MemberId),

    #[error("{0:?} is not a valid rank")]
    BadRank(String),
}

impl Error {
    pub(crate) fn decode(what: &'static str, detail: impl core::fmt::Display) -> Self {
        Error::Decode {
            what,
            detail: detail.to_string(),
        }
    }
}
