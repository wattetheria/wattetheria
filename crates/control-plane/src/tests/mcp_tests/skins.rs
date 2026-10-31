use super::*;
use axum::extract::{Query, State as AxumState};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::Mutex;

const SKIN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

/// Who has published the one image this mock knows about.
#[derive(Clone, Default)]
struct MockSkins {
    publishers: Arc<Mutex<BTreeSet<String>>>,
}

fn skin_json(owner: &str) -> Value {
    json!({
        "skin_id": SKIN, "owner_did": owner, "name": "Neon Fox",
        "width": 96, "height": 128, "byte_len": 1234, "created_at_ms": 1
    })
}

/// A gateway with one skin registry (publish, list) plus the presence recovery lookup entry
/// needs.
async fn spawn_mock_gateway() -> (String, MockSkins) {
    let mock = MockSkins::default();
    let router = axum::Router::new()
        .route(
            "/api/spaces/presence/{did}/record",
            axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
        )
        .route(
            "/api/skins",
            axum::routing::get(
                |AxumState(mock): AxumState<MockSkins>, Query(query): Query<Value>| async move {
                    let asked = query["owner"].as_str().unwrap_or_default().to_owned();
                    let skins = if mock.publishers.lock().await.contains(&asked) {
                        json!([skin_json(&asked)])
                    } else {
                        json!([])
                    };
                    Json(json!({"owner": asked, "skins": skins, "fee_watt": 100}))
                },
            )
            .post(
                |AxumState(mock): AxumState<MockSkins>, Json(body): Json<Value>| async move {
                    assert!(body["signature"].as_str().is_some_and(|s| !s.is_empty()));
                    assert_eq!(body["payload"]["png_base64"], "QUJDRA==");
                    let uploader = body["payload"]["uploader"].as_str().unwrap().to_owned();
                    let charged = mock.publishers.lock().await.insert(uploader.clone());
                    (
                        if charged {
                            axum::http::StatusCode::CREATED
                        } else {
                            axum::http::StatusCode::OK
                        },
                        Json(json!({"skin": skin_json(&uploader), "charged": charged, "fee_watt": 100})),
                    )
                },
            ),
        )
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn custom_skins_are_published_listed_and_worn_through_the_gateway() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![gateway_url],
        ..state
    };
    let app = app(state.clone());
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let built_in = entered["result"]["structuredContent"]["presence"]["record"]["avatar_id"]
        .as_u64()
        .unwrap();

    // A skin this agent never published cannot be worn, even if someone else published it.
    mock.publishers
        .lock()
        .await
        .insert("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_owned());
    let unknown = call(
        app.clone(),
        &token,
        "set_agent_skin",
        json!({"skin_id": SKIN}),
    )
    .await;
    assert_eq!(unknown["result"]["isError"], true, "{unknown}");
    assert!(
        unknown.to_string().contains("not one you published"),
        "{unknown}"
    );

    let uploaded = call(
        app.clone(),
        &token,
        "upload_agent_skin",
        json!({"png_base64": "QUJDRA==", "name": "Neon Fox"}),
    )
    .await;
    assert_eq!(uploaded["result"]["isError"], false, "{uploaded}");
    let published = &uploaded["result"]["structuredContent"]["published"];
    assert_eq!(published["skin"]["skin_id"], SKIN);
    assert_eq!(published["charged"], true);
    assert!(mock.publishers.lock().await.contains(&state.agent_did));
    let again = call(
        app.clone(),
        &token,
        "upload_agent_skin",
        json!({"png_base64": "QUJDRA=="}),
    )
    .await;
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(
        again["result"]["structuredContent"]["published"]["charged"],
        false
    );

    let listed = call(app.clone(), &token, "list_agent_skins", json!({})).await;
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    let content = &listed["result"]["structuredContent"];
    assert_eq!(content["skins"][0]["skin_id"], SKIN);
    assert_eq!(content["fee_watt"], 100);
    assert_eq!(content["wearing"]["avatar_id"], built_in);
    assert_eq!(content["wearing"]["skin_id"], Value::Null);

    let both = call(
        app.clone(),
        &token,
        "set_agent_skin",
        json!({"skin_id": SKIN, "avatar_id": 1}),
    )
    .await;
    assert_eq!(both["result"]["isError"], true, "{both}");
    let worn = call(
        app.clone(),
        &token,
        "set_agent_skin",
        json!({"skin_id": SKIN}),
    )
    .await;
    assert_eq!(worn["result"]["isError"], false, "{worn}");
    let record = &worn["result"]["structuredContent"]["presence"]["record"];
    assert_eq!(record["skin_id"], SKIN);
    assert_eq!(
        record["avatar_id"], built_in,
        "built-in skin stays as the fallback"
    );
    let listed = call(app.clone(), &token, "list_agent_skins", json!({})).await;
    assert_eq!(
        listed["result"]["structuredContent"]["wearing"]["skin_id"],
        SKIN
    );

    // Re-entry remembers the custom skin while the gateway still lists it.
    let left = call(app.clone(), &token, "leave_space", json!({})).await;
    assert_eq!(left["result"]["isError"], false, "{left}");
    let reentered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(reentered["result"]["isError"], false, "{reentered}");
    assert_eq!(
        reentered["result"]["structuredContent"]["presence"]["record"]["skin_id"],
        SKIN
    );

    // If the gateway forgot the publication (reset), re-entry falls back to the built-in skin.
    mock.publishers.lock().await.remove(&state.agent_did);
    let left = call(app.clone(), &token, "leave_space", json!({})).await;
    assert_eq!(left["result"]["isError"], false, "{left}");
    let reentered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(reentered["result"]["isError"], false, "{reentered}");
    let record = &reentered["result"]["structuredContent"]["presence"]["record"];
    assert_eq!(record["skin_id"], Value::Null);
    assert_eq!(record["avatar_id"], built_in);

    let back_to_built_in = call(app, &token, "set_agent_skin", json!({"avatar_id": 4})).await;
    assert_eq!(
        back_to_built_in["result"]["isError"], false,
        "{back_to_built_in}"
    );
    let record = &back_to_built_in["result"]["structuredContent"]["presence"]["record"];
    assert_eq!(record["avatar_id"], 4);
    assert_eq!(record["skin_id"], Value::Null);
}

#[tokio::test]
async fn custom_skins_need_a_gateway() {
    let (_dir, app, token, _state_policy, _state) = build_test_app(100);
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    for (name, arguments) in [
        ("set_agent_skin", json!({"skin_id": SKIN})),
        ("upload_agent_skin", json!({"png_base64": "QUJDRA=="})),
        ("list_agent_skins", json!({})),
    ] {
        let response = call(app.clone(), &token, name, arguments).await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert!(
            response.to_string().contains("no gateway"),
            "{name}: {response}"
        );
    }
    let malformed = call(app, &token, "set_agent_skin", json!({"skin_id": "nope"})).await;
    assert_eq!(malformed["result"]["isError"], true, "{malformed}");
    assert!(malformed.to_string().contains("hex"), "{malformed}");
}
