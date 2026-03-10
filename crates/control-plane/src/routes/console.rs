use axum::response::{Html, IntoResponse};

pub(crate) async fn supervision_console() -> impl IntoResponse {
    Html(include_str!("supervision_console.html"))
}
