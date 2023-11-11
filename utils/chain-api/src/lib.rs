use component_utils::Codec;
use protocols::chat::UserName;
use reqwest::Client;
use std::fmt;
use std::sync::OnceLock;

component_utils::protocol! { 'a:
    #[derive(Debug, Clone)]
    struct UserData {
        name: UserName,
        sign: crypto::sign::SerializedPublicKey,
        enc: crypto::enc::SerializedPublicKey,
    }

    #[derive(Debug, Clone, Copy)]
    struct NodeData {
        sign: crypto::sign::SerializedPublicKey,
        enc: crypto::enc::SerializedPublicKey,
        ip: [u8; 4],
        port: u16,
    }
}

pub const NODES: &str = "/nodes";

pub const USER_BY_NAME: &str = "/user/name/:id";
pub const USER_BY_SIGN: &str = "/user/sign/:id";
pub const CREATE_USER: &str = "/user";

fn get_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::new)
}

pub async fn register_node(
    addr: impl fmt::Display,
    data: NodeData,
) -> Result<(), RegisterNodeError> {
    let url = format!("{addr}{NODES}");
    let res = get_client().post(&url).body(data.to_bytes()).send().await?;
    match res.status() {
        reqwest::StatusCode::OK => Ok(()),
        reqwest::StatusCode::CONFLICT => Err(RegisterNodeError::Conflict),
        _ => Err(res.error_for_status().unwrap_err().into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterNodeError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("node already exists")]
    Conflict,
}

pub async fn nodes(addr: impl fmt::Display) -> Result<Vec<NodeData>, NodesError> {
    let url = format!("{addr}{NODES}");
    let res = get_client().get(&url).send().await?;
    match res.status() {
        reqwest::StatusCode::OK => {}
        _ => return Err(res.error_for_status().unwrap_err().into()),
    }
    let data = res.bytes().await?;
    <Vec<NodeData>>::decode(&mut data.as_ref()).ok_or(NodesError::Codec)
}

#[derive(Debug, thiserror::Error)]
pub enum NodesError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("failed to decode nodes data")]
    Codec,
}

pub async fn create_user(addr: impl fmt::Display, data: UserData) -> Result<(), CreateUserError> {
    let url = format!("{addr}{CREATE_USER}");
    let res = get_client().post(&url).body(data.to_bytes()).send().await?;
    match res.status() {
        reqwest::StatusCode::OK => Ok(()),
        reqwest::StatusCode::CONFLICT => Err(CreateUserError::Conflict),
        _ => Err(res.error_for_status().unwrap_err().into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateUserError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("user already exists")]
    Conflict,
}

pub async fn user_by_name(addr: impl fmt::Display, name: &str) -> Result<UserData, GetUserError> {
    let url = format!("{addr}{}", USER_BY_NAME.replace(":id", name));
    get_user(url).await
}

pub async fn user_by_sign(
    addr: impl fmt::Display,
    sign: crypto::sign::PublicKey,
) -> Result<UserData, GetUserError> {
    let hex_sign = hex::encode(sign.ed);
    let url = format!("{addr}{}", USER_BY_SIGN.replace(":id", &hex_sign));
    get_user(url).await
}

async fn get_user(path: String) -> Result<UserData, GetUserError> {
    let res = get_client().get(&path).send().await?;
    match res.status() {
        reqwest::StatusCode::OK => {}
        reqwest::StatusCode::NOT_FOUND => return Err(GetUserError::NotFound),
        _ => return Err(GetUserError::Reqwest(res.error_for_status().unwrap_err())),
    }
    let data = res.bytes().await?;
    UserData::decode(&mut data.as_ref()).ok_or(GetUserError::Codec)
}

#[derive(Debug, thiserror::Error)]
pub enum GetUserError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("failed to decode user data")]
    Codec,
    #[error("user not found")]
    NotFound,
}
