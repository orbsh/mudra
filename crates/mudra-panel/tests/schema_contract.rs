#[cfg(test)]
mod tests {
    //! Byte-level contract tests (native target): the panel's frame
    //! encodings must be byte-identical to the engine-side assembly —
    //! that is the whole point of sharing mudra-store's derives.

    use mudra_panel::schema;
    use okm_core::Document;

    #[test]
    fn state_key_equals_collection_primary_key() {
        // the engine-side truth: Collection::primary_key (document.rs),
        // same assembly path mudrad's writes take
        let dir = tempfile::tempdir().expect("tmpdir");
        let store =
            mudra_store::MudraStore::open(&dir.path().join("store")).expect("open store");
        for slot in [
            mudra_store::state::CURRENT_CONTEXT,
            mudra_store::state::EPOCH,
            mudra_store::state::PAGE_ID,
        ] {
            let engine_key = store.state.primary_key(&mudra_store::StateKey { slot });
            let panel_key = schema::state_key(slot);
            assert_eq!(
                engine_key, panel_key,
                "panel key for slot {slot} must match Collection::primary_key byte-for-byte"
            );
        }
    }

    #[test]
    fn state_prefix_is_the_table_header() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let store =
            mudra_store::MudraStore::open(&dir.path().join("store")).expect("open store");
        // the full-range scan prefix is exactly the two-byte ns header
        assert_eq!(
            schema::state_prefix(),
            vec![0x00, 0x06],
            "State ns 6 per SCHEMA (big-endian header)"
        );
        assert!(store.state.primary_key(&mudra_store::StateKey { slot: 1 }).starts_with(&schema::state_prefix()));
    }

    #[test]
    fn decode_state_text_reads_the_written_payload() {
        // round trip through the engine's own encoder: what mudrad stores,
        // the frame carries as bytes, the panel decodes with the same type
        let written = mudra_store::State {
            value: okm_core::Bytes(b"inbox".to_vec()),
        };
        let bytes = written.encode_payload();
        assert_eq!(schema::decode_state_text(&bytes), "inbox");
    }
}
