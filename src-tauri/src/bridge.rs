use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        DefaultBodyLimit, Path, State, WebSocketUpgrade,
    },
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub const PORT: u16 = 38261;
const MAX_BODY: usize = 256 * 1024;
const MAX_CLIENTS: usize = 32;

struct Client {
    user: String,
    role: String,
    tx: mpsc::Sender<Value>,
}
#[derive(Default)]
struct Inner {
    values: HashMap<(String, String), Value>,
    clients: HashMap<Uuid, Client>,
    revision: u64,
    ready: bool,
    error: Option<String>,
}
pub struct Bridge {
    token: String,
    instance: String,
    inner: Mutex<Inner>,
}
pub type Shared = Arc<Bridge>;

impl Bridge {
    pub fn new() -> Result<Shared, String> {
        let folder = std::path::PathBuf::from(
            std::env::var("LOCALAPPDATA").map_err(|_| "Local app data is unavailable")?,
        )
        .join("Larpblox");
        std::fs::create_dir_all(&folder).map_err(|_| "Couldn't create app data")?;
        let path = folder.join("bridge-token.txt");
        let token = if path.exists() {
            let value =
                std::fs::read_to_string(&path).map_err(|_| "Couldn't read pairing token")?;
            if value.trim().len() != 32 || !value.trim().bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid saved pairing token".into());
            }
            value.trim().to_owned()
        } else {
            let value = Uuid::new_v4().simple().to_string();
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| "Couldn't save pairing token")?;
            file.write_all(value.as_bytes())
                .map_err(|_| "Couldn't save pairing token")?;
            value
        };
        Ok(Arc::new(Self {
            token,
            instance: Uuid::new_v4().simple().to_string(),
            inner: Mutex::new(Inner::default()),
        }))
    }
    fn authorized(&self, value: &str) -> bool {
        let a = value.as_bytes();
        let b = self.token.as_bytes();
        a.len() == b.len() && a.iter().zip(b).fold(0_u8, |diff, (a, b)| diff | (a ^ b)) == 0
    }
    pub fn status(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"ready":inner.ready,"error":inner.error,"browsers":inner.clients.values().filter(|c|c.role=="browser").count(),"games":inner.clients.values().filter(|c|c.role=="game").count()})
    }
    fn publish(&self, channel: &str, value: &Value, user: &str) -> Result<Value, &'static str> {
        let mut value = clean(channel, value, user)?;
        let mut inner = self.inner.lock().unwrap();
        if !inner.values.keys().any(|(u, _)| u == user)
            && inner
                .values
                .keys()
                .map(|(u, _)| u)
                .collect::<HashSet<_>>()
                .len()
                >= 32
        {
            return Err("Local connection is full");
        }
        inner.revision += 1;
        if channel == "browser" {
            value["revision"] = json!(inner.revision);
        }
        inner
            .values
            .insert((user.into(), channel.into()), value.clone());
        let role = if channel == "game" { "browser" } else { "game" };
        let message = json!({"type":"state","channel":channel,"value":value});
        for client in inner
            .clients
            .values()
            .filter(|c| c.user == user && c.role == role)
        {
            let _ = client.tx.try_send(message.clone());
        }
        Ok(value)
    }
}

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && !value.starts_with('0')
        && value.bytes().all(|b| b.is_ascii_digit())
        && value
            .parse::<u64>()
            .is_ok_and(|n| n < 9_007_199_254_740_992)
}
fn asset(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|n| n > 0 && n < 9_007_199_254_740_992)
}
fn valid_image(value: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    value.len() <= 2048
        && RE
            .get_or_init(|| {
                regex::Regex::new(r"^https://(?:tr|t[0-9])\.rbxcdn\.com/[A-Za-z0-9_.~/-]+$")
                    .unwrap()
            })
            .is_match(value)
}
fn clean(channel: &str, value: &Value, user: &str) -> Result<Value, &'static str> {
    let bad = "Invalid appearance data";
    if value["version"] != 1 || value["userId"].as_str() != Some(user) {
        return Err(bad);
    }
    if channel == "names" {
        let name = value["name"].as_str().ok_or(bad)?;
        if !value["enabled"].is_boolean()
            || (!name.is_empty()
                && (!(3..=20).contains(&name.len())
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')))
        {
            return Err(bad);
        }
        return Ok(json!({"version":1,"userId":user,"enabled":value["enabled"],"name":name}));
    }
    let definition = value["definition"].as_object().ok_or(bad)?;
    let assets = definition
        .get("assets")
        .and_then(Value::as_array)
        .ok_or(bad)?;
    if assets.len() > 100 || assets.iter().any(|a| !asset(&a["id"])) {
        return Err(bad);
    }
    let mut d = Map::new();
    let mut safe_assets = Vec::new();
    for a in assets {
        let mut safe = json!({"id":a["id"]});
        if let Some(meta) = a.get("meta").filter(|m| !m.is_null()) {
            let mut m = Map::new();
            for key in ["order", "puffiness"] {
                if let Some(number) = meta.get(key) {
                    if !number
                        .as_f64()
                        .is_some_and(|n| n.is_finite() && (0.0..=100.0).contains(&n))
                    {
                        return Err(bad);
                    }
                    m.insert(key.into(), number.clone());
                }
            }
            safe["meta"] = Value::Object(m);
        }
        safe_assets.push(safe);
    }
    d.insert("assets".into(), json!(safe_assets));
    let mut colors = Map::new();
    for key in [
        "headColor",
        "torsoColor",
        "leftArmColor",
        "rightArmColor",
        "leftLegColor",
        "rightLegColor",
    ] {
        let color = value["definition"]["bodyColors"][key].as_str().ok_or(bad)?;
        let color = color.trim_start_matches('#');
        if color.len() != 6 || !color.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad);
        }
        colors.insert(key.into(), json!(color));
    }
    d.insert("bodyColors".into(), json!(colors));
    let mut scales = Map::new();
    for key in ["height", "width", "depth", "head", "bodyType", "proportion"] {
        if let Some(scale) = value["definition"]["scales"].get(key) {
            if !scale
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=5.0).contains(&n))
            {
                return Err(bad);
            }
            scales.insert(key.into(), scale.clone());
        }
    }
    d.insert("scales".into(), json!(scales));
    let rig = value["definition"]["playerAvatarType"]["playerAvatarType"]
        .as_str()
        .ok_or(bad)?;
    if !["R6", "R15"].contains(&rig) {
        return Err(bad);
    }
    d.insert("playerAvatarType".into(), json!({"playerAvatarType":rig}));
    let mut result = json!({"version":1,"userId":user,"definition":d});
    if channel == "image" {
        let url = value["imageUrl"].as_str().ok_or(bad)?;
        if !valid_image(url) {
            return Err(bad);
        }
        result["imageUrl"] = json!(url);
    } else {
        let items = value["items"].as_array().ok_or(bad)?;
        if items.len() > 100 {
            return Err(bad);
        }
        let mut safe_items = Vec::new();
        for item in items {
            let name = item["name"].as_str().ok_or(bad)?;
            if !asset(&item["id"])
                || !asset(&item["assetType"])
                || item["itemType"] != "Asset"
                || name.len() > 512
                || name.chars().any(char::is_control)
            {
                return Err(bad);
            }
            safe_items.push(json!({"id":item["id"],"name":name,"itemType":"Asset","assetType":item["assetType"]}));
        }
        result["items"] = json!(safe_items);
    }
    Ok(result)
}

fn error(status: StatusCode) -> Response {
    (status, Json(json!({"error":"Request rejected"}))).into_response()
}
async fn guard(req: axum::extract::Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host != "127.0.0.1:38261" && host != "localhost:38261" {
        return error(StatusCode::FORBIDDEN);
    }
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let extension = origin
        .and_then(|o| o.strip_prefix("chrome-extension://"))
        .is_some_and(|o| o.len() == 32 && o.bytes().all(|b| (b'a'..=b'p').contains(&b)));
    if origin.is_some_and(|o| o != "http://127.0.0.1:38261" && o != "http://localhost:38261")
        && !extension
    {
        return error(StatusCode::FORBIDDEN);
    }
    if req.uri().path() == "/v1/pair"
        && headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
        && !extension
    {
        return error(StatusCode::FORBIDDEN);
    }
    let mut response = match tokio::time::timeout(Duration::from_secs(10), next.run(req)).await {
        Ok(r) => r,
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}
async fn pair(State(s): State<Shared>) -> Json<Value> {
    Json(json!({"code":s.token}))
}
async fn status(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if !auth_header(&s, &headers) {
        return error(StatusCode::UNAUTHORIZED);
    }
    Json(json!({"version":1,"connected":true,"websocket":true,"instanceId":s.instance}))
        .into_response()
}
fn auth_header(s: &Bridge, h: &HeaderMap) -> bool {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|v| s.authorized(v))
}
async fn http(
    State(s): State<Shared>,
    Path((channel, user)): Path<(String, String)>,
    headers: HeaderMap,
    method: Method,
    body: Bytes,
) -> Response {
    if !auth_header(&s, &headers) {
        return error(StatusCode::UNAUTHORIZED);
    }
    if !id(&user) || !["game", "browser", "names", "image"].contains(&channel.as_str()) {
        return error(StatusCode::NOT_FOUND);
    }
    if method == Method::GET {
        return s
            .inner
            .lock()
            .unwrap()
            .values
            .get(&(user, channel))
            .cloned()
            .map(|v| Json(v).into_response())
            .unwrap_or_else(|| error(StatusCode::NOT_FOUND));
    }
    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    match s.publish(&channel, &value, &user) {
        Ok(v) => Json(v).into_response(),
        Err(_) => error(StatusCode::BAD_REQUEST),
    }
}
async fn upgrade(State(s): State<Shared>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_BODY)
        .max_frame_size(MAX_BODY)
        .on_upgrade(move |socket| session(s, socket))
}
async fn session(s: Shared, mut ws: WebSocket) {
    let message = match tokio::time::timeout(Duration::from_secs(5), ws.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => t,
        _ => return,
    };
    let auth = match serde_json::from_str::<Value>(&message) {
        Ok(v) => v,
        Err(_) => return,
    };
    let user = auth["userId"].as_str().unwrap_or("").to_owned();
    let role = auth["role"].as_str().unwrap_or("").to_owned();
    if auth["type"] != "auth"
        || auth["version"] != 1
        || !id(&user)
        || !["browser", "game"].contains(&role.as_str())
        || !s.authorized(auth["token"].as_str().unwrap_or(""))
    {
        let _ = ws.close().await;
        return;
    }
    let key = Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel::<Value>(32);
    let initial = {
        let mut inner = s.inner.lock().unwrap();
        if inner.clients.len() >= MAX_CLIENTS {
            return;
        }
        inner.clients.insert(
            key,
            Client {
                user: user.clone(),
                role: role.clone(),
                tx,
            },
        );
        inner
            .values
            .iter()
            .filter(|((u, c), _)| {
                u == &user
                    && if role == "browser" {
                        c == "game"
                    } else {
                        c != "game"
                    }
            })
            .map(|((_, c), v)| json!({"type":"state","channel":c,"value":v}))
            .collect::<Vec<_>>()
    };
    let (mut out, mut input) = ws.split();
    let _ = out
        .send(Message::Text(
            json!({"type":"ready","instanceId":s.instance})
                .to_string()
                .into(),
        ))
        .await;
    for value in initial {
        if out
            .send(Message::Text(value.to_string().into()))
            .await
            .is_err()
        {
            s.inner.lock().unwrap().clients.remove(&key);
            return;
        }
    }
    loop {
        tokio::select! {
            value=rx.recv()=>{match value {Some(v)=>if out.send(Message::Text(v.to_string().into())).await.is_err(){break},None=>break}}
            message=tokio::time::timeout(Duration::from_secs(60),input.next())=>{
                let text=match message {Ok(Some(Ok(Message::Text(t))))=>t,Ok(Some(Ok(Message::Ping(p))))=>{if out.send(Message::Pong(p)).await.is_err(){break}continue},Ok(Some(Ok(Message::Pong(_))))=>continue,_=>break};
                let value=match serde_json::from_str::<Value>(&text){Ok(v)=>v,Err(_)=>break};
                let response=if value["type"]=="ping" {json!({"type":"pong"})} else {
                    let channel=value["channel"].as_str().unwrap_or("");
                    if value["type"]!="publish" || !(if role=="game" {channel=="game"}else{["browser","names","image"].contains(&channel)}) {json!({"type":"error","error":"Invalid channel"})}
                    else {match s.publish(channel,&value["value"],&user){Ok(v)=>json!({"type":"ack","channel":channel,"value":v}),Err(_)=>json!({"type":"error","error":"Invalid appearance data"})}}
                };
                if out.send(Message::Text(response.to_string().into())).await.is_err(){break}
            }
        }
    }
    s.inner.lock().unwrap().clients.remove(&key);
}
pub async fn serve(s: Shared) {
    let app = Router::new()
        .route("/v1/pair", get(pair))
        .route("/v1/status", get(status))
        .route("/v1/socket", get(upgrade))
        .route("/v1/{channel}/{user}", get(http).post(http))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(guard))
        .with_state(s.clone());
    match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PORT)).await {
        Ok(listener) => {
            s.inner.lock().unwrap().ready = true;
            let _ = axum::serve(listener, app).await;
        }
        Err(_) => {
            s.inner.lock().unwrap().error = Some(
                "Another app is using the local connection. Close it and reopen Larpblox.".into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(user: &str) -> Value {
        json!({"version":1,"userId":user,"definition":{
            "assets":[{"id":123}],
            "bodyColors":{"headColor":"#ffffff","torsoColor":"ffffff","leftArmColor":"ffffff","rightArmColor":"ffffff","leftLegColor":"ffffff","rightLegColor":"ffffff"},
            "scales":{"height":1},"playerAvatarType":{"playerAvatarType":"R15"}},
            "items":[{"id":123,"name":"Hat","itemType":"Asset","assetType":8}]})
    }
    fn bridge() -> Bridge {
        Bridge {
            token: "0123456789abcdef0123456789abcdef".into(),
            instance: "test".into(),
            inner: Mutex::new(Inner::default()),
        }
    }
    #[test]
    fn cleans_cosmetic_data_and_rejects_other_accounts() {
        let mut value = sample("7");
        value["private"] = json!("discard");
        value["definition"]["assets"][0]["private"] = json!("discard");
        let cleaned = clean("game", &value, "7").unwrap();
        assert!(cleaned.get("private").is_none());
        assert!(cleaned["definition"]["assets"][0].get("private").is_none());
        assert_eq!(cleaned["definition"]["bodyColors"]["headColor"], "ffffff");
        assert!(clean("game", &value, "8").is_err());
        value["definition"]["scales"]["height"] = json!(6);
        assert!(clean("game", &value, "7").is_err());
    }
    #[test]
    fn rejects_unsafe_images_and_names() {
        for url in [
            "http://tr.rbxcdn.com/a",
            "https://tr.rbxcdn.com.evil.test/a",
            "https://tr.rbxcdn.com/a?secret=x",
            "https://user@tr.rbxcdn.com/a",
            "https://tr.rbxcdn.com/a#x",
        ] {
            assert!(!valid_image(url));
        }
        assert!(valid_image(
            "https://tr.rbxcdn.com/30DAY-abc-Png/420/420/Avatar/Png/noFilter"
        ));
        assert!(clean(
            "names",
            &json!({"version":1,"userId":"7","enabled":true,"name":"Valid_Name"}),
            "7"
        )
        .is_ok());
        assert!(clean(
            "names",
            &json!({"version":1,"userId":"7","enabled":true,"name":"Bad Name"}),
            "7"
        )
        .is_err());
    }
    #[tokio::test]
    async fn isolates_accounts_and_routes_roles() {
        let s = bridge();
        let (same, mut same_rx) = mpsc::channel(4);
        let (other, mut other_rx) = mpsc::channel(4);
        let (source, mut source_rx) = mpsc::channel(4);
        {
            let mut inner = s.inner.lock().unwrap();
            inner.clients.insert(
                Uuid::new_v4(),
                Client {
                    user: "7".into(),
                    role: "game".into(),
                    tx: same,
                },
            );
            inner.clients.insert(
                Uuid::new_v4(),
                Client {
                    user: "8".into(),
                    role: "game".into(),
                    tx: other,
                },
            );
            inner.clients.insert(
                Uuid::new_v4(),
                Client {
                    user: "7".into(),
                    role: "browser".into(),
                    tx: source,
                },
            );
        }
        let first = s.publish("browser", &sample("7"), "7").unwrap();
        assert_eq!(same_rx.try_recv().unwrap()["channel"], "browser");
        assert!(other_rx.try_recv().is_err());
        assert!(source_rx.try_recv().is_err());
        let next = s.publish("browser", &sample("7"), "7").unwrap();
        assert!(next["revision"].as_u64().unwrap() > first["revision"].as_u64().unwrap());
        assert!(s.authorized("0123456789abcdef0123456789abcdef"));
        assert!(!s.authorized("bad"));
    }
}
