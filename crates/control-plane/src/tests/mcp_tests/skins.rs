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
                    let publishers = mock.publishers.lock().await;
                    let Some(asked) = query["owner"].as_str() else {
                        // The public gallery: the one image, if anyone has published it.
                        let skins = publishers.iter().next().map_or_else(
                            || json!([]),
                            |first| {
                                let mut skin = skin_json(first);
                                skin["publishers"] = json!(publishers.iter().collect::<Vec<_>>());
                                json!([skin])
                            },
                        );
                        return Json(json!({
                            "skins": skins, "limit": query["limit"], "offset": query["offset"], "fee_watt": 100
                        }));
                    };
                    let skins = if publishers.contains(asked) {
                        json!([skin_json(asked)])
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
        gateway_urls: vec![gateway_url.clone()],
        ..state
    };
    let app = app(state.clone());
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let built_in = entered["result"]["structuredContent"]["presence"]["record"]["avatar_id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        entered["result"]["structuredContent"]["skin"],
        format!("builtin:{built_in}")
    );

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

    let listed = call(app.clone(), &token, "list_my_skins", json!({})).await;
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    let content = &listed["result"]["structuredContent"];
    assert_eq!(content["skins"][0]["skin_id"], SKIN);
    assert_eq!(
        content["skins"][0]["image_url"],
        format!("{gateway_url}/api/skins/{SKIN}")
    );
    assert_eq!(content["fee_watt"], 100);
    assert_eq!(content["wearing"], format!("builtin:{built_in}"));

    // Everything wearable: the named built-in skins plus the gateway gallery with image links.
    let all = call(
        app.clone(),
        &token,
        "list_skins",
        json!({"limit": 5, "offset": 0}),
    )
    .await;
    assert_eq!(all["result"]["isError"], false, "{all}");
    let content = &all["result"]["structuredContent"];
    let catalog = content["builtin"].as_array().unwrap();
    assert_eq!(catalog.len(), 25);
    assert_eq!(catalog[0]["skin_id"], "builtin:0");
    assert_eq!(catalog[24]["skin_id"], "builtin:24");
    assert!(catalog[16]["name"].as_str().unwrap().contains("cat"));
    assert_eq!(content["custom"][0]["skin_id"], SKIN);
    let publishers = content["custom"][0]["publishers"].as_array().unwrap();
    assert!(
        publishers.contains(&json!(state.agent_did)),
        "{publishers:?}"
    );
    assert_eq!(
        content["custom"][0]["image_url"],
        format!("{gateway_url}/api/skins/{SKIN}")
    );
    assert_eq!(content["gateway"], gateway_url);
    assert_eq!(content["wearing"], format!("builtin:{built_in}"));

    for bad in ["builtin:25", "builtin:x", "BUILTIN:1", "not-a-hash"] {
        let rejected = call(
            app.clone(),
            &token,
            "set_agent_skin",
            json!({"skin_id": bad}),
        )
        .await;
        assert_eq!(rejected["result"]["isError"], true, "{bad}: {rejected}");
    }
    let worn = call(
        app.clone(),
        &token,
        "set_agent_skin",
        json!({"skin_id": SKIN}),
    )
    .await;
    assert_eq!(worn["result"]["isError"], false, "{worn}");
    assert_eq!(worn["result"]["structuredContent"]["skin"], SKIN);
    let record = &worn["result"]["structuredContent"]["presence"]["record"];
    assert_eq!(record["skin_id"], SKIN);
    assert_eq!(
        record["avatar_id"], built_in,
        "built-in skin stays in the signed record as the fallback"
    );
    let listed = call(app.clone(), &token, "list_my_skins", json!({})).await;
    assert_eq!(listed["result"]["structuredContent"]["wearing"], SKIN);

    // Re-entry remembers the custom skin while the gateway still lists it.
    let left = call(app.clone(), &token, "leave_space", json!({})).await;
    assert_eq!(left["result"]["isError"], false, "{left}");
    let reentered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(reentered["result"]["isError"], false, "{reentered}");
    assert_eq!(reentered["result"]["structuredContent"]["skin"], SKIN);
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
    assert_eq!(
        reentered["result"]["structuredContent"]["skin"],
        format!("builtin:{built_in}")
    );
    let record = &reentered["result"]["structuredContent"]["presence"]["record"];
    assert_eq!(record["skin_id"], Value::Null);
    assert_eq!(record["avatar_id"], built_in);

    let back_to_built_in = call(
        app,
        &token,
        "set_agent_skin",
        json!({"skin_id": "builtin:4"}),
    )
    .await;
    assert_eq!(
        back_to_built_in["result"]["isError"], false,
        "{back_to_built_in}"
    );
    assert_eq!(
        back_to_built_in["result"]["structuredContent"]["skin"],
        "builtin:4"
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
        ("list_my_skins", json!({})),
    ] {
        let response = call(app.clone(), &token, name, arguments).await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert!(
            response.to_string().contains("no gateway"),
            "{name}: {response}"
        );
    }
    let malformed = call(
        app.clone(),
        &token,
        "set_agent_skin",
        json!({"skin_id": "nope"}),
    )
    .await;
    assert_eq!(malformed["result"]["isError"], true, "{malformed}");
    assert!(malformed.to_string().contains("hex"), "{malformed}");

    // Built-in skins are listed even without a gateway; the custom part is simply empty.
    let all = call(app, &token, "list_skins", json!({})).await;
    assert_eq!(all["result"]["isError"], false, "{all}");
    let content = &all["result"]["structuredContent"];
    assert_eq!(content["builtin"].as_array().unwrap().len(), 25);
    assert_eq!(content["custom"], json!([]));
    assert_eq!(content["gateway"], Value::Null);
}
