use crate::types::SocialResult;
use async_trait::async_trait;

#[async_trait]
pub trait TransportPort: Send + Sync {
    async fn send_friend_request(
        &self,
        remote_node_id: &str,
        payload: &serde_json::Value,
    ) -> SocialResult<()>;

    async fn send_friend_decision(
        &self,
        remote_node_id: &str,
        payload: &serde_json::Value,
    ) -> SocialResult<()>;

    async fn send_direct_message(
        &self,
        remote_node_id: &str,
        payload: &serde_json::Value,
    ) -> SocialResult<()>;
}
