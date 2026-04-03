use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{self, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, WebSocketStream};
use tokio_tungstenite::tungstenite::Error as WsError;

type WsStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReader = futures_util::stream::SplitStream<WsStream>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsRequest {
    pub id: String,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsResponse {
    pub id: Option<String>,
    pub data: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct WsConfig {
    pub url: String,
    pub ping_interval: Duration,
    pub reconnect_enabled: bool,
    pub max_reconnect_delay: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            ping_interval: Duration::from_secs(30),
            reconnect_enabled: true,
            max_reconnect_delay: Duration::from_secs(60),
            read_timeout: Duration::from_secs(90),
            write_timeout: Duration::from_secs(5),
        }
    }
}

enum ConnectionEvent {
    Reconnected(WsSink),
    Disconnected,
}

pub struct WebSocketManager {
    config: WsConfig,
    writer: Arc<Mutex<Option<WsSink>>>,
    stop_flag: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    reconnect_attempts: Arc<AtomicU64>,
    
    // Channels
    request_tx: mpsc::UnboundedSender<WsRequest>,
    request_rx: Arc<Mutex<mpsc::UnboundedReceiver<WsRequest>>>,
    
    response_tx: mpsc::UnboundedSender<WsResponse>,
    
    conn_event_tx: mpsc::Sender<ConnectionEvent>,
    conn_event_rx: Arc<Mutex<mpsc::Receiver<ConnectionEvent>>>,
}

impl WebSocketManager {
    pub fn new(config: WsConfig) -> (Self, mpsc::UnboundedReceiver<WsResponse>) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (response_tx, response_rx) = mpsc::unbounded_channel();
        let (conn_event_tx, conn_event_rx) = mpsc::channel(10);
        
        let manager = Self {
            config,
            writer: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            connected: Arc::new(AtomicBool::new(false)),
            reconnect_attempts: Arc::new(AtomicU64::new(0)),
            request_tx,
            request_rx: Arc::new(Mutex::new(request_rx)),
            response_tx,
            conn_event_tx,
            conn_event_rx: Arc::new(Mutex::new(conn_event_rx)),
        };
        
        (manager, response_rx)
    }
    
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Initial connection
        let (writer, reader) = self.connect().await?;
        
        {
            let mut writer_guard = self.writer.lock().await;
            *writer_guard = Some(writer);
        }
        
        self.connected.store(true, Ordering::Relaxed);
        self.reconnect_attempts.store(0, Ordering::Relaxed);
        
        // Start all tasks
        self.start_reader_task(reader);
        self.start_writer_task();
        self.start_ping_task();
        
        Ok(())
    }
    
    pub async fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.connected.store(false, Ordering::Relaxed);
        
        // Close the writer
        let mut writer_guard = self.writer.lock().await;
        if let Some(writer) = writer_guard.as_mut() {
            let _ = writer.close().await;
        }
        *writer_guard = None;
    }
    
    pub fn send_request(&self, request: WsRequest) -> Result<(), String> {
        self.request_tx
            .send(request)
            .map_err(|e| format!("Failed to queue request: {}", e))
    }
    
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    
    pub fn get_reconnect_attempts(&self) -> u64 {
        self.reconnect_attempts.load(Ordering::Relaxed)
    }
    
    async fn connect(&self) -> Result<(WsSink, WsReader), Box<dyn std::error::Error>> {
        log::info!("Connecting to WebSocket: {}", self.config.url);
        
        let (ws_stream, _) = connect_async(&self.config.url).await?;
        let (writer, reader) = ws_stream.split();
        
        log::info!("WebSocket connected successfully");
        Ok((writer, reader))
    }
    
    fn start_reader_task(&self, initial_reader: WsReader) {
        let stop_flag = Arc::clone(&self.stop_flag);
        let connected = Arc::clone(&self.connected);
        let reconnect_attempts = Arc::clone(&self.reconnect_attempts);
        let response_tx = self.response_tx.clone();
        let conn_event_tx = self.conn_event_tx.clone();
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut reader = initial_reader;
            
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    log::info!("Reader task stopping (stop flag set)");
                    break;
                }
                
                // Read with timeout
                let read_result = tokio::time::timeout(
                    config.read_timeout,
                    reader.next()
                ).await;
                
                match read_result {
                    Ok(Some(Ok(msg))) => {
                        reconnect_attempts.store(0, Ordering::Relaxed);
                        
                        match msg {
                            Message::Text(text) => {
                                log::debug!("Received text message: {}", text);
                                let response = WsResponse {
                                    id: None,
                                    data: text,
                                    is_error: false,
                                };
                                
                                if let Err(e) = response_tx.send(response) {
                                    log::error!("Failed to send response to channel: {}", e);
                                }
                            }
                            Message::Binary(data) => {
                                log::debug!("Received binary message: {} bytes", data.len());
                                let response = WsResponse {
                                    id: None,
                                    data: format!("Binary data: {} bytes", data.len()),
                                    is_error: false,
                                };
                                
                                if let Err(e) = response_tx.send(response) {
                                    log::error!("Failed to send response to channel: {}", e);
                                }
                            }
                            Message::Ping(_) => {
                                log::debug!("Received ping from server");
                            }
                            Message::Pong(_) => {
                                log::debug!("Received pong from server");
                            }
                            Message::Close(frame) => {
                                log::warn!("Received close frame: {:?}", frame);
                                connected.store(false, Ordering::Relaxed);
                                let _ = conn_event_tx.send(ConnectionEvent::Disconnected).await;
                                break;
                            }
                            Message::Frame(_) => {
                                log::debug!("Received raw frame");
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        log::error!("WebSocket error: {}", e);
                        connected.store(false, Ordering::Relaxed);
                        let _ = conn_event_tx.send(ConnectionEvent::Disconnected).await;
                        break;
                    }
                    Ok(None) => {
                        log::warn!("WebSocket stream ended");
                        connected.store(false, Ordering::Relaxed);
                        let _ = conn_event_tx.send(ConnectionEvent::Disconnected).await;
                        break;
                    }
                    Err(_) => {
                        log::error!("Read timeout - connection may be dead");
                        connected.store(false, Ordering::Relaxed);
                        let _ = conn_event_tx.send(ConnectionEvent::Disconnected).await;
                        break;
                    }
                }
            }
            
            // Reconnection logic
            if config.reconnect_enabled && !stop_flag.load(Ordering::Relaxed) {
                Self::reconnect_loop(
                    &config,
                    &stop_flag,
                    &connected,
                    &reconnect_attempts,
                    &conn_event_tx,
                ).await;
            } else {
                log::info!("Reader task stopped (reconnect disabled or stopped)");
            }
        });
    }
    
    async fn reconnect_loop(
        config: &WsConfig,
        stop_flag: &Arc<AtomicBool>,
        connected: &Arc<AtomicBool>,
        reconnect_attempts: &Arc<AtomicU64>,
        conn_event_tx: &mpsc::Sender<ConnectionEvent>,
    ) {
        while !stop_flag.load(Ordering::Relaxed) {
            let attempts = reconnect_attempts.fetch_add(1, Ordering::Relaxed);
            
            // Exponential backoff
            let backoff = Duration::from_secs(2u64.pow((attempts as u32).min(6)));
            let backoff = backoff.min(config.max_reconnect_delay);
            
            log::info!("Reconnecting in {:?} (attempt {})", backoff, attempts + 1);
            tokio::time::sleep(backoff).await;
            
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            
            match connect_async(&config.url).await {
                Ok((ws_stream, _)) => {
                    let (new_writer, new_reader) = ws_stream.split();
                    
                    log::info!("Reconnected successfully");
                    connected.store(true, Ordering::Relaxed);
                    
                    // Notify writer task
                    if let Err(e) = conn_event_tx.send(ConnectionEvent::Reconnected(new_writer)).await {
                        log::error!("Failed to notify writer of reconnection: {}", e);
                        break;
                    }
                    
                    // Continue with new reader in this task
                    Self::reader_loop(
                        new_reader,
                        config,
                        stop_flag,
                        connected,
                        reconnect_attempts,
                        conn_event_tx,
                    ).await;
                    
                    // If reader loop exits, continue reconnecting
                    if !stop_flag.load(Ordering::Relaxed) && config.reconnect_enabled {
                        continue;
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    log::error!("Reconnection failed: {}", e);
                    continue;
                }
            }
        }
    }
    
    async fn reader_loop(
        mut reader: WsReader,
        config: &WsConfig,
        stop_flag: &Arc<AtomicBool>,
        connected: &Arc<AtomicBool>,
        reconnect_attempts: &Arc<AtomicU64>,
        conn_event_tx: &mpsc::Sender<ConnectionEvent>,
    ) {
        // This is essentially the same as the main reader loop
        // Extracted for reuse after reconnection
        while !stop_flag.load(Ordering::Relaxed) {
            let read_result = tokio::time::timeout(
                config.read_timeout,
                reader.next()
            ).await;
            
            match read_result {
                Ok(Some(Ok(_))) => {
                    reconnect_attempts.store(0, Ordering::Relaxed);
                    // Process message (simplified here)
                }
                Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    connected.store(false, Ordering::Relaxed);
                    let _ = conn_event_tx.send(ConnectionEvent::Disconnected).await;
                    break;
                }
            }
        }
    }
    
    fn start_writer_task(&self) {
        let stop_flag = Arc::clone(&self.stop_flag);
        let connected = Arc::clone(&self.connected);
        let writer = Arc::clone(&self.writer);
        let request_rx = Arc::clone(&self.request_rx);
        let conn_event_rx = Arc::clone(&self.conn_event_rx);
        let config = self.config.clone();
        let response_tx = self.response_tx.clone();
        
        tokio::spawn(async move {
            let mut request_rx = request_rx.lock().await;
            let mut conn_event_rx = conn_event_rx.lock().await;
            
            while !stop_flag.load(Ordering::Relaxed) {
                tokio::select! {
                    Some(request) = request_rx.recv() => {
                        if !connected.load(Ordering::Relaxed) {
                            log::warn!("Cannot send request - not connected");
                            
                            let error_response = WsResponse {
                                id: Some(request.id),
                                data: "Not connected".to_string(),
                                is_error: true,
                            };
                            let _ = response_tx.send(error_response);
                            continue;
                        }
                        
                        let mut writer_guard = writer.lock().await;
                        if let Some(ws_writer) = writer_guard.as_mut() {
                            log::debug!("Sending request: {}", request.id);
                            
                            let send_result = tokio::time::timeout(
                                config.write_timeout,
                                ws_writer.send(Message::Text(request.payload.clone()))
                            ).await;
                            
                            match send_result {
                                Ok(Ok(_)) => {
                                    log::debug!("Request sent successfully: {}", request.id);
                                }
                                Ok(Err(e)) => {
                                    log::error!("Failed to send request {}: {}", request.id, e);
                                    
                                    let error_response = WsResponse {
                                        id: Some(request.id),
                                        data: format!("Send error: {}", e),
                                        is_error: true,
                                    };
                                    let _ = response_tx.send(error_response);
                                }
                                Err(_) => {
                                    log::error!("Send timeout for request: {}", request.id);
                                    
                                    let error_response = WsResponse {
                                        id: Some(request.id),
                                        data: "Send timeout".to_string(),
                                        is_error: true,
                                    };
                                    let _ = response_tx.send(error_response);
                                }
                            }
                        } else {
                            log::warn!("Writer not available");
                            
                            let error_response = WsResponse {
                                id: Some(request.id),
                                data: "Writer not available".to_string(),
                                is_error: true,
                            };
                            let _ = response_tx.send(error_response);
                        }
                    }
                    
                    Some(event) = conn_event_rx.recv() => {
                        match event {
                            ConnectionEvent::Reconnected(new_writer) => {
                                log::info!("Writer task updated with new connection");
                                let mut writer_guard = writer.lock().await;
                                *writer_guard = Some(new_writer);
                            }
                            ConnectionEvent::Disconnected => {
                                log::info!("Writer notified of disconnection");
                                connected.store(false, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
            
            log::info!("Writer task stopped");
        });
    }
    
    fn start_ping_task(&self) {
        let stop_flag = Arc::clone(&self.stop_flag);
        let connected = Arc::clone(&self.connected);
        let writer = Arc::clone(&self.writer);
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut ping_interval = time::interval(config.ping_interval);
            ping_interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            
            while !stop_flag.load(Ordering::Relaxed) {
                ping_interval.tick().await;
                
                if !connected.load(Ordering::Relaxed) {
                    log::debug!("Skipping ping - not connected");
                    continue;
                }
                
                let mut writer_guard = writer.lock().await;
                if let Some(ws_writer) = writer_guard.as_mut() {
                    let ping_result = tokio::time::timeout(
                        config.write_timeout,
                        ws_writer.send(Message::Ping(vec![]))
                    ).await;
                    
                    match ping_result {
                        Ok(Ok(_)) => {
                            log::debug!("Ping sent successfully");
                        }
                        Ok(Err(e)) => {
                            log::error!("Failed to send ping: {}", e);
                        }
                        Err(_) => {
                            log::error!("Ping send timeout");
                        }
                    }
                } else {
                    log::debug!("Skipping ping - writer not available");
                }
            }
            
            log::info!("Ping task stopped");
        });
    }
}

// Usage example
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_websocket_manager() {
        env_logger::init();
        
        let config = WsConfig {
            url: "wss://echo.websocket.org".to_string(),
            ..Default::default()
        };
        
        let (manager, mut response_rx) = WebSocketManager::new(config);
        
        // Start the manager
        manager.start().await.unwrap();
        
        // Send a test request
        let request = WsRequest {
            id: "test-1".to_string(),
            payload: "Hello WebSocket!".to_string(),
        };
        
        manager.send_request(request).unwrap();
        
        // Wait for response
        if let Some(response) = response_rx.recv().await {
            println!("Received response: {:?}", response);
        }
        
        // Give it some time to ping
        tokio::time::sleep(Duration::from_secs(5)).await;
        
        // Stop the manager
        manager.stop().await;
        
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}