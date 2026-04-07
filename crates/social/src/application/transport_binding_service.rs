use crate::domain::transport_bindings::RemoteTransportBinding;
use crate::ports::repositories::TransportBindingRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_transport_binding<R>(
    repository: &R,
    binding: &RemoteTransportBinding,
) -> SocialResult<()>
where
    R: TransportBindingRepository,
{
    if binding.public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "public_id is required".to_owned(),
        ));
    }
    if binding.transport_node_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "transport_node_id is required".to_owned(),
        ));
    }
    if binding.binding_source.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "binding_source is required".to_owned(),
        ));
    }
    if binding.binding_confidence < 0 {
        return Err(SocialError::InvalidInput(
            "binding_confidence must be >= 0".to_owned(),
        ));
    }
    repository.upsert_transport_binding(binding)
}

pub fn list_transport_bindings<R>(repository: &R) -> SocialResult<Vec<RemoteTransportBinding>>
where
    R: TransportBindingRepository,
{
    repository.list_transport_bindings()
}
