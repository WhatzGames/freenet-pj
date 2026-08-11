//! Contract parameters: the immutable identity of a board.

use serde::{Deserialize, Serialize};

use crate::envelope::Scope;
use crate::error::{Error, Result};
use crate::ids::MemberId;

/// The parameters a board contract instance is created with.
///
/// A Freenet contract instance id is the hash of the contract code plus its
/// parameters, so these fields decide *which* board you are talking to. They can
/// never change: editing them produces a different instance. That is exactly the
/// property we want for the owner key, which is the root of the board's
/// permission model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardParameters {
    /// The board owner's public key. The only key whose membership ops the
    /// contract honours, which makes it the root of trust for the board.
    pub owner: MemberId,
    /// Display name. Baked into the instance id, so renaming a board is not
    /// possible — it is a deliberate trade for having the name be tamper-proof.
    pub name: String,
    /// Random, so that the same owner can create two boards with the same name
    /// and still get distinct contract instances.
    pub salt: [u8; 16],
}

impl BoardParameters {
    pub fn new(owner: MemberId, name: impl Into<String>, salt: [u8; 16]) -> Self {
        Self {
            owner,
            name: name.into(),
            salt,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::serialize(self).expect("BoardParameters is always serializable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        bincode::deserialize(bytes).map_err(|e| Error::decode("BoardParameters", e))
    }

    /// What every op written to this board is signed against, so that one signed
    /// here cannot be replayed onto another board or into a profile. See [`Scope`].
    pub fn scope(&self) -> Scope {
        Scope::of(&self.encode())
    }
}
