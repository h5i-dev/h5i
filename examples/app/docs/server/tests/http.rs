mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use docs_server::{principal, DocsApp};
use http_body_util::BodyExt;
use h5i_app_http::{rpc_router, HmacAuth, H5iApp};
use h5i_app_pg::EngineConfig;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

async fn call(app: &axum::Router, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::post("/rpc").header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn review_workflow_over_http() {
    let Some(engine) = engine(EngineConfig::default()).await else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let auth = HmacAuth::<DocsApp>::new("test-secret", principal);
    let t = fresh_tenant();
    let (alice, bob) = (auth.issue(t, 1, 60), auth.issue(t, 2, 60));
    let forged = HmacAuth::<DocsApp>::new("wrong", principal).issue(t, 1, 60);
    let app = rpc_router(H5iApp::new(Arc::new(engine), auth));

    assert_eq!(call(&app, None, json!({"cmd": "create_project", "name": "p"})).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, Some(&forged), json!({"cmd": "create_project", "name": "p"})).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, Some(&alice), json!({"cmd": "nope"})).await.0, StatusCode::BAD_REQUEST);

    let (s, v) = call(&app, Some(&alice), json!({"cmd": "create_project", "name": "p"})).await;
    assert_eq!(s, StatusCode::OK);
    let p = v["created"].as_u64().unwrap();
    let (_, v) = call(&app, Some(&alice), json!({"cmd": "create_document", "project": p, "title": "t", "body": "b"})).await;
    let d = v["created"].as_u64().unwrap();

    // Bob is not a member yet, so the document looks missing to him.
    assert_eq!(call(&app, Some(&bob), json!({"cmd": "get_document", "doc": d})).await.0, StatusCode::NOT_FOUND);
    call(&app, Some(&alice), json!({"cmd": "set_member", "project": p, "user": 2, "role": "owner"})).await;
    call(&app, Some(&alice), json!({"cmd": "submit", "doc": d})).await;

    // Four-eyes rule: the author cannot approve.
    let (s, v) = call(&app, Some(&alice), json!({"cmd": "approve", "doc": d})).await;
    assert_eq!((s, v["error"].as_str()), (StatusCode::FORBIDDEN, Some("self_approval")));
    assert_eq!(call(&app, Some(&bob), json!({"cmd": "approve", "doc": d})).await.0, StatusCode::OK);
    assert_eq!(call(&app, Some(&alice), json!({"cmd": "publish", "doc": d})).await.0, StatusCode::OK);

    let (_, v) = call(&app, Some(&bob), json!({"cmd": "get_document", "doc": d})).await;
    assert_eq!((v["status"].as_str(), v["approver"].as_u64()), (Some("published"), Some(2)));
}
