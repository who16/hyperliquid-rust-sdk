use std::{
    borrow::BorrowMut,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpStream,
    spawn,
    sync::{mpsc::UnboundedSender, Mutex},
    time,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, protocol},
    MaybeTlsStream, WebSocketStream,
};
pub type ReponseMessageSender = UnboundedSender<PostResponseMessage>;
use crate::ws::sub_structs::PostData;
use crate::{prelude::*, Error};

#[derive(Serialize)]
struct PostRequest<'a> {
    method: &'static str,
    id: u64,
    request: &'a serde_json::Value,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "channel")]
#[serde(rename_all = "camelCase")]
pub enum PostResponseMessage {
    NoData,
    Post { data: PostData },
    Pong,
    HyperliquidError(String),
}
#[derive(Debug)]
pub(crate) struct PostWsManager {
    stop_flag: Arc<AtomicBool>,
    writer: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, protocol::Message>>>,
}

#[derive(Serialize)]
pub(crate) struct Ping {
    method: &'static str,
}

impl PostWsManager {
    const SEND_PING_INTERVAL: u64 = 50;

    pub(crate) async fn new(
        url: String,
        reconnect: bool,
        response_channel: Option<ReponseMessageSender>,
    ) -> Result<PostWsManager> {
        let stop_flag = Arc::new(AtomicBool::new(false));

        let (writer, mut reader) = Self::connect(&url).await?.split();
        let writer = Arc::new(Mutex::new(writer));

        {
            let stop_flag = Arc::clone(&stop_flag);
            let reader_fut = async move {
                while !stop_flag.load(Ordering::Relaxed) {
                    if let Some(data) = reader.next().await {
                        if let Some(ref rc) = response_channel {
                            if let Err(err) = PostWsManager::parse_and_send_data(data, rc).await {
                                error!("Error processing data received by WsManager reader: {err}");
                            }
                        }
                    } else {
                        if reconnect {
                            // Always sleep for 1 second before attempting to reconnect so it does not spin during reconnecting. This could be enhanced with exponential backoff.
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            info!("WsManager attempting to reconnect");
                            match Self::connect(&url).await {
                                Ok(ws) => {
                                    let (_, new_reader) = ws.split();
                                    reader = new_reader;
                                    info!("WsManager reconnect finished");
                                }
                                Err(err) => error!("Could not connect to websocket {err}"),
                            }
                        } else {
                            error!("WsManager reconnection disabled. Will not reconnect and exiting reader task.");
                            break;
                        }
                    }
                }
                warn!("ws message reader task stopped");
            };
            spawn(reader_fut);
        }

        {
            let stop_flag = Arc::clone(&stop_flag);
            let writer = Arc::clone(&writer);
            let ping_fut = async move {
                while !stop_flag.load(Ordering::Relaxed) {
                    match serde_json::to_string(&Ping { method: "ping" }) {
                        Ok(payload) => {
                            let mut writer = writer.lock().await;
                            if let Err(err) = writer.send(protocol::Message::Text(payload)).await {
                                error!("Error pinging server: {err}")
                            }
                        }
                        Err(err) => error!("Error serializing ping message: {err}"),
                    }
                    time::sleep(Duration::from_secs(Self::SEND_PING_INTERVAL)).await;
                }
                warn!("ws ping task stopped");
            };
            spawn(ping_fut);
        }

        Ok(PostWsManager { stop_flag, writer })
    }

    async fn connect(url: &str) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        Ok(connect_async(url)
            .await
            .map_err(|e| Error::Websocket(e.to_string()))?
            .0)
    }

    async fn parse_and_send_data(
        data: std::result::Result<protocol::Message, tungstenite::Error>,
        response_channel: &ReponseMessageSender,
    ) -> Result<()> {
        match data {
            Ok(data) => match data.into_text() {
                Ok(data) => {
                    if !data.starts_with('{') {
                        return Ok(());
                    }
                    let message = serde_json::from_str::<PostResponseMessage>(&data)
                        .map_err(|e| Error::JsonParse(e.to_string()))?;

                    response_channel
                        .send(message)
                        .map_err(|e| Error::WsSend(e.to_string()))?;
                    Ok(())
                }
                Err(err) => {
                    let error = Error::ReaderTextConversion(err.to_string());
                    response_channel
                        .send(PostResponseMessage::HyperliquidError(error.to_string()))
                        .map_err(|e| Error::WsSend(e.to_string()))?;

                    Ok(())
                }
            },
            Err(err) => {
                let error = Error::GenericReader(err.to_string());
                response_channel
                    .send(PostResponseMessage::HyperliquidError(error.to_string()))
                    .map_err(|e| Error::WsSend(e.to_string()))?;
                Ok(())
            }
        }
    }

    pub(crate) async fn send(&mut self, id: u64, data: String) -> Result<()> {
        let payload = serde_json::to_string(&PostRequest {
            method: "post",
            id,
            request: &serde_json::from_str::<serde_json::Value>(&data)
                .map_err(|e| Error::JsonParse(e.to_string()))?,
        })
        .map_err(|e| Error::JsonParse(e.to_string()))?;
        self.writer
            .lock()
            .await
            .borrow_mut()
            .send(protocol::Message::Text(payload))
            .await
            .map_err(|e| Error::Websocket(e.to_string()))?;
        Ok(())
    }
}

impl Drop for PostWsManager {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}
