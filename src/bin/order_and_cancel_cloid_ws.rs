use std::{thread::sleep, time::Duration};

use alloy::signers::local::PrivateKeySigner;
use hyperliquid_rust_sdk::{
    BaseUrl, ClientLimit, ClientOrder, ClientOrderRequest, ExchangeClient,
};
use log::info;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    env_logger::init();
    println!("Starting order_and_cancel_cloid_ws example...");
    // Key was randomly generated for testing and shouldn't be used with any real funds
    let wallet: PrivateKeySigner =
        ""
            .parse()
            .unwrap();
    let (tx, mut rx) = unbounded_channel();

    let mut exchange_client = ExchangeClient::new_with_ws(
        None,
        wallet,
        Some(BaseUrl::Mainnet),
        None,
        None,
        true,
        Some(tx.clone()),
    )
    .await
    .unwrap();

    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            println!("Received WS message: {:?}", message);
        }
    });

    // Order and Cancel with cloid
    let cloid = Uuid::new_v4();
    let order = ClientOrderRequest {
        asset: "ETH".to_string(),
        is_buy: true,
        reduce_only: false,
        limit_px: 1800.0,
        sz: 0.01,
        cloid: Some(cloid),
        order_type: ClientOrder::Limit(ClientLimit {
            tif: "Gtc".to_string(),
        }),
    };

    let response = exchange_client.ws_order(order, None).await.unwrap();
    info!("Order placed: {response:?}");

    // So you can see the order before it's cancelled
    sleep(Duration::from_secs(5));

    // let cancel = ClientCancelRequestCloid {
    //     asset: "ETH".to_string(),
    //     cloid,
    // };

    // // This response will return an error if order was filled (since you can't cancel a filled order), otherwise it will cancel the order
    // let response = exchange_client.cancel_by_cloid(cancel, None).await.unwrap();
    // info!("Order potentially cancelled: {response:?}");
}
