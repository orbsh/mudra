//! Schema byte helpers for the frame sender: the panel never restates a
//! layout — every byte comes from `mudra-store`'s shared types (the
//! derives are the single source; mudrad's collections write exactly
//! these keys). Slice 1 covers the State table end to end; slice 2 adds
//! Tag/Page/junction helpers as their views land.

use okm_core::Document;

/// Scan prefix for the whole State primary range (prefix scan over the
/// table header, legacy OP_SCAN grammar = empty value segment).
pub fn state_prefix() -> Vec<u8> {
    mudra_store::primary_header::<mudra_store::State>()
}

/// Full State primary key for one slot — delegates to the shared
/// assembly (`mudra_store::primary_key`), so the panel's bytes and
/// mudrad's `Collection::primary_key` cannot drift.
pub fn state_key(slot: u8) -> Vec<u8> {
    mudra_store::primary_key::<mudra_store::State>(&mudra_store::StateKey { slot })
}

/// Decode a State row's payload bytes, then read the slot's text value.
/// Raw bytes cross the frame; the slot decides interpretation (SCHEMA's
/// state contract), exactly like mudrad's `state_text`.
pub fn decode_state_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&mudra_store::State::decode_payload(bytes).value.0).into_owned()
}
