//! The questions panel's two routes: every box's questions, and the owner's answer to one
//! (`skein::asks`, SKEIN-1061).

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub(super) async fn api_asks() -> Json<Vec<skein::asks::Ask>> {
    // Blocking: it execs into the sandbox to read the queue.
    Json(
        tokio::task::spawn_blocking(skein::asks::fleet_asks)
            .await
            .unwrap_or_default(),
    )
}

#[derive(serde::Deserialize)]
pub(super) struct AnswerReq {
    /// **What the page showed**, echoed back — `api_git_grant_decide`'s reason: the file is the
    /// box's and can change between the render and the press.
    #[serde(rename = "box", default)]
    box_name: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<String>,
    /// The label pressed, or the line typed. Absent with `dismiss`.
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    dismiss: bool,
}

/// Answer or dismiss one question, once.
pub(super) async fn api_ask_answer(Path(id): Path<String>, Json(r): Json<AnswerReq>) -> Response {
    let reply = match (r.dismiss, r.answer) {
        (true, None) => skein::asks::Reply::Dismiss,
        (false, Some(a)) => skein::asks::Reply::Answer(a),
        _ => return (StatusCode::BAD_REQUEST, "an answer or a dismissal").into_response(),
    };
    let rendered = skein::asks::Ask {
        id,
        box_name: r.box_name,
        question: r.question,
        options: r.options,
        ..Default::default()
    };
    match tokio::task::spawn_blocking(move || skein::asks::answer(&rendered, &reply)).await {
        Ok(Ok(done)) => Json(done).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
