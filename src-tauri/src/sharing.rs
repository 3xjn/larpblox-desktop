use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
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
        Ok(Some(Self {
            user: user.into(),
            place: place.into(),
            job: job.into(),
            record: cleaned,
        }))
    }
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
                                if socket.send(Message::Text(json!({"type":"publish","record":next.record}).to_string().into())).await.is_err(){break}
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
                                    if ["snapshot","upsert","remove"].contains(&event["type"].as_str().unwrap_or("")) {
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
}
