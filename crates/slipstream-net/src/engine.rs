use dashmap::DashMap;
use log::{debug, info};
use quinn::{Connection, Endpoint};
use slipstream_common::{create_quic_config, SlipstreamError};
use solana_sdk::signature::Keypair;
use std::net::SocketAddr;
use std::sync::Arc;

pub struct QuicEngine {
    endpoint: Endpoint,
    connection_cache: Arc<DashMap<SocketAddr, Connection>>,
}

impl QuicEngine {
    pub fn new(identity: &Keypair) -> Result<Self, SlipstreamError> {
        let client_config = create_quic_config(identity)?;
        let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| SlipstreamError::Connection(format!("endpoint bind failed: {e}")))?;
        endpoint.set_default_client_config(client_config);

        Ok(Self {
            endpoint,
            connection_cache: Arc::new(DashMap::new()),
        })
    }

    pub async fn send_transaction(
        &self,
        target: SocketAddr,
        tx_bytes: Vec<u8>,
    ) -> Result<(), SlipstreamError> {
        let connection = self.get_connection(target).await?;
        let mut stream = connection
            .open_uni()
            .await
            .map_err(|e| SlipstreamError::Connection(format!("open stream failed: {e}")))?;

        stream.write_all(&tx_bytes).await?;
        stream.finish()?;
        Ok(())
    }

    pub async fn get_connection_handle(
        &self,
        target: SocketAddr,
    ) -> Result<Connection, SlipstreamError> {
        self.get_connection(target).await
    }

    async fn get_connection(&self, addr: SocketAddr) -> Result<Connection, SlipstreamError> {
        if let Some(conn) = self.connection_cache.get(&addr) {
            if conn.close_reason().is_none() {
                return Ok(conn.clone());
            }
        }

        self.connection_cache.remove(&addr);
        info!("handshake to leader {}", addr);

        let connecting = self
            .endpoint
            .connect(addr, "solana")
            .map_err(|e| SlipstreamError::Connection(format!("connect failed: {e}")))?;
        let connection = connecting.await?;

        self.connection_cache.insert(addr, connection.clone());
        debug!("connection cached for {}", addr);

        Ok(connection)
    }
}
