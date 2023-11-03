use component_utils::codec::Codec;
use std::{
    net::Ipv4Addr,
    sync::{Arc, Mutex},
};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Router,
};
use chain_api::{NodeData, UserData};

type Db = Arc<Mutex<NotAProdDB>>;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    config::env_config! {
        PORT: u16,
    }

    let app = Router::new()
        .route(chain_api::USER_BY_NAME, get(user_by_name))
        .route(chain_api::USER_BY_SIGN, get(user_by_sign))
        .route(chain_api::CREATE_USER, post(create_user))
        .route(chain_api::NODES, get(registered_nodes))
        .route(chain_api::NODES, post(register_node))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(Db::default());

    axum::Server::bind(&(Ipv4Addr::UNSPECIFIED, PORT).into())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn register_node(State(db): State<Db>, bytes: Bytes) -> Result<(), StatusCode> {
    let node = NodeData::decode(&mut bytes.as_ref()).ok_or(StatusCode::BAD_REQUEST)?;
    let mut db = db.lock().unwrap();
    if db
        .nodes
        .iter()
        .any(|n| n.sign == node.sign || n.enc == node.enc)
    {
        return Err(StatusCode::CONFLICT);
    }
    db.nodes.push(node);
    Ok(())
}

async fn registered_nodes(State(db): State<Db>) -> Result<Bytes, StatusCode> {
    let db = db.lock().unwrap();
    Ok(Bytes::from(db.nodes.to_bytes()))
}

async fn create_user(State(db): State<Db>, bytes: Bytes) -> Result<(), StatusCode> {
    let user = UserData::decode(&mut bytes.as_ref()).ok_or(StatusCode::BAD_REQUEST)?;
    let mut db = db.lock().unwrap();
    if db
        .users
        .iter()
        .any(|u| u.name == user.name || u.sign == user.sign || u.enc == user.enc)
    {
        return Err(StatusCode::CONFLICT);
    }
    db.users.push(user);
    Ok(())
}

async fn user_by_name(Path(name): Path<String>, State(db): State<Db>) -> Result<Bytes, StatusCode> {
    let db = db.lock().unwrap();
    let user = db
        .users
        .iter()
        .find(|u| u.name == name)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Bytes::from(user.to_bytes()))
}

async fn user_by_sign(Path(sign): Path<String>, State(db): State<Db>) -> Result<Bytes, StatusCode> {
    let sign = hex::decode(sign).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sign: [u8; 32] = sign.try_into().map_err(|_| StatusCode::BAD_REQUEST)?;
    let db = db.lock().unwrap();
    let user = db
        .users
        .iter()
        .find(|u| crypto::sign::PublicKey::from(u.sign).ed == sign)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Bytes::from(user.to_bytes()))
}

#[derive(Default)]
struct NotAProdDB {
    users: Vec<UserData>,
    nodes: Vec<NodeData>,
}
