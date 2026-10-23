use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use std::collections::HashSet;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Session {
    user: String,
    place: String,
    job: String,
    record: Value,
}

impl Session {
    pub(crate) fn parse(value: &Value, user: &str) -> Result<Option<Self>, &'static str> {
        if value["enabled"] == false {
            return Ok(None);
        }
        if value["enabled"] != true {
            return Err("Invalid sharing state");
        }
        let place = value["placeId"].as_str().ok_or("Invalid place")?;
        let job = value["jobId"].as_str().ok_or("Invalid server")?;
        if !super::bridge::id(place)
            || job.is_empty()
            || job.len() > 80
            || !job.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Join a live Roblox server before sharing");
        }
        let record = &value["record"];
        if record["version"] != 1 || record["userId"].as_str() != Some(user) {
            return Err("Invalid sharing identity");
        }
        let snapshot = super::bridge::clean("game", &record["snapshot"], user)?;
        let name = &record["name"];
        let username = name["username"].as_str().ok_or("Invalid shared name")?;
        let display = name["displayName"].as_str().ok_or("Invalid shared name")?;
        if !name["enabled"].is_boolean()
            || username.len() > 20
            || display.chars().count() > 50
            || !username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || display.chars().any(char::is_control)
            || (name["enabled"] == true && (username.len() < 3 || display.is_empty()))
        {
            return Err("Invalid shared name");
        }
        let mut cleaned = json!({"version":1,"userId":user,"snapshot":snapshot,"name":name});
        if let Some(verified) = record.get("verified") {
            if !verified.is_boolean() {
                return Err("Invalid sharing identity");
            }
            cleaned["verified"] = verified.clone();
        }
        if let Some(image) = record.get("imageUrl") {
            if !image.as_str().is_some_and(super::bridge::valid_image) {
                return Err("Invalid shared image");
            }
            cleaned["imageUrl"] = image.clone();
        }
        if let Some(hat_scripts) = record.get("hatScripts") {
            cleaned["hatScripts"] = clean_hat_scripts(hat_scripts)?;
        }
        Ok(Some(Self {
            user: user.into(),
            place: place.into(),
            job: job.into(),
            record: cleaned,
        }))
    }
}

fn clean_hat_scripts(value: &Value) -> Result<Value, &'static str> {
    let object = value.as_object().ok_or("Invalid hat scripts")?;
    if object.get("version") != Some(&json!(1)) {
        return Err("Invalid hat scripts");
    }
    let session = object.get("session").and_then(Value::as_str).ok_or("Invalid hat scripts")?;
    if session.len() != 36 || !session.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') {
        return Err("Invalid hat scripts");
    }
    let held = object.get("held").and_then(Value::as_str).ok_or("Invalid hat scripts")?;
    if !matches!(held, "" | "award" | "doom") {
        return Err("Invalid hat scripts");
    }
    let events = object.get("events").and_then(Value::as_array).ok_or("Invalid hat scripts")?;
    if events.len() > 16 {
        return Err("Invalid hat scripts");
    }
    let kinds = ["turret-fire", "doom-satellite", "doom-beam", "doom-vanish", "doom-stop", "award-burst", "award-gild"];
    let mut ids = HashSet::new();
    let satellite = match object.get("satellite") {
        Some(Value::Array(position)) if position.len() == 3 && position.iter().all(|value| value.as_f64().is_some_and(|number| number.is_finite() && number.abs() <= 1_000_000.0)) => Some(position),
        Some(_) => return Err("Invalid hat scripts"),
        None => None,
    };
    let mut clean_events = Vec::with_capacity(events.len());
    for event in events {
        let event = event.as_object().ok_or("Invalid hat event")?;
        let id = event.get("id").and_then(Value::as_u64).filter(|id| *id > 0 && *id <= 9_007_199_254_740_991).ok_or("Invalid hat event")?;
        if !ids.insert(id) {
            return Err("Invalid hat event");
        }
        let kind = event.get("kind").and_then(Value::as_str).filter(|kind| kinds.contains(kind)).ok_or("Invalid hat event")?;
        let at = event.get("at").and_then(Value::as_u64).filter(|at| *at > 0 && *at <= 9_007_199_254_740_991).ok_or("Invalid hat event")?;
        let target = event.get("target");
        let target = match target {
            Some(Value::String(target)) if !target.is_empty() && target.len() <= 16 && target.bytes().all(|byte| byte.is_ascii_digit()) && !target.starts_with('0') => Some(target),
            Some(_) => return Err("Invalid hat event"),
            None => None,
        };
        if kind == "turret-fire" && target.is_none() {
            return Err("Invalid hat event");
        }
        let position = event.get("position");
        let needs_position = matches!(kind, "doom-satellite" | "doom-beam" | "doom-vanish" | "award-gild");
        let position = match position {
            Some(Value::Array(position)) if position.len() == 3 && position.iter().all(|value| value.as_f64().is_some_and(|number| number.is_finite() && number.abs() <= 1_000_000.0)) => Some(position),
            Some(_) => return Err("Invalid hat event"),
            None if needs_position => return Err("Invalid hat event"),
            None => None,
        };
        let mut clean = json!({"id": id, "kind": kind, "at": at});
        if let Some(target) = target { clean["target"] = json!(target); }
        if let Some(position) = position { clean["position"] = json!(position); }
        clean_events.push(clean);
    }
    let mut clean = json!({"version": 1, "session": session, "held": held, "events": clean_events});
    if let Some(satellite) = satellite { clean["satellite"] = json!(satellite); }
    Ok(clean)
}

fn same_without_hat_scripts(left: &Value, right: &Value) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    if let Some(object) = left.as_object_mut() { object.remove("hatScripts"); }
    if let Some(object) = right.as_object_mut() { object.remove("hatScripts"); }
    left == right
}

fn forward(tx: &mpsc::Sender<Value>, value: Value) -> Result<(), ()> {
    tx.try_send(json!({"type":"state","channel":"shared","value":value}))
        .map_err(|_| ())
}

// A game socket owns one sharing connection. Dropping the game closes this task.
pub(crate) async fn run(mut changes: watch::Receiver<Option<Session>>, tx: mpsc::Sender<Value>) {
    let mut retry = 1;
    loop {
        let desired = changes.borrow_and_update().clone();
        let Some(mut current) = desired else {
            if changes.changed().await.is_err() {
                return;
            }
            continue;
        };
        let connection = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(8),connect_async_with_config(
                "wss://larp.3xjn.dev/v1/socket",
                Some(WebSocketConfig::default().max_message_size(Some(4*1024*1024)).max_frame_size(Some(4*1024*1024))),
                false
            ))=>result,
            result=changes.changed()=>{if result.is_err(){return}continue}
        };
        if let Ok(Ok((mut socket, _))) = connection {
            let join = json!({"version":1,"type":"join","userId":current.user,"placeId":current.place,"jobId":current.job,"record":current.record});
            if socket
                .send(Message::Text(join.to_string().into()))
                .await
                .is_ok()
            {
                let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
                let mut received = Instant::now();
                loop {
                    tokio::select! {
                        changed=changes.changed()=>{
                            if changed.is_err(){let _=socket.close(None).await;return}
                            let next=changes.borrow_and_update().clone();
                            let Some(next)=next else {break};
                            if next.user!=current.user || next.place!=current.place || next.job!=current.job {break}
                            if next.record!=current.record {
                                let compact_hat = next.record.get("hatScripts").is_some()
                                    && current.record.get("hatScripts") != next.record.get("hatScripts")
                                    && same_without_hat_scripts(&current.record, &next.record);
                                let message = if compact_hat {
                                    json!({"type":"hat-scripts","hatScripts":next.record["hatScripts"]})
                                } else {
                                    json!({"type":"publish","record":next.record})
                                };
                                if socket.send(Message::Text(message.to_string().into())).await.is_err(){break}
                            }
                            current=next;
                        }
                        _=heartbeat.tick()=>{
                            if received.elapsed()>Duration::from_secs(35){break}
                            if socket.send(Message::Text("{\"type\":\"ping\"}".into())).await.is_err(){break}
                        }
                        incoming=socket.next()=>{
                            match incoming {
                                Some(Ok(Message::Text(text)))=>{
                                    received=Instant::now();
                                    if text.len()>4*1024*1024 {break}
                                    let Ok(event)=serde_json::from_str::<Value>(&text) else {break};
                                    if ["snapshot","upsert","remove","hat-scripts"].contains(&event["type"].as_str().unwrap_or("")) {
                                        retry=1;
                                        if forward(&tx,event).is_err(){break}
                                    }
                                }
                                Some(Ok(Message::Ping(_)))|Some(Ok(Message::Pong(_)))=>{received=Instant::now();}
                                _=>break,
                            }
                        }
                    }
                }
                let _ = socket.close(None).await;
            }
        }
        if changes.borrow().as_ref() != Some(&current) {
            continue;
        }
        let _ = forward(
            &tx,
            json!({"type":"status","message":"Shared connection interrupted; reconnecting…"}),
        );
        tokio::select! {
            _=tokio::time::sleep(Duration::from_secs(retry))=>{},
            result=changes.changed()=>{if result.is_err(){return}}
        }
        retry = (retry * 2).min(15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Value {
        json!({"version":1,"userId":"7","definition":{
            "assets":[{"id":123}],
            "bodyColors":{"headColor":"ffffff","torsoColor":"ffffff","leftArmColor":"ffffff","rightArmColor":"ffffff","leftLegColor":"ffffff","rightLegColor":"ffffff"},
            "scales":{},"playerAvatarType":{"playerAvatarType":"R15"}},
            "items":[]})
    }

    #[test]
    fn preserves_verified_flag_and_rejects_invalid_type() {
        for verified in [false, true] {
            let value = json!({"enabled":true,"placeId":"123","jobId":"job-1","record":{
                "version":1,"userId":"7","verified":verified,"snapshot":snapshot(),
                "name":{"enabled":true,"username":"Player","displayName":"Player"}}});
            let session = Session::parse(&value, "7").unwrap().unwrap();
            assert_eq!(session.record["verified"], verified);
        }
        let invalid = json!({"enabled":true,"placeId":"123","jobId":"job-1","record":{
            "version":1,"userId":"7","verified":"yes","snapshot":snapshot(),
            "name":{"enabled":true,"username":"Player","displayName":"Player"}}});
        assert!(Session::parse(&invalid, "7").is_err());
    }

    #[test]
    fn preserves_sanitized_hat_scripts_and_legacy_records() {
        let mut value = json!({"enabled":true,"placeId":"123","jobId":"job-1","record":{
            "version":1,"userId":"7","snapshot":snapshot(),
            "name":{"enabled":true,"username":"Player","displayName":"Player"}}});
        assert!(Session::parse(&value, "7").unwrap().unwrap().record.get("hatScripts").is_none());
        value["record"]["hatScripts"] = json!({"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"doom",
            "satellite":[1,-2.5,3],"events":[{"id":1,"kind":"doom-beam","at":1700000000,"position":[4,5,6],"target":"7","script":"discard"}]});
        let session = Session::parse(&value, "7").unwrap().unwrap();
        assert_eq!(session.record["hatScripts"]["events"][0], json!({"id":1,"kind":"doom-beam","at":1700000000,"target":"7","position":[4,5,6]}));
        assert_eq!(session.record["hatScripts"]["satellite"], json!([1,-2.5,3]));
        for invalid in [
            json!({"version":1,"session":"short","held":"doom","events":[]}),
            json!({"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"doom","events":[{"id":1,"kind":"turret-fire","at":1}]}),
            json!({"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"doom","events":[{"id":1,"kind":"doom-beam","at":1,"position":[0,0]}]}),
        ] {
            value["record"]["hatScripts"] = invalid;
            assert!(Session::parse(&value, "7").is_err());
        }
    }

    #[test]
    fn compact_hat_delta_only_matches_hat_changes() {
        let base = json!({"version":1,"userId":"7","snapshot":snapshot(),"hatScripts":{"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"","events":[]}});
        let mut hats = base.clone();
        hats["hatScripts"]["held"] = json!("doom");
        assert!(same_without_hat_scripts(&base, &hats));
        let mut appearance = hats.clone();
        appearance["userId"] = json!("8");
        assert!(!same_without_hat_scripts(&base, &appearance));
        let duplicate = json!({"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"","events":[
            {"id":1,"kind":"doom-stop","at":1},{"id":1,"kind":"doom-stop","at":2}]});
        assert!(clean_hat_scripts(&duplicate).is_err());
        let oversized = json!({"version":1,"session":"01234567-89ab-cdef-0123-456789abcdef","held":"","events":
            (0..17).map(|id| json!({"id":id + 1,"kind":"doom-stop","at":1})).collect::<Vec<_>>()});
        assert!(clean_hat_scripts(&oversized).is_err());
    }
}
