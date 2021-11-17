//! VM host environment integration tests.
//!
//! You can enable logging with the `RUST_LOG` environment variable, e.g.:
//!
//! `RUST_LOG=debug cargo test`.
//!
//! Because the test runner captures the standard output from the test, this
//! will only print logging from failed tests. To avoid that, use the
//! `--nocapture` argument. Also, because by default the tests run in parallel,
//! it is better to select a single test, e.g.:
//!
//! `RUST_LOG=debug cargo test test_tx_read_write -- --nocapture`
pub mod ibc;
pub mod tx;
pub mod vp;

#[cfg(test)]
mod tests {

    use std::panic;

    use anoma::ledger::ibc::init_genesis_storage;
    use anoma::ledger::ibc::vp::Error as IbcError;
    use anoma::proto::Tx;
    use anoma::types::key::ed25519::SignedTxData;
    use anoma::types::storage::{self, Key, KeySeg};
    use anoma::types::time::DateTimeUtc;
    use anoma::types::{address, key};
    use anoma_vm_env::tx_prelude::{
        BorshDeserialize, BorshSerialize, KeyValIterator,
    };
    use anoma_vm_env::vp_prelude::{PostKeyValIterator, PreKeyValIterator};
    use itertools::Itertools;
    use test_env_log::test;

    use super::ibc;
    use super::tx::*;
    use super::vp::*;

    // paths to the WASMs used for tests
    const VP_ALWAYS_TRUE_WASM: &str = "../wasm_for_tests/vp_always_true.wasm";
    const VP_ALWAYS_FALSE_WASM: &str = "../wasm_for_tests/vp_always_false.wasm";

    #[test]
    fn test_tx_read_write() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        let key = "key";
        let read_value: Option<String> = tx_host_env::read(key);
        assert_eq!(
            None, read_value,
            "Trying to read a key that doesn't exists shouldn't find any value"
        );

        // Write some value
        let value = "test".repeat(4);
        tx_host_env::write(key, value.clone());

        let read_value: Option<String> = tx_host_env::read(key);
        assert_eq!(
            Some(value),
            read_value,
            "After a value has been written, we should get back the same \
             value when we read it"
        );

        let value = vec![1_u8; 1000];
        tx_host_env::write(key, value.clone());
        let read_value: Option<Vec<u8>> = tx_host_env::read(key);
        assert_eq!(
            Some(value),
            read_value,
            "Writing to an existing key should override the previous value"
        );
    }

    #[test]
    fn test_tx_has_key() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        let key = "key";
        assert!(
            !tx_host_env::has_key(key),
            "Before a key-value is written, its key shouldn't be found"
        );

        // Write some value
        let value = "test".to_string();
        tx_host_env::write(key, value);

        assert!(
            tx_host_env::has_key(key),
            "After a key-value has been written, its key should be found"
        );
    }

    #[test]
    fn test_tx_delete() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        let test_account = address::testing::established_address_1();
        env.spawn_accounts([&test_account]);
        init_tx_env(&mut env);

        // Trying to delete a key that doesn't exists should be a no-op
        let key = "key";
        tx_host_env::delete(key);

        let value = "test".to_string();
        tx_host_env::write(key, value);
        assert!(
            tx_host_env::has_key(key),
            "After a key-value has been written, its key should be found"
        );

        // Then delete it
        tx_host_env::delete(key);

        assert!(
            !tx_host_env::has_key(key),
            "After a key has been deleted, its key shouldn't be found"
        );

        // Trying to delete a validity predicate should fail
        let key = storage::Key::validity_predicate(&test_account).to_string();
        assert!(
            panic::catch_unwind(|| { tx_host_env::delete(key) })
                .err()
                .map(|a| a.downcast_ref::<String>().cloned().unwrap())
                .unwrap()
                .contains("CannotDeleteVp")
        );
    }

    #[test]
    fn test_tx_iter_prefix() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        let iter: KeyValIterator<Vec<u8>> = tx_host_env::iter_prefix("empty");
        assert_eq!(
            iter.count(),
            0,
            "Trying to iter a prefix that doesn't have any matching keys \
             should yield an empty iterator."
        );

        // Write some values directly into the storage first
        let prefix = Key::parse("prefix").unwrap();
        for i in 0..10_i32 {
            let key = prefix.join(&Key::parse(i.to_string()).unwrap());
            let value = i.try_to_vec().unwrap();
            env.storage.write(&key, value).unwrap();
        }
        env.storage.commit().unwrap();

        // Then try to iterate over their prefix
        let iter: KeyValIterator<i32> =
            tx_host_env::iter_prefix(prefix.to_string());
        let expected = (0..10).map(|i| (format!("{}/{}", prefix, i), i));
        itertools::assert_equal(iter.sorted(), expected.sorted());
    }

    #[test]
    fn test_tx_insert_verifier() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        assert!(env.verifiers.is_empty(), "pre-condition");
        let verifier = address::testing::established_address_1();
        tx_host_env::insert_verifier(&verifier);
        assert!(
            env.verifiers.contains(&verifier),
            "The verifier should have been inserted"
        );
        assert_eq!(
            env.verifiers.len(),
            1,
            "There should be only one verifier inserted"
        );
    }

    #[test]
    #[should_panic]
    fn test_tx_init_account_with_invalid_vp() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        let code = vec![];
        tx_host_env::init_account(code);
    }

    #[test]
    fn test_tx_init_account_with_valid_vp() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        let code =
            std::fs::read(VP_ALWAYS_TRUE_WASM).expect("cannot load wasm");
        tx_host_env::init_account(code);
    }

    #[test]
    fn test_tx_get_metadata() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        assert_eq!(tx_host_env::get_chain_id(), env.storage.get_chain_id().0);
        assert_eq!(
            tx_host_env::get_block_height(),
            env.storage.get_block_height().0
        );
        assert_eq!(
            tx_host_env::get_block_hash(),
            env.storage.get_block_hash().0
        );
        assert_eq!(
            tx_host_env::get_block_epoch(),
            env.storage.get_current_epoch().0
        );
    }

    /// An example how to write a VP host environment integration test
    #[test]
    fn test_vp_host_env() {
        // The environment must be initialized first
        let mut env = TestVpEnv::default();
        init_vp_env(&mut env);

        // We can add some data to the environment
        let key_raw = "key";
        let key = Key::parse(key_raw.to_string()).unwrap();
        let value = "test".to_string();
        let value_raw = value.try_to_vec().unwrap();
        env.write_log.write(&key, value_raw).unwrap();

        let read_pre_value: Option<String> = vp_host_env::read_pre(key_raw);
        assert_eq!(None, read_pre_value);
        let read_post_value: Option<String> = vp_host_env::read_post(key_raw);
        assert_eq!(Some(value), read_post_value);
    }

    #[test]
    fn test_vp_read_and_has_key() {
        let mut tx_env = TestTxEnv::default();

        let addr = address::testing::established_address_1();
        let addr_key = Key::from(addr.to_db_key());

        // Write some value to storage
        let existing_key =
            addr_key.join(&Key::parse("existing_key_raw").unwrap());
        let existing_key_raw = existing_key.to_string();
        let existing_value = vec![2_u8; 1000];
        // Values written to storage have to be encoded with Borsh
        let existing_value_encoded = existing_value.try_to_vec().unwrap();
        tx_env
            .storage
            .write(&existing_key, existing_value_encoded)
            .unwrap();

        // In a transaction, write override the existing key's value and add
        // another key-value
        let override_value = "override".to_string();
        let new_key =
            addr_key.join(&Key::parse("new_key").unwrap()).to_string();
        let new_value = "vp".repeat(4);

        // Initialize the VP environment via a transaction
        // The `_vp_env` MUST NOT be dropped until the end of the test
        let _vp_env = init_vp_env_from_tx(addr, tx_env, |_addr| {
            // Override the existing key
            tx_host_env::write(&existing_key_raw, &override_value);

            // Write the new key-value
            tx_host_env::write(&new_key, new_value.clone());
        });

        assert!(
            vp_host_env::has_key_pre(&existing_key_raw),
            "The existing key before transaction should be found"
        );
        let pre_existing_value: Option<Vec<u8>> =
            vp_host_env::read_pre(&existing_key_raw);
        assert_eq!(
            Some(existing_value),
            pre_existing_value,
            "The existing value read from state before transaction should be \
             unchanged"
        );

        assert!(
            !vp_host_env::has_key_pre(&new_key),
            "The new key before transaction shouldn't be found"
        );
        let pre_new_value: Option<Vec<u8>> = vp_host_env::read_pre(&new_key);
        assert_eq!(
            None, pre_new_value,
            "The new value read from state before transaction shouldn't yet \
             exist"
        );

        assert!(
            vp_host_env::has_key_post(&existing_key_raw),
            "The existing key after transaction should still be found"
        );
        let post_existing_value: Option<String> =
            vp_host_env::read_post(&existing_key_raw);
        assert_eq!(
            Some(override_value),
            post_existing_value,
            "The existing value read from state after transaction should be \
             overridden"
        );

        assert!(
            vp_host_env::has_key_post(&new_key),
            "The new key after transaction should be found"
        );
        let post_new_value: Option<String> = vp_host_env::read_post(&new_key);
        assert_eq!(
            Some(new_value),
            post_new_value,
            "The new value read from state after transaction should have a \
             value equal to the one written in the transaction"
        );
    }

    #[test]
    fn test_vp_iter_prefix() {
        let mut tx_env = TestTxEnv::default();

        let addr = address::testing::established_address_1();
        let addr_key = Key::from(addr.to_db_key());

        // Write some value to storage
        let prefix = addr_key.join(&Key::parse("prefix").unwrap());
        for i in 0..10_i32 {
            let key = prefix.join(&Key::parse(i.to_string()).unwrap());
            let value = i.try_to_vec().unwrap();
            tx_env.storage.write(&key, value).unwrap();
        }
        tx_env.storage.commit().unwrap();

        // In a transaction, write override the existing key's value and add
        // another key-value
        let existing_key = prefix.join(&Key::parse(5.to_string()).unwrap());
        let existing_key_raw = existing_key.to_string();
        let new_key = prefix.join(&Key::parse(11.to_string()).unwrap());
        let new_key_raw = new_key.to_string();

        // Initialize the VP environment via a transaction
        // The `_vp_env` MUST NOT be dropped until the end of the test
        let _vp_env = init_vp_env_from_tx(addr, tx_env, |_addr| {
            // Override one of the existing keys
            tx_host_env::write(&existing_key_raw, 100_i32);

            // Write the new key-value under the same prefix
            tx_host_env::write(&new_key_raw, 11.try_to_vec().unwrap());
        });

        let iter_pre: PreKeyValIterator<i32> =
            vp_host_env::iter_prefix_pre(prefix.to_string());
        let expected_pre = (0..10).map(|i| (format!("{}/{}", prefix, i), i));
        itertools::assert_equal(iter_pre.sorted(), expected_pre.sorted());

        let iter_post: PostKeyValIterator<i32> =
            vp_host_env::iter_prefix_post(prefix.to_string());
        let expected_post = (0..10).map(|i| {
            let val = if i == 5 { 100 } else { i };
            (format!("{}/{}", prefix, i), val)
        });
        itertools::assert_equal(iter_post.sorted(), expected_post.sorted());
    }

    #[test]
    fn test_vp_verify_tx_signature() {
        let mut env = TestVpEnv::default();

        let addr = address::testing::established_address_1();

        // Write the public key to storage
        let pk_key = key::ed25519::pk_key(&addr);
        let keypair = key::ed25519::testing::keypair_1();
        let pk = keypair.public.clone();
        env.storage
            .write(&pk_key, pk.try_to_vec().unwrap())
            .unwrap();

        // Use some arbitrary bytes for tx code
        let code = vec![4, 3, 2, 1, 0];
        for data in &[
            // Tx with some arbitrary data
            Some(vec![1, 2, 3, 4].repeat(10)),
            // Tx without any data
            None,
        ] {
            env.tx = Tx::new(code.clone(), data.clone()).sign(&keypair);
            // Initialize the environment
            init_vp_env(&mut env);

            let tx_data = env.tx.data.expect("data should exist");
            let signed_tx_data =
                match SignedTxData::try_from_slice(&tx_data[..]) {
                    Ok(data) => data,
                    _ => panic!("decoding failed"),
                };
            assert_eq!(&signed_tx_data.data, data);
            assert!(vp_host_env::verify_tx_signature(&pk, &signed_tx_data.sig));

            let other_keypair = key::ed25519::testing::keypair_2();
            assert!(!vp_host_env::verify_tx_signature(
                &other_keypair.public,
                &signed_tx_data.sig
            ));
        }
    }

    #[test]
    fn test_vp_get_metadata() {
        // The environment must be initialized first
        let mut env = TestVpEnv::default();
        init_vp_env(&mut env);

        assert_eq!(vp_host_env::get_chain_id(), env.storage.get_chain_id().0);
        assert_eq!(
            vp_host_env::get_block_height(),
            env.storage.get_block_height().0
        );
        assert_eq!(
            vp_host_env::get_block_hash(),
            env.storage.get_block_hash().0
        );
        assert_eq!(
            vp_host_env::get_block_epoch(),
            env.storage.get_current_epoch().0
        );
    }

    #[test]
    fn test_vp_eval() {
        // The environment must be initialized first
        let mut env = TestVpEnv::default();
        init_vp_env(&mut env);

        // evaluating without any code should fail
        let empty_code = vec![];
        let input_data = vec![];
        let result = vp_host_env::eval(empty_code, input_data);
        assert!(!result);

        // evaluating the VP template which always returns `true` should pass
        let code =
            std::fs::read(VP_ALWAYS_TRUE_WASM).expect("cannot load wasm");
        let input_data = vec![];
        let result = vp_host_env::eval(code, input_data);
        assert!(result);

        // evaluating the VP template which always returns `false` shouldn't
        // pass
        let code =
            std::fs::read(VP_ALWAYS_FALSE_WASM).expect("cannot load wasm");
        let input_data = vec![];
        let result = vp_host_env::eval(code, input_data);
        assert!(!result);
    }

    #[test]
    fn test_ibc_client() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);

        // Start an invalid transaction
        let data = ibc::client_creation_data();
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and increment the connection counter
        let counter_key = ibc::client_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no client counter");
        tx_host_env::write(&counter_key, counter + 1);
        let client_id = data.client_id(counter).expect("invalid client ID");
        // only insert a client type
        let client_type_key = ibc::client_type_key(&client_id).to_string();
        tx_host_env::write(&client_type_key, data.client_state.client_type());

        // Check should fail due to no client state
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(matches!(
            ibc_vp
                .validate(&tx_data)
                .expect_err("validation succeeded unexpectedly"),
            IbcError::ClientError(_),
        ));
        // drop the transaction
        env.write_log.drop_tx();

        // Start a transaction to create a new client
        let data = ibc::client_creation_data();
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // get and increment the connection counter
        let counter_key = ibc::client_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no client counter");
        tx_host_env::write(&counter_key, counter + 1);
        let client_id = data.client_id(counter).expect("invalid client ID");
        // client type
        let client_type_key = ibc::client_type_key(&client_id).to_string();
        tx_host_env::write(&client_type_key, data.client_state.client_type());
        // client state
        let client_state_key = ibc::client_state_key(&client_id).to_string();
        tx_host_env::write(&client_state_key, data.client_state.clone());
        // consensus state
        let height = data.client_state.latest_height();
        let consensus_state_key =
            ibc::consensus_state_key(&client_id, height).to_string();
        tx_host_env::write(&consensus_state_key, data.consensus_state.clone());
        let event = ibc::make_create_client_event(&client_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start an invalid transaction
        let data = ibc::client_update_data(client_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and update the client without a header
        let client_id = data.client_id.clone();
        // update the client with the same state
        let old_data = ibc::client_creation_data();
        let same_client_state = old_data.client_state.clone();
        let height = same_client_state.latest_height();
        let same_consensus_state = old_data.consensus_state;
        let client_state_key = ibc::client_state_key(&client_id).to_string();
        tx_host_env::write(&client_state_key, same_client_state);
        let consensus_state_key =
            ibc::consensus_state_key(&client_id, height).to_string();
        tx_host_env::write(&consensus_state_key, same_consensus_state);
        let event = ibc::make_update_client_event(&client_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check should fail due to the invalid updating
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(matches!(
            ibc_vp
                .validate(&tx_data)
                .expect_err("validation succeeded unexpectedly"),
            IbcError::ClientError(_),
        ));
        // drop the transaction
        env.write_log.drop_tx();

        // Start a transaction to update the client
        let data = ibc::client_update_data(client_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and update the client
        let client_id = data.client_id.clone();
        let client_state_key = ibc::client_state_key(&client_id).to_string();
        let client_state =
            tx_host_env::read(&client_state_key).expect("no client state");
        let (new_client_state, new_consensus_state) =
            ibc::update_client(client_state, data.headers.clone())
                .expect("updating a client failed");
        let height = new_client_state.latest_height();
        tx_host_env::write(&client_state_key, new_client_state);
        let consensus_state_key =
            ibc::consensus_state_key(&client_id, height).to_string();
        tx_host_env::write(&consensus_state_key, new_consensus_state);
        let event = ibc::make_update_client_event(&client_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start a transaction to upgrade the client
        let data = ibc::client_upgrade_data(client_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // upgrade the client
        let client_id = data.client_id.clone();
        let new_client_state = data.client_state.clone();
        let new_consensus_state = data.consensus_state.clone();
        let client_state_key = ibc::client_state_key(&client_id).to_string();
        let height = new_client_state.latest_height();
        let consensus_state_key =
            ibc::consensus_state_key(&client_id, height).to_string();
        tx_host_env::write(&client_state_key, new_client_state);
        tx_host_env::write(&consensus_state_key, new_consensus_state);
        let event = ibc::make_upgrade_client_event(&client_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_connection_init_and_open() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);
        // block header
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, client_state, writes) = ibc::prepare_client();
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start an invalid transaction
        let data = ibc::connection_open_init_data(client_id.clone());
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and increment the connection counter
        let counter_key = ibc::connection_counter_key().to_string();
        let counter: u64 =
            tx_host_env::read(&counter_key).expect("no connection counter");
        tx_host_env::write(&counter_key, counter + 1);
        // insert a new opened connection
        let conn_id = ibc::connection_id(counter);
        let conn_key = ibc::connection_key(&conn_id).to_string();
        let conn = ibc::open_connection(&mut data.connection());
        tx_host_env::write(&conn_key, conn);
        let event = ibc::make_open_init_connection_event(&conn_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check should fail due to directly opening a connection
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(matches!(
            ibc_vp
                .validate(&tx_data)
                .expect_err("validation succeeded unexpectedly"),
            IbcError::ConnectionError(_),
        ));
        // drop the transaction
        env.write_log.drop_tx();

        // Start a transaction for ConnectionOpenInit
        // tx (Not need to decode tx_data)
        let data = ibc::connection_open_init_data(client_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and increment the connection counter
        let counter_key = ibc::connection_counter_key().to_string();
        let counter: u64 =
            tx_host_env::read(&counter_key).expect("no connection counter");
        tx_host_env::write(&counter_key, counter + 1);
        // new connection
        let conn_id = ibc::connection_id(counter);
        let conn_key = ibc::connection_key(&conn_id).to_string();
        tx_host_env::write(&conn_key, data.connection());
        let event = ibc::make_open_init_connection_event(&conn_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();
        // set a block header again
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Start the next transaction for ConnectionOpenAck
        // tx (Not need to decode tx_data)
        let data = ibc::connection_open_ack_data(conn_id, client_state);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // update the connection
        let conn_key = ibc::connection_key(&data.conn_id).to_string();
        let mut conn = tx_host_env::read(&conn_key).expect("no connection");
        ibc::open_connection(&mut conn);
        tx_host_env::write(&conn_key, conn);
        let event = ibc::make_open_ack_connection_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_connection_try_and_open() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);
        // block header
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, client_state, writes) = ibc::prepare_client();
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction for ConnectionOpenTry
        // tx (Not need to decode tx_data)
        let data = ibc::connection_open_try_data(client_id, client_state);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // get and increment the connection counter
        let counter_key = ibc::connection_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no connection counter");
        tx_host_env::write(&counter_key, counter + 1);
        // new connection
        let conn_id = ibc::connection_id(counter);
        let conn_key = ibc::connection_key(&conn_id).to_string();
        tx_host_env::write(&conn_key, data.connection());
        let event = ibc::make_open_try_connection_event(&conn_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();
        // set a block header again
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Start the next transaction for ConnectionOpenConfirm
        // tx (Not need to decode tx_data)
        let data = ibc::connection_open_confirm_data(conn_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // update the connection
        let conn_key = ibc::connection_key(&data.conn_id).to_string();
        let mut conn = tx_host_env::read(&conn_key).expect("no connection");
        ibc::open_connection(&mut conn);
        tx_host_env::write(&conn_key, conn);
        let event = ibc::make_open_confirm_connection_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_channel_init_and_open() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start an invalid transaction
        let port_id = ibc::port_id("test_port").expect("invalid port ID");
        let data =
            ibc::channel_open_init_data(port_id.clone(), conn_id.clone());
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // not bind a port
        // get and increment the channel counter
        let counter_key = ibc::channel_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no channel counter");
        tx_host_env::write(&counter_key, counter + 1);
        // channel
        let channel_id = ibc::channel_id(counter);
        let port_channel_id = ibc::port_channel_id(port_id, channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        tx_host_env::write(&channel_key, data.channel());
        let event = ibc::make_open_init_channel_event(&channel_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check should fail due to no port binding
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(matches!(
            ibc_vp
                .validate(&tx_data)
                .expect_err("validation succeeded unexpectedly"),
            IbcError::ChannelError(_),
        ));
        // drop the transaction
        env.write_log.drop_tx();

        // Start an invalid transaction
        let port_id = ibc::port_id("test_port").expect("invalid port ID");
        let data =
            ibc::channel_open_init_data(port_id.clone(), conn_id.clone());
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // bind a port
        let index_key = ibc::capability_index_key().to_string();
        let cap_index: u64 = tx_host_env::read(&index_key).expect("no index");
        tx_host_env::write(&index_key, cap_index + 1);
        let port_key = ibc::port_key(&port_id).to_string();
        tx_host_env::write(&port_key, cap_index);
        let cap_key = ibc::capability_key(cap_index).to_string();
        tx_host_env::write(&cap_key, port_id.clone());
        // get and increment the channel counter
        let counter_key = ibc::channel_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no channel counter");
        tx_host_env::write(&counter_key, counter + 1);
        // insert a opened channel
        let channel_id = ibc::channel_id(counter);
        let port_channel_id = ibc::port_channel_id(port_id, channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let channel = ibc::open_channel(&mut data.channel());
        tx_host_env::write(&channel_key, channel);
        let event = ibc::make_open_init_channel_event(&channel_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check should fail due to directly opening a channel
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(matches!(
            ibc_vp
                .validate(&tx_data)
                .expect_err("validation succeeded unexpectedly"),
            IbcError::ChannelError(_),
        ));
        // drop the transaction
        env.write_log.drop_tx();

        // Start a transaction for ChannelOpenInit
        // tx (Not need to decode tx_data)
        let port_id = ibc::port_id("test_port").expect("invalid port ID");
        let data = ibc::channel_open_init_data(port_id.clone(), conn_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // bind a port
        let index_key = ibc::capability_index_key().to_string();
        let cap_index: u64 = tx_host_env::read(&index_key).expect("no index");
        tx_host_env::write(&index_key, cap_index + 1);
        let port_key = ibc::port_key(&port_id).to_string();
        tx_host_env::write(&port_key, cap_index);
        let cap_key = ibc::capability_key(cap_index).to_string();
        tx_host_env::write(&cap_key, port_id.clone());
        // get and increment the channel counter
        let counter_key = ibc::channel_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no channel counter");
        tx_host_env::write(&counter_key, counter + 1);
        // channel
        let channel_id = ibc::channel_id(counter);
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        tx_host_env::write(&channel_key, data.channel());
        let event = ibc::make_open_init_channel_event(&channel_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start the next transaction for ChannelOpenAck
        // tx (Not need to decode tx_data)
        let data = ibc::channel_open_ack_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // update the channel
        let port_channel_id =
            ibc::port_channel_id(data.port_id.clone(), data.channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::open_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        let event = ibc::make_open_ack_channel_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_channel_try_and_open() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction for ChannelOpenTry
        // tx (Not need to decode tx_data)
        let port_id = ibc::port_id("test_port").expect("invalid port ID");
        let data = ibc::channel_open_try_data(port_id.clone(), conn_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // bind a port
        let index_key = ibc::capability_index_key().to_string();
        let cap_index: u64 = tx_host_env::read(&index_key).expect("no index");
        tx_host_env::write(&index_key, cap_index + 1);
        let port_key = ibc::port_key(&port_id).to_string();
        tx_host_env::write(&port_key, cap_index);
        let cap_key = ibc::capability_key(cap_index).to_string();
        tx_host_env::write(&cap_key, port_id.clone());
        // get and increment the connection counter
        let counter_key = ibc::channel_counter_key().to_string();
        let counter =
            tx_host_env::read(&counter_key).expect("no channel counter");
        tx_host_env::write(&counter_key, counter + 1);
        // channel
        let channel_id = ibc::channel_id(counter);
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        tx_host_env::write(&channel_key, data.channel());
        let event = ibc::make_open_try_channel_event(&channel_id, &data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start the next transaction for ChannelOpenConfirm
        // tx (Not need to decode tx_data)
        let data = ibc::channel_open_confirm_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // update the channel
        let port_channel_id =
            ibc::port_channel_id(data.port_id.clone(), data.channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::open_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        let event = ibc::make_open_confirm_channel_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_channel_close_init() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to close the channel
        // tx (Not need to decode tx_data)
        let data = ibc::channel_close_init_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // close the channel
        let port_channel_id =
            ibc::port_channel_id(data.port_id.clone(), data.channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::close_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        let event = ibc::make_close_init_channel_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_channel_close_confirm() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to close the channel
        // tx (Not need to decode tx_data)
        let data = ibc::channel_close_confirm_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // close the channel
        let port_channel_id =
            ibc::port_channel_id(data.port_id.clone(), data.channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::close_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        let event = ibc::make_close_confirm_channel_event(&data);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_send_packet() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to send a packet
        // tx (Not need to decode tx_data)
        let data = ibc::packet_send_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // increment nextSequenceSend
        let port_id = data.source_port.clone();
        let channel_id = data.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let next_seq_send_key =
            ibc::next_sequence_send_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_send_key).unwrap_or(1);
        tx_host_env::write(&next_seq_send_key, seq_index + 1);
        // make a packet from the given data
        let packet = data.packet(ibc::sequence(seq_index));
        // commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        let commitment = ibc::commitment(&packet);
        tx_host_env::write(&commitment_key, commitment);
        let event = ibc::make_send_packet_event(packet.clone());
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // the transaction does something before senging a packet

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start the next transaction for receiving an ack
        // tx (Not need to decode tx_data)
        let data = ibc::packet_ack_data(packet);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // delete the commitment
        let packet = data.packet;
        let port_id = packet.source_port.clone();
        let channel_id = packet.source_channel.clone();
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        tx_host_env::delete(&commitment_key);
        // increment nextSequenceAck
        let next_seq_ack_key =
            ibc::next_sequence_ack_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_ack_key).unwrap_or(1);
        tx_host_env::write(&next_seq_ack_key, seq_index + 1);
        let event = ibc::make_ack_event(packet);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // the transaction does something after the ack

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_receive_packet() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);
        // block header to check timeout timestamp
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // packet
        let packet =
            ibc::received_packet(port_id, channel_id, ibc::sequence(1));

        // Start a transaction to receive a packet
        // tx (Not need to decode tx_data)
        let data = ibc::packet_receipt_data(packet);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // increment nextSequenceRecv
        let port_id = data.packet.destination_port;
        let channel_id = data.packet.destination_channel;
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let next_seq_recv_key =
            ibc::next_sequence_recv_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_recv_key).unwrap_or(1);
        tx_host_env::write(&next_seq_recv_key, seq_index + 1);
        // receipt
        let receipt_key =
            ibc::receipt_key(&port_id, &channel_id, data.packet.sequence)
                .to_string();
        tx_host_env::write(&receipt_key, 0_u64);
        // ack
        let ack_key = ibc::ack_key(&port_id, &channel_id, data.packet.sequence)
            .to_string();
        tx_host_env::write(&ack_key, "ack".try_to_vec().unwrap());

        // the transaction does something according to the packet

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_send_packet_unordered() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to send a packet
        // tx (Not need to decode tx_data)
        let data = ibc::packet_send_data(port_id, channel_id);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // increment nextSequenceSend
        let port_id = data.source_port.clone();
        let channel_id = data.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let next_seq_send_key =
            ibc::next_sequence_send_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_send_key).unwrap_or(1);
        tx_host_env::write(&next_seq_send_key, seq_index + 1);
        // make a packet from the given data
        let packet = data.packet(ibc::sequence(seq_index));
        // commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        let commitment = ibc::commitment(&packet);
        tx_host_env::write(&commitment_key, commitment);
        let event = ibc::make_send_packet_event(packet.clone());
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // the transaction does something before senging a packet

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start the next transaction for receiving an ack
        // tx (Not need to decode tx_data)
        let data = ibc::packet_ack_data(packet);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // delete the commitment
        let packet = data.packet;
        let port_id = packet.source_port;
        let channel_id = packet.source_channel;
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        tx_host_env::delete(&commitment_key);
        // increment nextSequenceAck
        let next_seq_ack_key =
            ibc::next_sequence_ack_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_ack_key).unwrap_or(1);
        tx_host_env::write(&next_seq_ack_key, seq_index + 1);

        // the transaction does something after the ack

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_receive_packet_unordered() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);
        // block header to check timeout timestamp
        env.storage.set_header(ibc::tm_dummy_header()).unwrap();

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // packet (sequence number isn't checked for the unordered channel)
        let packet =
            ibc::received_packet(port_id, channel_id, ibc::sequence(100));

        // Start a transaction to receive a packet
        // tx (Not need to decode tx_data)
        let data = ibc::packet_receipt_data(packet);
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };
        // not need to increment nextSequenceRecv
        // receipt
        let port_id = data.packet.destination_port.clone();
        let channel_id = data.packet.destination_channel.clone();
        let receipt_key =
            ibc::receipt_key(&port_id, &channel_id, data.packet.sequence)
                .to_string();
        tx_host_env::write(&receipt_key, 0_u64);
        // ack
        let ack_key = ibc::ack_key(&port_id, &channel_id, data.packet.sequence)
            .to_string();
        tx_host_env::write(&ack_key, "ack".try_to_vec().unwrap());

        // the transaction does something according to the packet

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_packet_timeout() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to send a packet
        // tx (Not need to decode tx_data)
        let mut data = ibc::packet_send_data(port_id, channel_id);
        ibc::set_timeout_height(&mut data);
        // increment nextSequenceSend
        let port_id = data.source_port.clone();
        let channel_id = data.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let next_seq_send_key =
            ibc::next_sequence_send_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_send_key).unwrap_or(1);
        tx_host_env::write(&next_seq_send_key, seq_index + 1);
        // make a packet from the given data
        let packet = data.packet(ibc::sequence(seq_index));
        // commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        let commitment = ibc::commitment(&packet);
        tx_host_env::write(&commitment_key, commitment);
        let event = ibc::make_send_packet_event(packet.clone());
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start a transaction to notify the timeout
        let data = ibc::timeout_data(packet, ibc::sequence(1));
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // close the channel
        let packet = data.packet;
        let port_id = packet.source_port.clone();
        let channel_id = packet.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::close_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        // delete the commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        tx_host_env::delete(&commitment_key);
        let event = ibc::make_timeout_event(packet);
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }

    #[test]
    fn test_ibc_timeout_on_close() {
        // The environment must be initialized first
        let mut env = TestTxEnv::default();
        init_tx_env(&mut env);

        // Set the initial state before starting transactions
        init_genesis_storage(&mut env.storage);
        let (client_id, _client_state, mut writes) = ibc::prepare_client();
        let (conn_id, conn_writes) = ibc::prepare_opened_connection(&client_id);
        writes.extend(conn_writes);
        let (port_id, channel_id, channel_writes) =
            ibc::prepare_opened_channel(&conn_id);
        writes.extend(channel_writes);
        writes.into_iter().for_each(|(key, val)| {
            env.storage.write(&key, val).expect("write error");
        });

        // Start a transaction to send a packet
        // tx (Not need to decode tx_data)
        let data = ibc::packet_send_data(port_id, channel_id);
        // increment nextSequenceSend
        let port_id = data.source_port.clone();
        let channel_id = data.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let next_seq_send_key =
            ibc::next_sequence_send_key(&port_channel_id).to_string();
        let seq_index: u64 = tx_host_env::read(&next_seq_send_key).unwrap_or(1);
        tx_host_env::write(&next_seq_send_key, seq_index + 1);
        // make a packet from the given data (not timeout)
        let packet = data.packet(ibc::sequence(seq_index));
        // commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        let commitment = ibc::commitment(&packet);
        tx_host_env::write(&commitment_key, commitment);
        let event = ibc::make_send_packet_event(packet.clone());
        tx_host_env::emit_ibc_event(&event.try_into().unwrap());

        // Commit
        env.write_log.commit_tx();
        env.write_log.commit_block(&mut env.storage).unwrap();

        // Start a transaction to notify the timing-out on closed
        let data = ibc::timeout_data(packet, ibc::sequence(1));
        let tx_data = data.try_to_vec().expect("encoding failed");
        let tx = Tx {
            code: vec![],
            data: Some(tx_data.clone()),
            timestamp: DateTimeUtc::now(),
        };

        // close the channel
        let packet = data.packet;
        let port_id = packet.source_port.clone();
        let channel_id = packet.source_channel.clone();
        let port_channel_id =
            ibc::port_channel_id(port_id.clone(), channel_id.clone());
        let channel_key = ibc::channel_key(&port_channel_id).to_string();
        let mut channel = tx_host_env::read(&channel_key).expect("no channel");
        ibc::close_channel(&mut channel);
        tx_host_env::write(&channel_key, channel);
        // delete the commitment
        let commitment_key =
            ibc::commitment_key(&port_id, &channel_id, packet.sequence)
                .to_string();
        tx_host_env::delete(&commitment_key);

        // Check
        let ibc_vp = ibc::init_ibc_vp_from_tx(&env, &tx);
        assert!(
            ibc_vp
                .validate(&tx_data)
                .expect("validation failed unexpectedly")
        );
    }
}
