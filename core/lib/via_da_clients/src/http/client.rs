use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde_json::json;
use zksync_da_client::{
    types::{DAError, DispatchResponse, InclusionData},
    DataAvailabilityClient,
};

#[derive(Debug, Clone)]
pub struct HttpDaClient {
    base_url: String,
    client: Arc<Client>,
}

impl HttpDaClient {
    pub fn new(base_url: String) -> Self {
        let client = Client::new();
        Self {
            base_url,
            client: Arc::new(client),
        }
    }
}

#[async_trait]
impl DataAvailabilityClient for HttpDaClient {
    async fn dispatch_blob(
        &self,
        batch_number: u32,
        data: Vec<u8>,
    ) -> Result<DispatchResponse, DAError> {
        let url = format!("{}/da/dispatch", self.base_url);
        let body = json!({
            "batch_number": batch_number,
            "data": base64::engine::general_purpose::STANDARD.encode(data),
        });

        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| DAError {
                error: e.into(),
                is_retriable: true,
            })?;

        if !res.status().is_success() {
            return Err(DAError {
                error: anyhow::anyhow!("dispatch_blob failed: {}", res.status()),
                is_retriable: false,
            });
        }

        res.json::<DispatchResponse>().await.map_err(|e| DAError {
            error: e.into(),
            is_retriable: false,
        })
    }

    async fn get_inclusion_data(&self, blob_id: &str) -> Result<Option<InclusionData>, DAError> {
        let url = format!("{}/da/inclusion/{}", self.base_url, blob_id);

        let res = self.client.get(&url).send().await.map_err(|e| DAError {
            error: e.into(),
            is_retriable: true,
        })?;

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !res.status().is_success() {
            return Err(DAError {
                error: anyhow::anyhow!("get_inclusion_data failed: {}", res.status()),
                is_retriable: false,
            });
        }

        res.json::<InclusionData>()
            .await
            .map(Some)
            .map_err(|e| DAError {
                error: e.into(),
                is_retriable: false,
            })
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        None // the HTTP backend doesn’t enforce size, server does
    }
}
