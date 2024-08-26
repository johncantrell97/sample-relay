use argh::FromArgs;
use bitcoin::Network;
use hex::prelude::*;
use lightning_relay::lightning::sign::KeysManager;
use lightning_relay::lightning::util::logger::{Logger, Record};
use lightning_relay::peer_manager::relay::{PeerManagerServer, RelayServer, RemotePeerManager};
use lightning_relay::relay_manager::RelayId;
use lightning_relay::relay_manager::RelayManager;
use lightning_relay::tonic::transport::Server;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub struct TracingLogger {}

impl Logger for TracingLogger {
    fn log(&self, record: Record) {
        tracing::info!("{:?}", record);
    }
}

#[derive(FromArgs)]
/// Lightning Relay Sample Relay
struct Args {
    #[argh(option)]
    /// the port the relay listens for incoming peer connections on.
    ldk_peer_listening_port: u16,
    #[argh(option)]
    /// the port the relay listens for requests from the node on.
    relay_service_port: u16,
    #[argh(option)]
    /// address the node service is listening on.
    node_service_address: String,
    #[argh(option)]
    /// node seed bytes as hex string.
    seed_hex: String,
    #[argh(option)]
    /// bitcoin network relay is running on.
    network: Network
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Args = argh::from_env();
    let logger = Arc::new(TracingLogger {});
    let cur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    
    let seed_bytes = <[u8; 64]>::from_hex(&args.seed_hex).expect("valid 64 byte hex seed");
    // TODO: does this network really matter for this usage?
    let xprv = bitcoin::bip32::ExtendedPrivKey::new_master(args.network, &seed_bytes).unwrap();
    let keys_seed: [u8; 32] = xprv.private_key.secret_bytes();
    let keys_manager = Arc::new(KeysManager::new(
        &keys_seed,
        cur.as_secs(),
        cur.subsec_nanos(),
    ));

    let relay_id = RelayId::random();
    let peer_manager = RemotePeerManager::new(
        relay_id.clone(),
        args.node_service_address.clone(),
        logger,
        keys_manager,
    )
    .await;
    let relay_peer_manager = Arc::new(peer_manager.clone());
    let relay_manager = RelayManager::new(
        relay_id.clone(),
        args.node_service_address,
        relay_peer_manager,
    )
    .await;

    let (stop_listen_connect, listener_handle) =
        relay_manager.handle_incoming_connections(args.ldk_peer_listening_port);

    let (stop_timer_tick, timer_tick_handle) = relay_manager.handle_timer_ticks();

    let relay_manager_server = relay_manager.clone();
    tokio::task::spawn(async move {
        if let Err(e) = Server::builder()
        .add_service(RelayServer::new(relay_manager_server))
        .add_service(PeerManagerServer::new(peer_manager))
        .serve(
            format!("127.0.0.1:{}", args.relay_service_port)
                .parse()
                .unwrap(),
        )
        .await
    {
        tracing::error!("failed to start relay server: {:?}", e);
    }
    });

    // TODO: get this from introspection / local metadata service?
    let relay_ip_address = "http://127.0.0.1".to_string();
    let relay_connection_string = format!("{}:{}", relay_ip_address, args.relay_service_port);
    
    // TODO: there's not a good way that I can find to know when our service starts listening...
    // tokio::time::sleep(Duration::from_secs(5)).await;

    relay_manager
        .signal_relay_started(relay_connection_string)
        .await;
    
    // stop_listen_connect.store(true, Ordering::Relaxed);

    if let Err(e) = listener_handle.await {
        tracing::error!("failed to await peer connection listener: {:?}", e);
    }

    stop_timer_tick.store(true, Ordering::Relaxed);
    if let Err(e) = timer_tick_handle.await {
        tracing::error!("failed to await timer tick handle: {:?}", e);
    }

    relay_manager.signal_relay_stopped().await;

    Ok(())
}
