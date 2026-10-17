use super::*;

#[tokio::test]
async fn equal_sequence_reset_conflicts_retire_true_once_and_keep_false_bytes() {
    for (raw_entered, reverse) in [(true, false), (true, true), (false, false), (false, true)] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let path = dir.path().join("equal-reset-conflicts.db");
        let mut state = disk_state(state, &path);
        let mut local = super::sequence_recovery::signed_floor(&state, 10);
        local.record.entered = false;
        local.signature = state.sign_payload(&local.record).unwrap();
        let registry = OwnSpacePresence {
            networks: BTreeMap::from([(
                local.record.network_id.clone(),
                BTreeMap::from([(local.record.space_id.clone(), local.clone())]),
            )]),
            active_space: None,
        };
        state
            .local_db
            .save_domain("own_space_presence", &registry)
            .unwrap();
        state.own_space_presence = Arc::new(Mutex::new(registry));
        let mut remote_false = local.clone();
        remote_false.record.activity = "different inactive record".to_owned();
        remote_false.signature = state.sign_payload(&remote_false.record).unwrap();
        let mut remote_other = local.clone();
        remote_other.record.entered = raw_entered;
        if !raw_entered {
            remote_other.record.activity = "another inactive record".to_owned();
        }
        remote_other.signature = state.sign_payload(&remote_other.record).unwrap();
        let mut raw = vec![remote_false, remote_other];
        if reverse {
            raw.reverse();
        }
        let mut servers = Vec::new();
        for record in raw {
            let (url, server) = super::sequence_recovery::floor_server(record).await;
            state.gateway_urls.push(url);
            servers.push(server);
        }
        let conn = rusqlite::Connection::open(&path).unwrap();
        if !raw_entered {
            conn.execute_batch("CREATE TRIGGER forbid_equal_reset_write BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'equal false record rewritten'); END;").unwrap();
        }
        let mut events = state.stream_tx.subscribe();
        crate::reset_space_presence(&state).await.unwrap();
        let expected = if raw_entered {
            let inactive: SignedSpacePresence = serde_json::from_value(
                events
                    .try_recv()
                    .expect("equal sequence conflicting true must be retired")
                    .payload,
            )
            .unwrap();
            let mut expected = local.record.clone();
            expected.seq = 11;
            expected.updated_at = inactive.record.updated_at;
            assert_eq!(inactive.record, expected);
            assert!(
                verify_payload(&inactive.record, &inactive.signature, &inactive.public_key)
                    .unwrap()
            );
            assert_ne!(inactive.signature, local.signature);
            conn.execute_batch("CREATE TRIGGER forbid_equal_reset_write BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'equal conflict retired twice'); END;").unwrap();
            inactive
        } else {
            local.clone()
        };
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let before = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        for _ in 0..3 {
            crate::reset_space_presence(&state).await.unwrap();
            crate::reconcile_space_presence(&state).await.unwrap();
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                before
            );
            let registry = state.own_space_presence.lock().await;
            assert_eq!(
                registry.stored(&local.record.network_id, &local.record.space_id),
                Some(&expected)
            );
            assert!(registry.active_space.is_none());
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        }
        for server in servers {
            server.abort();
        }
    }
}
