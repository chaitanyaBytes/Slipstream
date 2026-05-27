use crate::cartographer::Cartographer;
use http::Uri;
use log::{error, info};
use slipstream_common::SlipstreamError;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};
use tonic::{service::Interceptor, Request, Status};
use yellowstone_grpc_proto::geyser::SubscribeRequest;
use yellowstone_grpc_proto::geyser::{
    geyser_client::GeyserClient, subscribe_update::UpdateOneof, SubscribeRequestFilterSlots,
};

pub struct GeyserListener {
    client: GeyserClient<tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>>,
    cartographer: Arc<Cartographer>,
}

#[derive(Clone)]
struct AuthInterceptor {
    token: Option<String>,
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.token {
            let val = tonic::metadata::MetadataValue::from_str(token)
                .map_err(|_| Status::invalid_argument("invalid token format"))?;
            req.metadata_mut().insert("x-token", val);
        }
        Ok(req)
    }
}

impl GeyserListener {
    pub async fn connect(
        endpoint: String,
        cartographer: Arc<Cartographer>,
    ) -> Result<Self, SlipstreamError> {
        info!("geyser: parsing endpoint...");
        let mut clean_endpoint = endpoint;
        let mut x_token = None;

        if let Ok(uri) = clean_endpoint.parse::<Uri>() {
            if let Some(path) = uri.path_and_query() {
                let path_str = path.as_str();
                if path_str.len() > 1 && path_str != "/" {
                    x_token = Some(path_str.trim_start_matches('/').to_string());

                    let scheme = uri.scheme_str().unwrap_or("https");
                    let authority = uri
                        .authority()
                        .ok_or_else(|| {
                            SlipstreamError::InvalidUri(format!(
                                "geyser url missing authority: {}",
                                clean_endpoint
                            ))
                        })?
                        .as_str();
                    clean_endpoint = format!("{}://{}", scheme, authority);
                }
            }
        }

        info!("geyser: connecting to {}", clean_endpoint);
        let channel = Endpoint::from_shared(clean_endpoint.clone())
            .map_err(|e| SlipstreamError::InvalidUri(format!("invalid endpoint: {}", e)))?
            .tls_config(tonic::transport::ClientTlsConfig::new())
            .map_err(|e| SlipstreamError::Geyser(format!("tls config failed: {}", e)))?
            .connect()
            .await
            .map_err(|e| SlipstreamError::Geyser(format!("connect failed: {}", e)))?;

        let interceptor = AuthInterceptor { token: x_token };
        let client = GeyserClient::with_interceptor(channel, interceptor);

        info!("geyser: connected");
        Ok(Self {
            client,
            cartographer,
        })
    }

    pub async fn start_tracking(&mut self) -> Result<(), SlipstreamError> {
        info!("geyser: subscribing to slot updates");

        let mut slots = std::collections::HashMap::new();
        slots.insert(
            "client".to_string(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: None,
                interslot_updates: None,
            },
        );

        let request = SubscribeRequest {
            slots,
            accounts: std::collections::HashMap::new(),
            transactions: std::collections::HashMap::new(),
            transactions_status: std::collections::HashMap::new(),
            blocks: std::collections::HashMap::new(),
            blocks_meta: std::collections::HashMap::new(),
            entry: std::collections::HashMap::new(),
            commitment: None,
            accounts_data_slice: vec![],
            ping: None,
            from_slot: None,
        };

        let (tx, rx) = mpsc::channel(32);
        tx.send(request)
            .await
            .map_err(|e| SlipstreamError::Channel(format!("failed to send request: {}", e)))?;

        let response = self
            .client
            .subscribe(ReceiverStream::new(rx))
            .await
            .map_err(|e| SlipstreamError::Geyser(format!("subscribe failed: {}", e)))?;
        let mut stream = response.into_inner();

        info!("geyser: stream active");
        while let Some(message) = stream
            .message()
            .await
            .map_err(|e| SlipstreamError::Geyser(format!("stream message failed: {}", e)))?
        {
            if let Some(UpdateOneof::Slot(slot_update)) = message.update_oneof {
                if slot_update.status == 0 {
                    self.cartographer.update_slot(slot_update.slot);
                }
            }
        }

        Ok(())
    }
}

pub fn spawn_geyser_monitor(
    endpoint: String,
    cartographer: Arc<Cartographer>,
    initial_delay: Duration,
    max_delay: Duration,
) -> oneshot::Receiver<Result<(), SlipstreamError>> {
    let (startup_tx, startup_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut retry_delay = initial_delay;
        let mut startup_tx = Some(startup_tx);

        loop {
            match GeyserListener::connect(endpoint.clone(), cartographer.clone()).await {
                Ok(mut listener) => {
                    retry_delay = initial_delay;

                    if let Some(tx) = startup_tx.take() {
                        let _ = tx.send(Ok(()));
                    }

                    if let Err(e) = listener.start_tracking().await {
                        error!(
                            "geyser stream error: {}. reconnecting in {:?}",
                            e, retry_delay
                        );
                    }
                }
                Err(e) => {
                    if let Some(tx) = startup_tx.take() {
                        let _ = tx.send(Err(SlipstreamError::Geyser(e.to_string())));
                    }
                    error!(
                        "geyser connect failed: {}. retrying in {:?}",
                        e, retry_delay
                    );
                }
            }

            tokio::time::sleep(retry_delay).await;
            retry_delay = std::cmp::min(retry_delay * 2, max_delay);
        }
    });

    startup_rx
}
