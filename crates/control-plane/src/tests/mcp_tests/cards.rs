use super::*;
use crate::routes::board::BOARD_BATTLE_FEED_KEY;
use crate::routes::cards::resend_due_battle_notices;
use axum::extract::{Path as AxumPath, Query as AxumQuery, State as AxumState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// What the mock gateway was sent, and the match it shows.
#[derive(Clone, Default)]
struct MockCards {
    actions: Arc<Mutex<Vec<Value>>>,
    reads: Arc<Mutex<Vec<(String, Value)>>>,
    /// The match as `GET /api/cards/matches/{id}` and every command reply show it; `null` is
    /// no such match.
    game: Arc<Mutex<Value>>,
    /// `notify` of every command reply.
    notify: Arc<Mutex<Value>>,
}

fn game(status: &str, pending: &str, from: &str, to: &str, waiting: &[&str]) -> Value {
    json!({
        "match_id": "cm_1",
        "status": status,
        "challenger": {"agent_did": from, "label": "Alice", "wins": 0},
        "challenged": {"agent_did": to, "label": "Bob", "wins": 0},
        "round": 1,
        "rounds": [],
        "pending_step": pending,
        "waiting_for": waiting,
        "expires_at": "2030-01-09T09:00:00Z",
        "deadline_at": null,
        "winner": null,
    })
}

/// The agent a signed read is for; panics unless the node signed it with a fresh time.
fn own_read(body: &Value) -> Value {
    assert!(
        body["signature"].as_str().is_some_and(|s| !s.is_empty()),
        "{body}"
    );
    assert!(body["payload"]["issued_at"].as_str().is_some(), "{body}");
    body["payload"]["agent_did"].clone()
}

/// Takes any command but the one with action id `late`, answering with the match and `notify`.
async fn take_action(
    AxumState(mock): AxumState<MockCards>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    mock.actions.lock().await.push(body.clone());
    let payload = &body["payload"];
    if payload["action_id"] == "late" {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({"error": "not_open", "message": "the invite ran out", "details": null})),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "action": payload["action"],
            "match": mock.game.lock().await.clone(),
            "notify": mock.notify.lock().await.clone(),
        })),
    )
}

async fn list_searches(
    AxumState(mock): AxumState<MockCards>,
    AxumQuery(query): AxumQuery<HashMap<String, String>>,
) -> Json<Value> {
    mock.reads
        .lock()
        .await
        .push(("searches".to_owned(), json!(query)));
    Json(json!({"searches": [], "next_before": null}))
}

async fn show_day(
    AxumState(mock): AxumState<MockCards>,
    AxumPath(day): AxumPath<String>,
) -> Json<Value> {
    mock.reads.lock().await.push(("day".to_owned(), json!(day)));
    Json(json!({"day": day, "seed_hash": "h"}))
}

async fn spawn_mock_gateway() -> (String, MockCards) {
    let mock = MockCards::default();
    let router =
        axum::Router::new()
            .route(
                "/api/spaces/presence/{did}/record",
                axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route("/api/cards/searches", axum::routing::get(list_searches))
            .route("/api/cards/days/{day}", axum::routing::get(show_day))
            .route(
                "/api/games/{game_id}",
                axum::routing::get(|AxumPath(game_id): AxumPath<String>| async move {
                    Json(json!({"game_id": game_id, "space_id": "the-ville", "rules": {}}))
                }),
            )
            .route(
                "/api/cards/status",
                axum::routing::post(
                    |AxumState(mock): AxumState<MockCards>, Json(body): Json<Value>| async move {
                        let did = own_read(&body);
                        mock.reads
                            .lock()
                            .await
                            .push(("status".to_owned(), body["payload"].clone()));
                        Json(json!({"agent_did": did, "cards": [], "to_do": []}))
                    },
                ),
            )
            .route(
                "/api/cards/opponents",
                axum::routing::post(
                    |AxumState(mock): AxumState<MockCards>, Json(body): Json<Value>| async move {
                        own_read(&body);
                        mock.reads
                            .lock()
                            .await
                            .push(("opponents".to_owned(), body["payload"].clone()));
                        Json(json!({"opponents": [], "next_cursor": null}))
                    },
                ),
            )
            .route(
                "/api/cards/matches",
                axum::routing::get(
                    |AxumState(mock): AxumState<MockCards>,
                     AxumQuery(query): AxumQuery<HashMap<String, String>>| async move {
                        mock.reads
                            .lock()
                            .await
                            .push(("log".to_owned(), json!(query)));
                        Json(json!({"matches": [], "next_before": null}))
                    },
                ),
            )
            .route(
                "/api/cards/matches/{match_id}",
                axum::routing::get(
                    |AxumState(mock): AxumState<MockCards>,
                     AxumPath(match_id): AxumPath<String>| async move {
                        mock.reads
                            .lock()
                            .await
                            .push(("one".to_owned(), json!(match_id)));
                        let game = mock.game.lock().await.clone();
                        if game.is_null() {
                            (
                                axum::http::StatusCode::NOT_FOUND,
                                Json(json!({"error": "unknown", "message": "no match"})),
                            )
                        } else {
                            (axum::http::StatusCode::OK, Json(game))
                        }
                    },
                ),
            )
            .route("/api/cards/actions", axum::routing::post(take_action))
            .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, mock)
}

async fn call(app: Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp_request(
        app,
        token,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await
}

fn content(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

/// A node wired to the mock gateway.
async fn wired() -> (
    tempfile::TempDir,
    Router,
    String,
    ControlPlaneState,
    MockCards,
) {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![gateway_url],
        ..state
    };
    (dir, app(state.clone()), token, state, mock)
}

/// What was posted in the battle channel, oldest first: each message and whom it mentions.
async fn battle_posts(state: &ControlPlaneState) -> Vec<(Value, Option<String>)> {
    let network_id = state.swarm_bridge.current_network_id().await.unwrap();
    state
        .swarm_bridge
        .list_topic_messages(
            Some(&network_id),
            BOARD_BATTLE_FEED_KEY,
            "group:board-battle",
            100,
            None,
            None,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|message| {
            let target = message
                .agent_envelope
                .and_then(|envelope| envelope.target_agent_id);
            (message.content, target)
        })
        .collect()
}

fn notice_task(state: &ControlPlaneState, step: &str, target: &str) -> Option<(i64, i64)> {
    state
        .social_store
        .get_reliability_task("card_battle_notice", &format!("cm_1|{step}|{target}"))
        .unwrap()
        .map(|task| (task.attempt_count, task.next_attempt_at))
}

#[tokio::test]
async fn mcp_lists_the_card_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    for read in ["card_status", "card_opponents", "card_log"] {
        assert_eq!(
            find_tool(tools, read)["_meta"]["wattetheria"]["readOnly"],
            true,
            "{read}"
        );
    }
    // A deck is set with its cards, or only named: the schema asks for neither.
    assert_schema_requires(find_tool(tools, "card_deck"), &["action_id"]);
    for field in ["name", "note"] {
        assert_eq!(
            find_tool(tools, "card_deck")["inputSchema"]["properties"][field]["type"],
            "string"
        );
    }
    let challenge = find_tool(tools, "card_challenge");
    assert_schema_requires(challenge, &["action_id", "opponent_did"]);
    assert_eq!(challenge["inputSchema"]["additionalProperties"], false);
    // A match takes a deck and nothing else: no tool has a stake to name.
    assert_schema_requires(find_tool(tools, "card_accept"), &["action_id", "match_id"]);
    for tool in ["card_challenge", "card_accept", "card_opponents"] {
        assert!(
            find_tool(tools, tool)["inputSchema"]["properties"]
                .get("stake")
                .is_none(),
            "{tool}"
        );
    }
    assert_schema_requires(
        find_tool(tools, "card_play"),
        &["action_id", "card", "match_id"],
    );
    // The agent chooses which star of a card its deck fields.
    assert_eq!(
        find_tool(tools, "card_deck")["inputSchema"]["properties"]["stars"]["type"],
        "object"
    );
    // A card tool says what it does and answers; the rules and every number are the gateway's
    // to say (game_info), so that a rule changes without a node release.
    let cards: Vec<&Value> = tools
        .iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("card_"))
        })
        .collect();
    assert_eq!(cards.len(), 11);
    for tool in cards {
        let description = tool["description"].as_str().unwrap();
        assert!(!description.contains('%'), "{tool}");
        assert!(
            description.len() <= 900,
            "{} explains too much",
            tool["name"]
        );
    }
    assert_eq!(
        find_tool(tools, "leaderboard")["inputSchema"]["properties"]["board"]["enum"],
        json!(["space", "farm", "cards", "housing"])
    );
    // What to search is an object or a lot's piece, so neither is required by the schema.
    assert_schema_requires(find_tool(tools, "card_search"), &["action_id"]);
    assert_schema_requires(find_tool(tools, "card_merge"), &["action_id", "card"]);
    assert_schema_requires(find_tool(tools, "card_gacha"), &["action_id"]);
    assert_schema_requires(find_tool(tools, "card_deck_slot"), &["action_id"]);
    assert_eq!(
        find_tool(tools, "card_log")["inputSchema"]["properties"]["list"]["enum"],
        json!(["matches", "searches"])
    );
    assert_eq!(
        find_tool(tools, "publish_board_message")["inputSchema"]["properties"]["category"]["enum"],
        json!(["general", "trade", "search", "request", "battle"])
    );
}

#[tokio::test]
async fn game_info_adds_the_card_tools() {
    let (_dir, app, token, _state, _mock) = wired().await;
    let manual = content(&call(app, &token, "game_info", json!({"game_id": "cards"})).await);
    assert_eq!(manual["tools"][0]["tool"], "card_status", "{manual}");
    let tools: Vec<&str> = manual["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["tool"].as_str().unwrap())
        .collect();
    for tool in [
        "card_deck",
        "card_opponents",
        "card_challenge",
        "card_accept",
        "card_play",
        "card_search",
        "card_merge",
        "card_gacha",
        "card_deck_slot",
        "card_log",
        "leaderboard",
        "space_bag",
    ] {
        assert!(tools.contains(&tool), "{tool} missing from {tools:?}");
    }
}

#[tokio::test]
async fn the_card_log_also_reads_searches_and_a_days_hidden_cards() {
    let (_dir, app, token, state, mock) = wired().await;
    call(
        app.clone(),
        &token,
        "card_log",
        json!({"list": "searches", "agent_did": "self", "site": "lot:7", "limit": 3}),
    )
    .await;
    let day = content(
        &call(
            app.clone(),
            &token,
            "card_log",
            json!({"day": "2030-01-08"}),
        )
        .await,
    );
    assert_eq!(day["day"], "2030-01-08", "{day}");
    for bad in [json!({"day": "../status"}), json!({"list": "packs"})] {
        let refused = content(&call(app.clone(), &token, "card_log", bad).await);
        assert_eq!(refused["error"], "invalid", "{refused}");
    }
    let reads = mock.reads.lock().await.clone();
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(
        reads[0],
        (
            "searches".to_owned(),
            json!({"agent_did": state.agent_did, "limit": "3", "site": "lot:7"})
        )
    );
    assert_eq!(reads[1], ("day".to_owned(), json!("2030-01-08")));
}

#[tokio::test]
async fn the_commands_of_a_match_carry_where_the_agent_stands() {
    let (_dir, app, token, _state, mock) = wired().await;
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    // A match is played in one place: inviting, accepting and playing carry it too.
    for (tool, arguments, action) in [
        (
            "card_challenge",
            json!({"action_id": "c1", "opponent_did": "did:key:bob"}),
            "challenge",
        ),
        (
            "card_accept",
            json!({"action_id": "a1", "match_id": "cm_1"}),
            "accept",
        ),
        (
            "card_play",
            json!({"action_id": "p1", "match_id": "cm_1", "card": "stick"}),
            "play",
        ),
    ] {
        call(app.clone(), &token, tool, arguments).await;
        let sent = mock.actions.lock().await.last().cloned().unwrap();
        assert_eq!(sent["payload"]["action"], action);
        assert!(sent["payload"]["presence"]["record"].is_object(), "{sent}");
    }
}

#[tokio::test]
async fn searching_and_buying_a_pack_carry_where_the_agent_stands() {
    let (_dir, app, token, state, mock) = wired().await;
    // Outside the Space the command still goes out; the gateway is the one that refuses it.
    call(
        app.clone(),
        &token,
        "card_search",
        json!({"action_id": "s0", "lot": 7, "piece": 12}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "search");
    assert_eq!(sent["payload"]["lot"], 7);
    assert_eq!(sent["payload"]["piece"], 12);
    assert!(sent["payload"].get("presence").is_none(), "{sent}");

    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let found = call(
        app.clone(),
        &token,
        "card_search",
        json!({"action_id": "s1", "object": "hobbs-cafe/cafe/piano"}),
    )
    .await;
    assert_eq!(found["result"]["isError"], false, "{found}");
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert!(sent["signature"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(sent["payload"]["object"], "hobbs-cafe/cafe/piano");
    assert_eq!(
        sent["payload"]["presence"]["record"]["agent_did"],
        state.agent_did
    );
    call(
        app.clone(),
        &token,
        "card_gacha",
        json!({"action_id": "b1"}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "gacha");
    assert!(sent["payload"]["presence"]["record"].is_object(), "{sent}");

    // A deck slot and a star are bought in the market too: where the agent stands goes along.
    call(
        app.clone(),
        &token,
        "card_deck_slot",
        json!({"action_id": "b2"}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "deck_slot");
    assert!(sent["payload"]["presence"]["record"].is_object(), "{sent}");
    call(
        app.clone(),
        &token,
        "card_merge",
        json!({"action_id": "m1", "card": "stick", "stars": 2}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "merge");
    assert_eq!(sent["payload"]["card"], "stick");
    assert_eq!(sent["payload"]["stars"], 2);
    assert!(sent["payload"]["presence"]["record"].is_object(), "{sent}");
    // A deck goes with the stars its agent chose for its cards, and without when it chose none.
    call(
        app.clone(),
        &token,
        "card_deck",
        json!({"action_id": "d1", "cards": ["stick", "bed"], "stars": {"stick": 1}}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "deck");
    assert_eq!(sent["payload"]["cards"], json!(["stick", "bed"]));
    assert_eq!(sent["payload"]["stars"], json!({"stick": 1}));
    // Setting a deck is done from anywhere: no presence goes with it.
    assert!(sent["payload"].get("presence").is_none(), "{sent}");
    // A deck is named without its cards.
    call(
        app.clone(),
        &token,
        "card_deck",
        json!({"action_id": "d2", "slot": 2, "name": "anti-hunt", "note": ""}),
    )
    .await;
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "deck");
    assert_eq!(sent["payload"]["name"], "anti-hunt");
    assert_eq!(sent["payload"]["note"], "");
    assert!(sent["payload"].get("cards").is_none(), "{sent}");
    for (tool, bad) in [
        ("card_deck", json!({"action_id": "b3", "name": 7})),
        ("card_merge", json!({"action_id": "m2"})),
    ] {
        let refused = content(&call(app.clone(), &token, tool, bad).await);
        assert_eq!(refused["error"], "invalid", "{refused}");
    }
    assert_eq!(mock.actions.lock().await.len(), 7);
}

#[tokio::test]
async fn card_reads_are_signed_by_the_agent_and_relayed() {
    let (_dir, app, token, state, mock) = wired().await;
    let status = content(&call(app.clone(), &token, "card_status", json!({})).await);
    assert_eq!(status["agent_did"], state.agent_did);
    call(app.clone(), &token, "card_opponents", json!({"name": "bo"})).await;
    call(
        app.clone(),
        &token,
        "card_log",
        json!({"agent_did": "self", "limit": 5}),
    )
    .await;
    *mock.game.lock().await = game("active", "accept", "did:key:a", "did:key:b", &[]);
    let one = content(&call(app.clone(), &token, "card_log", json!({"match_id": "cm_1"})).await);
    assert_eq!(one["match_id"], "cm_1");
    // An id that could reach another gateway path is never sent.
    let odd = content(&call(app, &token, "card_log", json!({"match_id": "../status"})).await);
    assert_eq!(odd["error"], "unknown", "{odd}");

    let reads = mock.reads.lock().await.clone();
    assert_eq!(reads.len(), 4, "{reads:?}");
    assert_eq!(reads[0].0, "status");
    assert_eq!(reads[1].0, "opponents");
    assert_eq!(reads[1].1["agent_did"], state.agent_did);
    assert_eq!(reads[1].1["name"], "bo");
    assert!(reads[1].1.get("cursor").is_none());
    assert_eq!(
        reads[2],
        (
            "log".to_owned(),
            json!({"agent_did": state.agent_did, "limit": "5"})
        )
    );
    assert_eq!(reads[3], ("one".to_owned(), json!("cm_1")));
}

#[tokio::test]
async fn a_card_command_is_signed_and_the_other_agent_is_told_in_the_battle_channel() {
    let (_dir, app, token, state, mock) = wired().await;
    let bob = Identity::new_random().agent_did;
    *mock.game.lock().await = game("invited", "challenge", &state.agent_did, &bob, &[&bob]);
    *mock.notify.lock().await = json!({"agent_did": bob, "step": "challenge"});

    let arguments = json!({"action_id": "a-1", "opponent_did": bob, "deck": 2});
    let reply = content(&call(app.clone(), &token, "card_challenge", arguments.clone()).await);
    assert_eq!(reply["match"]["match_id"], "cm_1", "{reply}");
    assert_eq!(reply["told"]["agent_did"], bob);
    assert_eq!(reply["told"]["step"], "challenge");
    assert_eq!(reply["told"]["posted"], true);

    let sent = mock.actions.lock().await.clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0]["signature"].as_str().is_some_and(|s| !s.is_empty()));
    let payload = &sent[0]["payload"];
    assert_eq!(payload["action"], "challenge");
    assert_eq!(payload["action_id"], "a-1");
    assert_eq!(payload["agent_did"], state.agent_did);
    assert_eq!(payload["opponent_did"], bob);
    assert_eq!(payload["deck"], 2);
    assert!(payload.get("stake").is_none(), "{payload}");

    let posts = battle_posts(&state).await;
    assert_eq!(posts.len(), 1, "{posts:?}");
    let (notice, target) = &posts[0];
    assert_eq!(target.as_deref(), Some(bob.as_str()));
    assert_eq!(notice["kind"], "card_battle");
    assert_eq!(notice["match_id"], "cm_1");
    assert_eq!(notice["step"], "challenge");
    assert!(
        notice["text"]
            .as_str()
            .unwrap()
            .starts_with("Alice invites you to card match cm_1. Take it up with card_accept"),
        "{notice}"
    );
    // Kept to send again: sent once, next in five minutes.
    let (attempts, next) = notice_task(&state, "challenge", &bob).expect("kept for retry");
    assert_eq!(attempts, 1);
    let now = chrono::Utc::now().timestamp();
    assert!((now + 295..=now + 305).contains(&next), "{next} vs {now}");

    // The same command again: the gateway answers the same, and nothing is posted twice.
    let again = content(&call(app, &token, "card_challenge", arguments).await);
    assert_eq!(again["told"]["posted"], true);
    assert_eq!(battle_posts(&state).await.len(), 1);
    assert_eq!(notice_task(&state, "challenge", &bob).unwrap().0, 1);
}

#[tokio::test]
async fn a_refused_command_and_a_first_card_tell_nobody_and_the_end_is_told_once() {
    let (_dir, app, token, state, mock) = wired().await;
    let bob = Identity::new_random().agent_did;
    *mock.game.lock().await = game("active", "accept", &bob, &state.agent_did, &[&bob]);
    *mock.notify.lock().await = json!({"agent_did": bob, "step": "accept"});

    // Refused by the gateway: relayed as it is.
    let refused = content(
        &call(
            app.clone(),
            &token,
            "card_accept",
            json!({"action_id": "late", "match_id": "cm_1"}),
        )
        .await,
    );
    assert_eq!(refused["error"], "not_open", "{refused}");
    assert!(refused.get("told").is_none());
    // A command the node cannot make sense of never reaches the gateway.
    let invalid = content(
        &call(
            app.clone(),
            &token,
            "card_play",
            json!({"action_id": "p-0", "match_id": "cm_1"}),
        )
        .await,
    );
    assert_eq!(invalid["error"], "invalid", "{invalid}");
    assert_eq!(mock.actions.lock().await.len(), 1);

    // The first card of a round: the gateway names nobody to tell.
    *mock.notify.lock().await = Value::Null;
    let first = content(
        &call(
            app.clone(),
            &token,
            "card_play",
            json!({"action_id": "p-1", "match_id": "cm_1", "card": "moss_blob"}),
        )
        .await,
    );
    assert!(first["told"].is_null(), "{first}");
    assert_eq!(battle_posts(&state).await.len(), 0);

    // The card that ends the match: told, and not kept to send again.
    *mock.game.lock().await = game("finished", "final", &bob, &state.agent_did, &[]);
    *mock.notify.lock().await = json!({"agent_did": bob, "step": "final"});
    let last = content(
        &call(
            app,
            &token,
            "card_play",
            json!({"action_id": "p-2", "match_id": "cm_1", "card": "moss_blob"}),
        )
        .await,
    );
    assert_eq!(last["told"]["posted"], true, "{last}");
    let posts = battle_posts(&state).await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0["step"], "final");
    assert!(notice_task(&state, "final", &bob).is_none());
}

#[tokio::test]
async fn a_notice_is_sent_again_until_the_gateway_shows_the_agent_moved_on() {
    let (_dir, app, token, state, mock) = wired().await;
    let bob = Identity::new_random().agent_did;
    *mock.game.lock().await = game("invited", "challenge", &state.agent_did, &bob, &[&bob]);
    *mock.notify.lock().await = json!({"agent_did": bob, "step": "challenge"});
    call(
        app,
        &token,
        "card_challenge",
        json!({"action_id": "a-1", "opponent_did": bob}),
    )
    .await;
    let now = chrono::Utc::now().timestamp();

    // Not due yet.
    assert_eq!(resend_due_battle_notices(&state, now, 10).await.unwrap(), 0);
    assert_eq!(battle_posts(&state).await.len(), 1);

    // Due, and the match still waits for Bob: sent again, next in fifteen minutes.
    let due = now + 400;
    assert_eq!(resend_due_battle_notices(&state, due, 10).await.unwrap(), 1);
    let posts = battle_posts(&state).await;
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].0["step"], "challenge");
    assert_eq!(posts[1].1.as_deref(), Some(bob.as_str()));
    assert_eq!(notice_task(&state, "challenge", &bob), Some((2, due + 900)));

    // Bob took the invite up: the notice is dropped without another post.
    *mock.game.lock().await = game("active", "accept", &state.agent_did, &bob, &[&bob]);
    let later = due + 1000;
    assert_eq!(
        resend_due_battle_notices(&state, later, 10).await.unwrap(),
        1
    );
    assert_eq!(battle_posts(&state).await.len(), 2);
    assert!(notice_task(&state, "challenge", &bob).is_none());
}

#[tokio::test]
async fn a_notice_stops_after_ten_retries_or_when_the_match_is_gone() {
    let (_dir, _app, _token, state, mock) = wired().await;
    let bob = Identity::new_random().agent_did;
    *mock.game.lock().await = game("active", "round:1", &state.agent_did, &bob, &[&bob]);
    let id = format!("cm_1|round:1|{bob}");
    // The first send and nine retries are behind it.
    for sent in 0..10 {
        state
            .social_store
            .record_reliability_attempt("card_battle_notice", &id, sent, sent + 1, None)
            .unwrap();
    }
    // The tenth retry goes out.
    assert_eq!(resend_due_battle_notices(&state, 100, 10).await.unwrap(), 1);
    assert_eq!(battle_posts(&state).await.len(), 1);
    assert_eq!(notice_task(&state, "round:1", &bob), Some((11, 100 + 3600)));
    // There is no eleventh.
    assert_eq!(
        resend_due_battle_notices(&state, 10_000, 10).await.unwrap(),
        1
    );
    assert_eq!(battle_posts(&state).await.len(), 1);
    assert!(notice_task(&state, "round:1", &bob).is_none());

    // A notice about a match the gateway does not know is dropped.
    state
        .social_store
        .record_reliability_attempt("card_battle_notice", &id, 0, 1, None)
        .unwrap();
    *mock.game.lock().await = Value::Null;
    assert_eq!(resend_due_battle_notices(&state, 100, 10).await.unwrap(), 1);
    assert!(notice_task(&state, "round:1", &bob).is_none());
    assert_eq!(battle_posts(&state).await.len(), 1);
}

#[tokio::test]
async fn a_notice_waits_without_using_a_retry_while_the_gateway_cannot_be_asked() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec!["http://127.0.0.1:1".to_owned()],
        ..state
    };
    let bob = Identity::new_random().agent_did;
    let id = format!("cm_1|accept|{bob}");
    state
        .social_store
        .record_reliability_attempt("card_battle_notice", &id, 0, 1, None)
        .unwrap();
    assert_eq!(resend_due_battle_notices(&state, 100, 10).await.unwrap(), 1);
    assert_eq!(notice_task(&state, "accept", &bob), Some((1, 100 + 900)));
    assert_eq!(battle_posts(&state).await.len(), 0);
}
