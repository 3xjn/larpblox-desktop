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
    time::{Duration, Instant},
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
    avatar_lookup: bool,
    client_requests: bool,
}
struct AvatarLookup {
    browser: Uuid,
    game: Uuid,
    request_id: String,
    target: String,
    generic: bool,
}
#[derive(Default)]
struct Inner {
    values: HashMap<(String, String), Value>,
    clients: HashMap<Uuid, Client>,
    avatar_lookups: HashMap<Uuid, AvatarLookup>,
    activity: HashMap<(String, String), Instant>,
    revision: u64,
    ready: bool,
    starting: bool,
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
        let connected = |role: &str| {
            let mut users: HashSet<&str> = inner
                .clients
                .values()
                .filter(|c| c.role == role)
                .map(|c| c.user.as_str())
                .collect();
            users.extend(
                inner
                    .activity
                    .iter()
                    .filter(|((_, r), time)| r == role && time.elapsed() < Duration::from_secs(15))
                    .map(|((user, _), _)| user.as_str()),
            );
            users.len()
        };
        json!({"ready":inner.ready,"starting":inner.starting,"error":inner.error,"browsers":connected("browser"),"games":connected("game")})
    }
    fn touch(&self, user: &str, channel: &str, method: &Method) {
        let role = if (method == Method::GET) == (channel == "game") {
            "browser"
        } else {
            "game"
        };
        let mut inner = self.inner.lock().unwrap();
        inner
            .activity
            .retain(|_, time| time.elapsed() < Duration::from_secs(15));
        let key = (user.to_owned(), role.to_owned());
        if inner.activity.len() < 64 || inner.activity.contains_key(&key) {
            inner.activity.insert(key, Instant::now());
        }
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
    fn start_avatar_lookup(&self, browser: Uuid, value: &Value) -> Result<Uuid, &'static str> {
        let request_id = value["id"]
            .as_str()
            .filter(|v| Uuid::parse_str(v).is_ok())
            .ok_or("Invalid avatar request")?;
        let target = value["targetUserId"]
            .as_str()
            .filter(|v| id(v))
            .ok_or("Invalid Roblox account")?;
        let mut inner = self.inner.lock().unwrap();
        let user = &inner
            .clients
            .get(&browser)
            .filter(|c| c.role == "browser")
            .ok_or("Invalid avatar request")?
            .user;
        if inner.avatar_lookups.len() >= 32
            || inner
                .avatar_lookups
                .values()
                .filter(|v| v.browser == browser)
                .count()
                >= 4
        {
            return Err("An avatar lookup is already pending. Try again shortly.");
        }
        let (&game, client) = inner
            .clients
            .iter()
            .find(|(_, c)| c.user == *user && c.role == "game" && c.avatar_lookup)
            .ok_or("Connect your game and reload the Larpblox interface to copy an avatar.")?;
        let key = Uuid::new_v4();
        client
            .tx
            .try_send(json!({"type":"avatar-lookup","id":key.to_string(),"targetUserId":target}))
            .map_err(|_| "The game connection is busy. Try again.")?;
        inner.avatar_lookups.insert(
            key,
            AvatarLookup {
                browser,
                game,
                request_id: request_id.into(),
                target: target.into(),
                generic: false,
            },
        );
        Ok(key)
    }
    fn start_client_request(&self, browser: Uuid, value: &Value) -> Result<Uuid, &'static str> {
        let request_id = value["id"]
            .as_str()
            .filter(|v| Uuid::parse_str(v).is_ok())
            .ok_or("Invalid client request")?;
        let method = value["method"]
            .as_str()
            .filter(|v| {
                !v.is_empty()
                    && v.len() <= 64
                    && v.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_'
                    })
            })
            .ok_or("Invalid client method")?;
        let params = value["params"]
            .as_object()
            .filter(|v| serde_json::to_vec(v).is_ok_and(|v| v.len() <= 65536))
            .ok_or("Invalid client parameters")?;
        let mut inner = self.inner.lock().unwrap();
        let user = &inner
            .clients
            .get(&browser)
            .filter(|c| c.role == "browser")
            .ok_or("Invalid client request")?
            .user;
        if inner.avatar_lookups.len() >= 32
            || inner
                .avatar_lookups
                .values()
                .filter(|v| v.browser == browser)
                .count()
                >= 4
        {
            return Err("A client request is already pending. Try again shortly.");
        }
        let (&game, client) = inner
            .clients
            .iter()
            .find(|(_, c)| c.user == *user && c.role == "game" && c.client_requests)
            .ok_or("Connect your game and reload the Larpblox interface.")?;
        let key = Uuid::new_v4();
        client.tx.try_send(json!({"type":"client-request","id":key.to_string(),"method":method,"params":params})).map_err(|_|"The game connection is busy. Try again.")?;
        inner.avatar_lookups.insert(
            key,
            AvatarLookup {
                browser,
                game,
                request_id: request_id.into(),
                target: String::new(),
                generic: true,
            },
        );
        Ok(key)
    }
    fn finish_client_request(&self, game: Uuid, value: &Value) -> Result<(), &'static str> {
        let key = value["id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or("Invalid client response")?;
        let mut inner = self.inner.lock().unwrap();
        let Some(request) = inner.avatar_lookups.get(&key) else {
            return Ok(());
        };
        if request.game != game || !request.generic {
            return Err("Invalid client response");
        }
        let response = if value["ok"] == true {
            json!({"type":"client-result","id":request.request_id,"ok":true,"result":value["result"]})
        } else {
            let message = value["error"]
                .as_str()
                .filter(|v| v.len() <= 256 && !v.chars().any(char::is_control))
                .unwrap_or("The client request failed.");
            json!({"type":"client-result","id":request.request_id,"ok":false,"error":message})
        };
        let request = inner.avatar_lookups.remove(&key).unwrap();
        if let Some(client) = inner.clients.get(&request.browser) {
            let _ = client.tx.try_send(response);
        }
        Ok(())
    }
    fn finish_avatar_lookup(&self, game: Uuid, value: &Value) -> Result<(), &'static str> {
        let key = value["id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or("Invalid avatar response")?;
        let mut inner = self.inner.lock().unwrap();
        let Some(request) = inner.avatar_lookups.get(&key) else {
            return Ok(());
        };
        if request.game != game || request.generic {
            return Err("Invalid avatar response");
        }
        let response = if value["ok"] == true {
            let user = &inner.clients.get(&game).ok_or("Game disconnected")?.user;
            let snapshot = clean("game", &value["snapshot"], user)?;
            json!({"type":"avatar-lookup-result","id":request.request_id,"targetUserId":request.target,"ok":true,"snapshot":snapshot})
        } else {
            let message = value["error"]
                .as_str()
                .filter(|v| v.len() <= 256 && !v.chars().any(char::is_control))
                .unwrap_or("The client could not load this avatar. Try again.");
            json!({"type":"avatar-lookup-result","id":request.request_id,"ok":false,"error":message})
        };
        let request = inner.avatar_lookups.remove(&key).unwrap();
        if let Some(client) = inner.clients.get(&request.browser) {
            let _ = client.tx.try_send(response);
        }
        Ok(())
    }
    fn cancel_avatar_lookup(&self, key: Uuid, message: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(request) = inner.avatar_lookups.remove(&key) {
            if let Some(client) = inner.clients.get(&request.browser) {
                let kind = if request.generic {
                    "client-result"
                } else {
                    "avatar-lookup-result"
                };
                let _ = client.tx.try_send(
                    json!({"type":kind,"id":request.request_id,"ok":false,"error":message}),
                );
            }
        }
    }
    fn disconnect(&self, key: Uuid) {
        let keys: Vec<_> = {
            let mut inner = self.inner.lock().unwrap();
            inner.clients.remove(&key);
            inner
                .avatar_lookups
                .iter()
                .filter(|(_, v)| v.browser == key || v.game == key)
                .map(|(k, _)| *k)
                .collect()
        };
        for key in keys {
            self.cancel_avatar_lookup(key, "The game disconnected during the avatar lookup.");
        }
    }
}

pub(crate) fn id(value: &str) -> bool {
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
pub(crate) fn valid_image(value: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    value.len() <= 2048
        && RE
            .get_or_init(|| {
                regex::Regex::new(r"^https://(?:tr|t[0-9])\.rbxcdn\.com/[A-Za-z0-9_.~/-]+$")
                    .unwrap()
            })
            .is_match(value)
}
pub(crate) fn clean(channel: &str, value: &Value, user: &str) -> Result<Value, &'static str> {
    let bad = "Invalid appearance data";
    if value["version"] != 1 || value["userId"].as_str() != Some(user) {
        return Err(bad);
    }
    if channel == "names" {
        let name = value["name"].as_str().ok_or(bad)?;
        let display_name = value["displayName"].as_str().unwrap_or(name);
        if !value["enabled"].is_boolean()
            || (!name.is_empty()
                && (!(3..=20).contains(&name.len())
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')))
            || (value["enabled"] == true && display_name.is_empty())
            || display_name.chars().count() > 50
            || display_name.chars().any(char::is_control)
        {
            return Err(bad);
        }
        let mut result = json!({"version":1,"userId":user,"enabled":value["enabled"],"name":name,"displayName":display_name});
        for key in ["sharing", "verified"] {
            if let Some(flag) = value.get(key) {
                if !flag.is_boolean() {
                    return Err(bad);
                }
                result[key] = flag.clone();
            }
        }
        return Ok(result);
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
            for key in ["order", "puffiness", "version"] {
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
            for (key, min, max) in [
                ("position", -10.0, 10.0),
                ("rotation", -360.0, 360.0),
                ("scale", 0.000001, 5.0),
            ] {
                if let Some(vector) = meta.get(key) {
                    let mut safe_vector = Map::new();
                    for axis in ["X", "Y", "Z"] {
                        let value = vector.get(axis).ok_or(bad)?;
                        if !value.as_f64().is_some_and(|n| n.is_finite() && (min..=max).contains(&n)) {
                            return Err(bad);
                        }
                        safe_vector.insert(axis.into(), value.clone());
                    }
                    m.insert(key.into(), Value::Object(safe_vector));
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
    let background = value
        .get("backgroundId")
        .or_else(|| definition.get("backgroundId"))
        .or_else(|| {
            definition
                .get("backgroundRequestModel")
                .and_then(|v| v.get("id"))
        });
    if let Some(id) = background {
        if id != &json!(0) && !asset(id) {
            return Err(bad);
        }
        if id != &json!(0) {
            d.insert("backgroundId".into(), id.clone());
            d.insert("backgroundRequestModel".into(), json!({"id":id}));
        }
    }
    let mut result = json!({"version":1,"userId":user,"definition":d});
    if let Some(id) = background {
        result["backgroundId"] = id.clone();
    }
    if let Some(collection) = value.get("backgrounds") {
        let entries = collection.as_array().ok_or(bad)?;
        if entries.len() > 100 {
            return Err(bad);
        }
        let mut seen = HashSet::new();
        let mut backgrounds = Vec::new();
        for entry in entries {
            let name = entry["name"].as_str().ok_or(bad)?;
            if !asset(&entry["id"])
                || !seen.insert(entry["id"].as_u64().ok_or(bad)?)
                || name.chars().count() > 128
                || name.chars().any(char::is_control)
            {
                return Err(bad);
            }
            backgrounds.push(json!({"id":entry["id"],"name":name}));
        }
        if let Some(id) = background.filter(|id| *id != &json!(0)) {
            if !seen.contains(&id.as_u64().ok_or(bad)?) {
                return Err(bad);
            }
        }
        result["backgrounds"] = json!(backgrounds);
    }
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
    let local_http =
        origin.is_some_and(|o| ["http://127.0.0.1:38261", "http://localhost:38261"].contains(&o));
    // Volt supplies a ws:// Origin. Permit it only on the local upgrade path.
    let local_websocket = req.uri().path() == "/v1/socket"
        && origin.is_some_and(|o| ["ws://127.0.0.1:38261", "ws://localhost:38261"].contains(&o));
    if origin.is_some() && !local_http && !local_websocket && !extension {
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
    Json(json!({"version":1,"connected":true,"websocket":true,"sharing":true,"avatarLookup":true,"instanceId":s.instance}))
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
    s.touch(&user, &channel, &method);
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
    let (sharing_tx, sharing_rx) = tokio::sync::watch::channel(None);
    let sharing_task = tokio::spawn(super::sharing::run(sharing_rx, tx.clone()));
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
                avatar_lookup: auth["avatarLookup"] == true,
                client_requests: auth["clientRequests"] == true,
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
            json!({"type":"ready","sharing":true,"avatarLookup":true,"clientRequests":true,"instanceId":s.instance})
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
            s.disconnect(key);
            return;
        }
    }
    loop {
        tokio::select! {
            value=rx.recv()=>{match value {Some(v)=>if out.send(Message::Text(v.to_string().into())).await.is_err(){break},None=>break}}
            message=tokio::time::timeout(Duration::from_secs(60),input.next())=>{
                let text=match message {Ok(Some(Ok(Message::Text(t))))=>t,Ok(Some(Ok(Message::Ping(p))))=>{if out.send(Message::Pong(p)).await.is_err(){break}continue},Ok(Some(Ok(Message::Pong(_))))=>continue,_=>break};
                let value=match serde_json::from_str::<Value>(&text){Ok(v)=>v,Err(_)=>break};
                let response=if value["type"]=="ping" {json!({"type":"pong"})}
                else if value["type"]=="sharing" && role=="game" {
                    match super::sharing::Session::parse(&value["value"], &user) {
                        Ok(next)=>{sharing_tx.send_replace(next);json!({"type":"ack","channel":"sharing"})},
                        Err(message)=>json!({"type":"state","channel":"shared","value":{"type":"status","message":message}})
                    }
                } else if value["type"]=="client-request" && role=="browser" {
                    match s.start_client_request(key,&value) {
                        Ok(request)=>{let state=s.clone();tokio::spawn(async move {tokio::time::sleep(Duration::from_secs(30)).await;state.cancel_avatar_lookup(request,"The client request timed out. Try again.");});continue},
                        Err(message)=>json!({"type":"client-result","id":value["id"],"ok":false,"error":message})
                    }
                } else if value["type"]=="client-result" && role=="game" {
                    match s.finish_client_request(key,&value){Ok(())=>continue,Err(message)=>json!({"type":"error","error":message})}
                } else if value["type"]=="avatar-lookup" && role=="browser" {
                    match s.start_avatar_lookup(key, &value) {
                        Ok(request)=>{let state=s.clone();tokio::spawn(async move {tokio::time::sleep(Duration::from_secs(30)).await;state.cancel_avatar_lookup(request,"The client avatar lookup timed out. Try again.");});continue},
                        Err(message)=>json!({"type":"avatar-lookup-result","id":value["id"],"ok":false,"error":message})
                    }
                } else if value["type"]=="avatar-lookup-result" && role=="game" {
                    match s.finish_avatar_lookup(key, &value) {Ok(())=>continue,Err(message)=>json!({"type":"error","error":message})}
                } else {
                    let channel=value["channel"].as_str().unwrap_or("");
                    if value["type"]!="publish" || !(if role=="game" {channel=="game"}else{["browser","names","image"].contains(&channel)}) {json!({"type":"error","error":"Invalid channel"})}
                    else {match s.publish(channel,&value["value"],&user){Ok(v)=>json!({"type":"ack","channel":channel,"value":v}),Err(_)=>json!({"type":"error","error":"Invalid appearance data"})}}
                };
                if out.send(Message::Text(response.to_string().into())).await.is_err(){break}
            }
        }
    }
    s.disconnect(key);
    drop(sharing_tx);
    sharing_task.abort();
}
async fn bind_listener(
    s: &Shared,
    address: std::net::SocketAddr,
) -> Result<Option<tokio::net::TcpListener>, String> {
    {
        let mut inner = s.inner.lock().unwrap();
        if inner.ready || inner.starting {
            return Ok(None);
        }
        inner.starting = true;
        inner.error = None;
    }
    let result = tokio::net::TcpListener::bind(address).await;
    let mut inner = s.inner.lock().unwrap();
    inner.starting = false;
    match result {
        Ok(listener) => {
            inner.ready = true;
            Ok(Some(listener))
        }
        Err(error) => {
            let message = if error.kind() == std::io::ErrorKind::AddrInUse {
                "Another app is using the local connection. Close that app, then retry."
            } else {
                "Couldn’t start the local connection. Please retry."
            };
            inner.ready = false;
            inner.error = Some(message.into());
            Err(message.into())
        }
    }
}

pub async fn serve(s: Shared) -> Result<(), String> {
    let Some(listener) = bind_listener(&s, (std::net::Ipv4Addr::LOCALHOST, PORT).into()).await?
    else {
        return Ok(());
    };
    let app = Router::new()
        .route("/v1/pair", get(pair))
        .route("/v1/status", get(status))
        .route("/v1/socket", get(upgrade))
        .route("/v1/{channel}/{user}", get(http).post(http))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(guard))
        .with_state(s.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
        let mut inner = s.inner.lock().unwrap();
        inner.ready = false;
        inner.error = Some("The local connection stopped. Please retry.".into());
    });
    Ok(())
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
    #[tokio::test]
    async fn retries_after_port_is_released_and_ignores_duplicate_starts() {
        let occupied = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = occupied.local_addr().unwrap();
        let state = Arc::new(bridge());
        assert!(bind_listener(&state, address).await.is_err());
        assert_eq!(state.status()["ready"], false);
        assert_eq!(state.status()["starting"], false);
        assert!(state.status()["error"].is_string());
        drop(occupied);
        let listener = bind_listener(&state, address).await.unwrap().unwrap();
        assert_eq!(state.status()["ready"], true);
        assert!(state.status()["error"].is_null());
        assert!(bind_listener(&state, address).await.unwrap().is_none());
        drop(listener);
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
    fn preserves_background_selection_removal_and_collection() {
        let mut value = sample("7");
        value["backgroundId"] = json!(75789087422384_u64);
        value["backgrounds"] = json!([{"id":75789087422384_u64,"name":"Cat","private":"discard"}]);
        for channel in ["browser", "game"] {
            let cleaned = clean(channel, &value, "7").unwrap();
            assert_eq!(cleaned["backgroundId"], value["backgroundId"]);
            assert_eq!(
                cleaned["definition"]["backgroundRequestModel"]["id"],
                value["backgroundId"]
            );
            assert!(cleaned["backgrounds"][0].get("private").is_none());
        }
        value["backgroundId"] = json!(0);
        let cleared = clean("browser", &value, "7").unwrap();
        assert_eq!(cleared["backgroundId"], 0);
        assert!(cleared["definition"]
            .get("backgroundRequestModel")
            .is_none());
        value["backgroundId"] = json!(-1);
        assert!(clean("browser", &value, "7").is_err());
        value["backgroundId"] = json!(42);
        assert!(clean("browser", &value, "7").is_err());
        value["backgroundId"] = json!(0);
        value["backgrounds"] = json!([{"id":42,"name":"A"},{"id":42,"name":"B"}]);
        assert!(clean("browser", &value, "7").is_err());
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
        let cleaned = clean(
            "names",
            &json!({"version":1,"userId":"7","enabled":true,"name":"Valid_Name","displayName":"Friendly"}),
            "7",
        )
        .unwrap();
        assert_eq!(cleaned["displayName"], "Friendly");
        assert!(clean(
            "names",
            &json!({"version":1,"userId":"7","enabled":true,"name":"Valid_Name","displayName":""}),
            "7",
        ).is_err());
        assert!(clean(
            "names",
            &json!({"version":1,"userId":"7","enabled":true,"name":"Bad Name"}),
            "7"
        )
        .is_err());
    }
    #[test]
    fn preserves_optional_name_flags_and_rejects_invalid_types() {
        for verified in [false, true] {
            let cleaned = clean(
                "names",
                &json!({"version":1,"userId":"7","enabled":true,"name":"Valid_Name","sharing":true,"verified":verified}),
                "7",
            )
            .unwrap();
            assert_eq!(cleaned["sharing"], true);
            assert_eq!(cleaned["verified"], verified);
        }
        for key in ["sharing", "verified"] {
            let mut value = json!({"version":1,"userId":"7","enabled":true,"name":"Valid_Name"});
            value[key] = json!("yes");
            assert!(clean("names", &value, "7").is_err());
        }
    }
    #[test]
    fn http_fallback_counts_as_connected_and_expires() {
        let s = bridge();
        s.touch("7", "game", &Method::POST);
        s.touch("7", "game", &Method::GET);
        assert_eq!(s.status()["games"], 1);
        assert_eq!(s.status()["browsers"], 1);
        for time in s.inner.lock().unwrap().activity.values_mut() {
            *time = Instant::now() - Duration::from_secs(16);
        }
        assert_eq!(s.status()["games"], 0);
        assert_eq!(s.status()["browsers"], 0);
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
                    avatar_lookup: false,
                    client_requests: false,
                },
            );
            inner.clients.insert(
                Uuid::new_v4(),
                Client {
                    user: "8".into(),
                    role: "game".into(),
                    tx: other,
                    avatar_lookup: false,
                    client_requests: false,
                },
            );
            inner.clients.insert(
                Uuid::new_v4(),
                Client {
                    user: "7".into(),
                    role: "browser".into(),
                    tx: source,
                    avatar_lookup: false,
                    client_requests: false,
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
    #[test]
    fn avatar_lookups_are_correlated_isolated_and_ephemeral() {
        let s = bridge();
        let browser = Uuid::new_v4();
        let game = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (browser_tx, mut browser_rx) = mpsc::channel(8);
        let (game_tx, mut game_rx) = mpsc::channel(8);
        let (other_tx, mut other_rx) = mpsc::channel(8);
        {
            let mut inner = s.inner.lock().unwrap();
            inner.clients.insert(
                browser,
                Client {
                    user: "7".into(),
                    role: "browser".into(),
                    tx: browser_tx,
                    avatar_lookup: false,
                    client_requests: false,
                },
            );
            inner.clients.insert(
                game,
                Client {
                    user: "7".into(),
                    role: "game".into(),
                    tx: game_tx,
                    avatar_lookup: true,
                    client_requests: true,
                },
            );
            inner.clients.insert(
                other,
                Client {
                    user: "8".into(),
                    role: "game".into(),
                    tx: other_tx,
                    avatar_lookup: true,
                    client_requests: true,
                },
            );
        }
        assert!(s
            .start_avatar_lookup(
                game,
                &json!({"id":Uuid::new_v4().to_string(),"targetUserId":"99"})
            )
            .is_err());
        assert!(s
            .start_avatar_lookup(browser, &json!({"id":"bad","targetUserId":"99"}))
            .is_err());
        let request_id = Uuid::new_v4();
        let key = s
            .start_avatar_lookup(
                browser,
                &json!({"id":request_id.to_string(),"targetUserId":"99"}),
            )
            .unwrap();
        let sent = game_rx.try_recv().unwrap();
        assert_eq!(sent["id"], key.to_string());
        assert_eq!(sent["targetUserId"], "99");
        assert!(other_rx.try_recv().is_err());
        assert!(s
            .finish_avatar_lookup(
                other,
                &json!({"id":key.to_string(),"ok":true,"snapshot":sample("8")})
            )
            .is_err());
        assert!(s
            .finish_avatar_lookup(
                game,
                &json!({"id":key.to_string(),"ok":true,"snapshot":sample("8")})
            )
            .is_err());
        s.finish_avatar_lookup(
            game,
            &json!({"id":key.to_string(),"ok":true,"snapshot":sample("7")}),
        )
        .unwrap();
        let result = browser_rx.try_recv().unwrap();
        assert_eq!(result["id"], request_id.to_string());
        assert_eq!(result["targetUserId"], "99");
        assert_eq!(result["snapshot"]["items"][0]["id"], 123);
        assert!(
            s.inner.lock().unwrap().values.is_empty(),
            "lookup must not equip or persist the returned outfit"
        );
        assert!(s.inner.lock().unwrap().avatar_lookups.is_empty());
        s.finish_avatar_lookup(
            game,
            &json!({"id":key.to_string(),"ok":true,"snapshot":sample("7")}),
        )
        .unwrap();
        assert!(
            browser_rx.try_recv().is_err(),
            "late duplicate results are ignored"
        );
        let client_id = Uuid::new_v4();
        let client_key=s.start_client_request(browser,&json!({"id":client_id.to_string(),"method":"future.feature","params":{"value":42}})).unwrap();
        let request = game_rx.try_recv().unwrap();
        assert_eq!(request["method"], "future.feature");
        assert_eq!(request["params"]["value"], 42);
        assert!(s
            .finish_client_request(
                other,
                &json!({"id":client_key.to_string(),"ok":true,"result":42})
            )
            .is_err());
        s.finish_client_request(
            game,
            &json!({"id":client_key.to_string(),"ok":true,"result":{"value":42}}),
        )
        .unwrap();
        let response = browser_rx.try_recv().unwrap();
        assert_eq!(response["type"], "client-result");
        assert_eq!(response["id"], client_id.to_string());
        assert_eq!(response["result"]["value"], 42);
        assert!(s
            .start_client_request(
                browser,
                &json!({"id":client_id.to_string(),"method":"eval()","params":{}})
            )
            .is_err());
        assert!(s.start_client_request(browser,&json!({"id":client_id.to_string(),"method":"valid.method","params":"not an object"})).is_err());
        let key = s
            .start_avatar_lookup(
                browser,
                &json!({"id":Uuid::new_v4().to_string(),"targetUserId":"99"}),
            )
            .unwrap();
        s.cancel_avatar_lookup(key, "Timed out");
        assert_eq!(browser_rx.try_recv().unwrap()["error"], "Timed out");
        s.start_avatar_lookup(
            browser,
            &json!({"id":Uuid::new_v4().to_string(),"targetUserId":"99"}),
        )
        .unwrap();
        s.disconnect(game);
        assert!(browser_rx.try_recv().unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("disconnected"));
        assert!(s
            .start_avatar_lookup(
                browser,
                &json!({"id":Uuid::new_v4().to_string(),"targetUserId":"99"})
            )
            .is_err());
    }
}
