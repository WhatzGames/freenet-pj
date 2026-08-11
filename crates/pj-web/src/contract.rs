//! The wasm components this app carries: the board contract, the registry
//! contract, and the identity delegate.
//!
//! The frontend ships the code rather than just addresses because creating a
//! board is a PUT of *code plus parameters plus initial state* — parameters are
//! hashed with the code to form the instance id, so every board is its own
//! instance and the node needs the code in hand to instantiate one. The registry
//! needs the same treatment the first time anyone lists a board, and a delegate
//! has to be registered with the node before it will answer.
//!
//! The bytes are raw wasm, not fdev's packaged form: fdev's file is the same wasm
//! behind an 8-byte version tag and its blake3 hash, and `ContractCode` recomputes
//! that hash itself, so both routes yield identical keys.

use std::sync::Arc;

use freenet_stdlib::prelude::{
    CodeHash, ContractCode, ContractContainer, ContractInstanceId, ContractKey, Delegate,
    DelegateCode, DelegateContainer, DelegateKey, DelegateWasmAPIVersion, Parameters,
    WrappedContract,
};
use pj_core::{
    BoardParameters, OrgParameters, RegistryParameters, TaskAddr, TaskParameters, UserParameters,
};

/// All four are staged next to this crate by `scripts/build.sh`.
const BOARD_CONTRACT_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_board_contract.wasm"
));
const TASK_CONTRACT_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_task_contract.wasm"
));
const REGISTRY_CONTRACT_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_registry_contract.wasm"
));
const ORG_CONTRACT_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_org_contract.wasm"
));
const USER_CONTRACT_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_user_contract.wasm"
));
const IDENTITY_DELEGATE_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_identity_delegate.wasm"
));
const PREFS_DELEGATE_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contract/pj_prefs_delegate.wasm"
));

// ---------------------------------------------------------------- board

pub(crate) fn board_code() -> ContractCode<'static> {
    ContractCode::from(BOARD_CONTRACT_WASM.to_vec())
}

/// Shown in the UI so a mismatch between the board's governing contract and the
/// one this build carries is diagnosable rather than mysterious.
pub(crate) fn board_code_hash() -> String {
    board_code().hash_str()
}

pub(crate) fn board_container(params: &BoardParameters) -> ContractContainer {
    let parameters = Parameters::from(params.encode());
    ContractContainer::from(freenet_stdlib::prelude::ContractWasmAPIVersion::V1(
        WrappedContract::new(Arc::new(board_code()), parameters),
    ))
}

pub(crate) fn board_key(params: &BoardParameters) -> ContractKey {
    ContractKey::from_params_and_code(Parameters::from(params.encode()), board_code())
}

// ---------------------------------------------------------------- task

fn task_code() -> ContractCode<'static> {
    ContractCode::from(TASK_CONTRACT_WASM.to_vec())
}

pub(crate) fn task_container(params: &TaskParameters) -> ContractContainer {
    ContractContainer::from(freenet_stdlib::prelude::ContractWasmAPIVersion::V1(
        WrappedContract::new(Arc::new(task_code()), Parameters::from(params.encode())),
    ))
}

pub(crate) fn task_key(params: &TaskParameters) -> ContractKey {
    ContractKey::from_params_and_code(Parameters::from(params.encode()), task_code())
}

/// A task's address, which is also the whole of a link to it.
///
/// Derivable here because the code is carried by this build: given the
/// parameters, the address follows. That is what lets a pasted address be
/// fetched with nothing else alongside it.
pub(crate) fn task_addr(params: &TaskParameters) -> TaskAddr {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(task_key(params).id().as_bytes());
    TaskAddr(bytes)
}

/// The reverse: a pasted address, as something the node can be asked for.
pub(crate) fn task_instance(addr: TaskAddr) -> ContractInstanceId {
    ContractInstanceId::new(addr.0)
}

// ---------------------------------------------------------------- registry

fn registry_parameters() -> Parameters<'static> {
    Parameters::from(RegistryParameters::current().encode())
}

pub(crate) fn registry_container() -> ContractContainer {
    ContractContainer::from(freenet_stdlib::prelude::ContractWasmAPIVersion::V1(
        WrappedContract::new(
            Arc::new(ContractCode::from(REGISTRY_CONTRACT_WASM.to_vec())),
            registry_parameters(),
        ),
    ))
}

/// The directory's address. Constant parameters mean every build derives the same
/// one, which is what makes a shared registry possible at all on a network with no
/// enumeration: you cannot search for it, so its location has to be computable.
pub(crate) fn registry_key() -> ContractKey {
    ContractKey::from_params_and_code(
        registry_parameters(),
        ContractCode::from(REGISTRY_CONTRACT_WASM.to_vec()),
    )
}

pub(crate) fn registry_id() -> ContractInstanceId {
    *registry_key().id()
}

// ---------------------------------------------------------------- organization

pub(crate) fn org_container(params: &OrgParameters) -> ContractContainer {
    let parameters = Parameters::from(params.encode());
    ContractContainer::from(freenet_stdlib::prelude::ContractWasmAPIVersion::V1(
        WrappedContract::new(
            Arc::new(ContractCode::from(ORG_CONTRACT_WASM.to_vec())),
            parameters,
        ),
    ))
}

pub(crate) fn org_key(params: &OrgParameters) -> ContractKey {
    ContractKey::from_params_and_code(
        Parameters::from(params.encode()),
        ContractCode::from(ORG_CONTRACT_WASM.to_vec()),
    )
}

// ---------------------------------------------------------------- user profile

/// A person's profile lives at an address derived from their own public key, which
/// is what lets a client find its owner's profile with nothing but the key it
/// already holds — there is nothing to search on Freenet.
pub(crate) fn user_container(params: &UserParameters) -> ContractContainer {
    let parameters = Parameters::from(params.encode());
    ContractContainer::from(freenet_stdlib::prelude::ContractWasmAPIVersion::V1(
        WrappedContract::new(
            Arc::new(ContractCode::from(USER_CONTRACT_WASM.to_vec())),
            parameters,
        ),
    ))
}

pub(crate) fn user_key(params: &UserParameters) -> ContractKey {
    ContractKey::from_params_and_code(
        Parameters::from(params.encode()),
        ContractCode::from(USER_CONTRACT_WASM.to_vec()),
    )
}

// ---------------------------------------------------------------- delegate

pub(crate) fn delegate_container() -> DelegateContainer {
    let code = DelegateCode::from(IDENTITY_DELEGATE_WASM.to_vec());
    let params = Parameters::from(Vec::new());
    DelegateContainer::Wasm(DelegateWasmAPIVersion::V1(
        Delegate::from((&code, &params)).into_owned(),
    ))
}

/// Taken from the container rather than derived directly — `DelegateKey`'s own
/// `from_params_and_code` is private to the stdlib.
pub(crate) fn delegate_key() -> DelegateKey {
    delegate_container().key().clone()
}

/// The node-local preferences store.
///
/// A second delegate rather than more surface on the identity one: secrets hang off
/// `hash(code + parameters)`, so extending that delegate would have moved its key
/// and taken every user's signing seed with it. This one is new, so it has nothing
/// to lose, and it stores opaque bytes so that adding preferences never rebuilds it.
pub(crate) fn prefs_delegate_container() -> DelegateContainer {
    let code = DelegateCode::from(PREFS_DELEGATE_WASM.to_vec());
    let params = Parameters::from(Vec::new());
    DelegateContainer::Wasm(DelegateWasmAPIVersion::V1(
        Delegate::from((&code, &params)).into_owned(),
    ))
}

pub(crate) fn prefs_delegate_key() -> DelegateKey {
    prefs_delegate_container().key().clone()
}

/// Delegate keys of retired generations, newest last.
///
/// A delegate's key is `hash(code + parameters)` and every user's seed is stored
/// under that key, so a rebuilt delegate looks at an empty namespace and mints
/// fresh identities — silently costing people access to boards they own.
/// Registering with these as predecessors asks the node to copy the old secrets
/// forward, and appending to this list is a precondition for that.
///
/// **It does not work, and cannot be made to work from this side.** Tested to
/// destruction on 2026-07-25 against freenet 0.2.105, twice, with the node's log
/// as evidence:
///
/// ```text
/// delegate secret copy-forward: predecessor has no recorded origin
/// (NoProvenance); refusing (legacy data migrates via the app-side path)
/// … copy-forward completed predecessors=4 copied=0
/// ```
///
/// The node refuses to copy from any predecessor with no recorded *origin*, and
/// it has none for ours. That is not a legacy problem: generation 3 was
/// registered by this very app, on this node, minutes before generation 4 asked
/// to inherit from it, and was refused for the same reason. Registration requests
/// evidently do not carry the web-app origin that application messages do, so no
/// provenance is ever recorded and every predecessor is refused.
///
/// A separate real defect was found and fixed along the way — the node
/// acknowledges registration with an *empty* `DelegateResponse`, which this app
/// discarded, so the seed was requested before any copy could land. `store.rs`
/// now waits for it. That was worth fixing but was not the blocker.
///
/// So: **a delegate rebuild is destructive.** The recovery key is the migration
/// path today. The node's message names the real alternative — an app-side
/// migration, where the client reads its seed from the previous generation and
/// writes it into the new one. That is entirely within our control and is not yet
/// built.
///
/// Read the current key with:
/// `cargo test -p pj-identity-delegate -- --nocapture delegate_key`
const PREDECESSOR_DELEGATE_KEYS: &[&str] = &[
    // Generation 1: the delegate while it still depended on `pj-core`, so every
    // unrelated change there moved it. Split out into `pj-identity-proto` to stop
    // that happening again.
    "FB7Kf9yqsWXCF2dJ5V5RjuiH6hsd8FXSywW5DX2Bkuft",
    // Generation 2: after the protocol moved into `pj-identity-proto`.
    "7dxWQwLk2u5oSnpDkW6HEjEQwmE9YBeQy4JzKocJjRHk",
    // The key generation 2's wasm actually ended up at — the build drifted from
    // the recorded value (edition and dependency changes both move it), which is
    // its own argument for reading the key rather than assuming it. Every live
    // identity is stored here, so this is the one that has to carry forward.
    "5yfudAGx24ZJkrFBiaKj1b2GGrQsrUAURbJPRQbtptqZ",
    // Generation 3. Unlike the three above, this one was registered by this app on
    // a node that records provenance, so it is the first predecessor the node has
    // any hope of copying from.
    "D1rboRBawJthaugLcuVmUwtkzNvs3QHab6TYFxJ9yfw9",
];

pub(crate) fn predecessor_delegate_keys() -> Vec<DelegateKey> {
    PREDECESSOR_DELEGATE_KEYS
        .iter()
        // A key that does not decode is a typo in the list above, not something to
        // fail the whole registration over: the node ignores predecessors it does
        // not hold anyway.
        .filter_map(|encoded| {
            let mut key = [0u8; 32];
            let written = bs58::decode(encoded).onto(&mut key).ok()?;
            if written != 32 {
                return None;
            }
            // The code hash is not recoverable from an encoded key — `encode()`
            // covers only the 32-byte namespace id — but it does not need to be:
            // a delegate's secret namespace is `From<DelegateKey> for SecretsId`,
            // which reads that id alone.
            Some(DelegateKey::new(key, CodeHash::new([0; 32])))
        })
        .collect()
}
