use crate::{
    app::App,
    config::Config,
    model::{self, Action, Command, Role},
};
use anyhow::{ensure, Context, Result};
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS, Transport};
use serde_json::{json, Value};
use std::{io::BufReader, sync::Arc, time::Duration};

fn tls(cfg: &crate::config::Mqtt, data: &std::path::Path) -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = &cfg.ca_file {
        let pem = std::fs::read(path)?;
        ensure!(pem.len() <= 1_048_576, "CA file too large");
        let certs = rustls_pemfile::certs(&mut BufReader::new(pem.as_slice()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(!certs.is_empty(), "empty CA file");
        for c in certs {
            roots.add(c)?;
        }
    }
    let roots = Arc::new(roots);
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots.clone());
    let mut config = if let (Some(cert), Some(key)) = (&cfg.client_certificate, &cfg.client_key) {
        let certs = rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(cert)?))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(key)?))?
            .context("missing client key")?;
        builder.with_client_auth_cert(certs, key)?
    } else {
        builder.with_no_client_auth()
    };
    if cfg.certificate_name.is_some() || cfg.allow_self_signed {
        config
            .dangerous()
            .set_certificate_verifier(crate::tls::verifier(
                roots,
                cfg.certificate_name.clone(),
                cfg.allow_self_signed,
                format!("mqtt://{}:{}", cfg.host, cfg.port),
                data,
            )?);
    }
    Ok(config)
}
fn decode(name: &str, payload: &[u8]) -> Result<Command> {
    if name == "COMMAND" {
        return Ok(serde_json::from_slice(payload)?);
    }
    let mut value: Value = serde_json::from_slice(payload)?;
    let id = value.get_mut("id").context("command id required")?.take();
    let issued_at = value
        .get_mut("issued_at")
        .context("issued_at required")?
        .take();
    let object = value.as_object_mut().context("JSON object required")?;
    object.remove("id");
    object.remove("issued_at");
    ensure!(
        !object.contains_key("type"),
        "type is derived from the command topic"
    );
    let kind = match name {
        "SCHEDULE" => "set_schedules",
        "DELETE" => "delete_media",
        "EVENT" => "external_event",
        "TOSS" => "toss",
        "LED" => "led",
        "TUNNEL" => "tunnel",
        "NIGHT" => "night",
        "VOLUME" => "volume",
        "MUTE" => "mute",
        "IMAGE" => "image",
        "CAPTURE" => "capture",
        "RECORD" => "record",
        "VIDEO" => "video",
        "PLAY" => "play",
        "UPLOAD" => "upload",
        "WIFI" => "wifi_stage",
        "CONFIGURE" => "configure",
        "WIFICONFIRM" => "wifi_confirm",
        "WIFIROLLBACK" => "wifi_rollback",
        _ => anyhow::bail!("unknown command"),
    };
    object.insert("type".into(), json!(kind));
    Ok(serde_json::from_value(
        json!({"id":id,"issued_at":issued_at,"action":value}),
    )?)
}
fn admin_action(a: &Action) -> bool {
    matches!(
        a,
        Action::WifiStage { .. }
            | Action::WifiConfirm { .. }
            | Action::WifiRollback {}
            | Action::Configure { .. }
    )
}
pub async fn run(app: Arc<App>) -> Result<()> {
    let cfg = &app.cfg.mqtt;
    let base = cfg.topic_prefix.trim_end_matches('/').to_owned();
    let mut options = MqttOptions::new(format!("furbo-{}", app.cfg.device_id), &cfg.host, cfg.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(true);
    options.set_max_packet_size(16384, 16384);
    options.set_inflight(4);
    options.set_last_will(LastWill::new(
        format!("{base}/tele/LWT"),
        "Offline",
        QoS::AtLeastOnce,
        true,
    ));
    if let Some(username) = &cfg.username {
        options.set_credentials(
            username,
            cfg.password
                .clone()
                .or(Config::secret(&cfg.password_file)?)
                .unwrap_or_default(),
        );
    }
    if cfg.tls {
        options.set_transport(Transport::tls_with_config(
            tls(cfg, &app.cfg.data_dir)?.into(),
        ));
    }
    let (client, mut poll) = AsyncClient::new(options, 16);
    let mut events = app.events.subscribe();
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::channel::<(String, Value)>(8);
    let mut status = tokio::time::interval(Duration::from_secs(30));
    loop {
        if cfg.tls && !model::clock_valid() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        tokio::select! {
            event=poll.poll()=>match event{
                Ok(Event::Incoming(Incoming::ConnAck(_)))=>{
                    app.mqtt_connected.store(true,std::sync::atomic::Ordering::Relaxed);
                    let _=client.try_subscribe(format!("{base}/cmnd/+"),QoS::AtLeastOnce);
                    let _=client.try_publish(format!("{base}/tele/LWT"),QoS::AtLeastOnce,true,"Online");
                },
                Ok(Event::Incoming(Incoming::Publish(p)))=>{
                    let Some(name)=p.topic.strip_prefix(&format!("{base}/cmnd/"))else{continue;};
                    if p.retain{continue;}
                    if name.len()>24||!name.bytes().all(|c|c.is_ascii_uppercase()){continue;}
                    let tx=ack_tx.clone();let app=app.clone();let name=name.to_owned();let payload=p.payload.clone();
                    tokio::spawn(async move{
                        let result=async{
                            if ["STATUS","STATS","WIFISTATUS","SCHEDULES","MEDIA","SETTINGS"].contains(&name.as_str()){
                                let v:Value=serde_json::from_slice(&payload)?;
                                let id=v["id"].as_str().context("id required")?;let issued=v["issued_at"].as_i64().context("issued_at required")?;
                                ensure!(model::valid_id(id)&&model::now().abs_diff(issued)<=app.cfg.mqtt.max_command_age_seconds as u64,"invalid or stale request");
                                let result=match name.as_str(){"SETTINGS"=>crate::settings::view(&app.cfg),"STATS"=>crate::stats::collect(&app.cfg),"WIFISTATUS"=>app.hw(crate::hardware::Request::WifiStatus,&[]).await?.value,
                                    "SCHEDULES"=>json!(app.db.lock().await.schedules),
                                    "MEDIA"=>{let items=app.list_media()?;let total=items.len();let offset=v["offset"].as_u64().unwrap_or(0)as usize;let limit=v["limit"].as_u64().unwrap_or(24).min(32)as usize;json!({"items":items.into_iter().rev().skip(offset).take(limit).collect::<Vec<_>>(),"total":total,"offset":offset})},_=>app.status().await};
                                return Ok(json!({"id":id,"ok":true,"result":result}));
                            }
                            let cmd=decode(&name,&payload)?;let role=if admin_action(&cmd.action){Role::Admin}else{Role::Operator};
                            app.execute(cmd,role,None).await
                        }.await;
                        let id=serde_json::from_slice::<Value>(&payload).ok().and_then(|v|v["id"].as_str().filter(|s|model::valid_id(s)).map(str::to_owned));
                        let response=match result{Ok(v)=>v,Err(e)=>json!({"id":id,"ok":false,"error":e.to_string()})};
                        let suffix=if name=="COMMAND"{"RESULT".to_owned()}else{name};
                        let _=tx.try_send((suffix,response));
                    });
                },
                Ok(_)=>{},
                Err(_)=>{app.mqtt_connected.store(false,std::sync::atomic::Ordering::Relaxed);crate::log_error!("MQTT disconnected; retrying");tokio::time::sleep(Duration::from_secs(3)).await;},
            },
            received=events.recv()=>if let Ok(event)=received{let _=client.try_publish(format!("{base}/tele/EVENT"),QoS::AtLeastOnce,false,serde_json::to_vec(&event)?);},
            Some((suffix,result))=ack_rx.recv()=>{let _=client.try_publish(format!("{base}/stat/{suffix}"),QoS::AtLeastOnce,false,serde_json::to_vec(&result)?);},
            _=status.tick()=>{let value=json!({"state":app.status().await,"stats":crate::stats::collect(&app.cfg)});let _=client.try_publish(format!("{base}/tele/STATE"),QoS::AtLeastOnce,true,serde_json::to_vec(&value)?);},
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_commands() {
        let c = decode(
            "WIFI",
            br#"{"id":"setup-1","issued_at":1790000000,"ssid":"home","passphrase":"abcdefgh"}"#,
        )
        .unwrap();
        assert!(matches!(c.action, Action::WifiStage { .. }));
        assert!(decode("TOSS", br#"{"id":"a","issued_at":1,"extra":"no"}"#).is_err());
    }
}
