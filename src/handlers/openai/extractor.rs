//! Custom JSON extractor with OpenAI-compatible error responses
//!
//! Wraps Axum's `Json` extractor to produce OpenAI-formatted error responses
//! when deserialization fails. This ensures compatibility with OpenAI SDKs
//! like LangChain and the official OpenAI Python/JS libraries.

use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

/// OpenAI-compatible error response structure
///
/// OpenAI SDKs expect errors in this format:
/// ```json
/// {
///   "error": {
///     "message": "...",
///     "type": "invalid_request_error",
///     "param": null,
///     "code": null
///   }
/// }
/// ```
#[derive(serde::Serialize)]
pub struct OpenAiError {
    pub error: OpenAiErrorBody,
}

#[derive(serde::Serialize)]
pub struct OpenAiErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub param: Option<String>,
    pub code: Option<String>,
}

impl OpenAiError {
    /// Create a new OpenAI-formatted error for invalid requests
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            error: OpenAiErrorBody {
                message: message.into(),
                error_type: "invalid_request_error".to_string(),
                param: None,
                code: None,
            },
        }
    }

    /// Create a new OpenAI-formatted error for server errors
    pub fn server_error(message: impl Into<String>) -> Self {
        Self {
            error: OpenAiErrorBody {
                message: message.into(),
                error_type: "server_error".to_string(),
                param: None,
                code: None,
            },
        }
    }

    /// Create an error with a specific parameter that failed validation
    pub fn invalid_param(message: impl Into<String>, param: impl Into<String>) -> Self {
        Self {
            error: OpenAiErrorBody {
                message: message.into(),
                error_type: "invalid_request_error".to_string(),
                param: Some(param.into()),
                code: None,
            },
        }
    }
}

/// OpenAI-compatible JSON extraction error
///
/// Wraps Axum's `JsonRejection` to produce OpenAI-formatted error responses.
/// Uses appropriate HTTP status codes based on the type of rejection:
/// - JSON syntax errors → 400 Bad Request
/// - Data validation errors → 422 Unprocessable Entity
/// - Missing content type → 415 Unsupported Media Type
pub struct OpenAiJsonRejection(JsonRejection);

impl IntoResponse for OpenAiJsonRejection {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            JsonRejection::JsonSyntaxError(_) => (StatusCode::BAD_REQUEST, self.0.body_text()),
            JsonRejection::JsonDataError(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, self.0.body_text())
            }
            JsonRejection::MissingJsonContentType(_) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type must be application/json".to_string(),
            ),
            _ => {
                // BytesRejection or future rejection types
                (StatusCode::BAD_REQUEST, self.0.body_text())
            }
        };
        let error = OpenAiError::invalid_request(message);
        (status, Json(error)).into_response()
    }
}

/// Custom JSON extractor that produces OpenAI-compatible error responses
///
/// Use this instead of `axum::Json` in OpenAI-compatible handlers to ensure
/// validation errors are returned in the format expected by OpenAI SDKs.
///
/// # Example
///
/// ```ignore
/// use crate::handlers::openai::extractor::OpenAiJson;
///
/// pub async fn handler(
///     OpenAiJson(request): OpenAiJson<ChatCompletionRequest>,
/// ) -> Result<Response, AppError> {
///     // If request deserialization fails, an OpenAI-formatted error is returned
///     // automatically
/// }
/// ```
pub struct OpenAiJson<T>(pub T);

impl<S, T> FromRequest<S> for OpenAiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = OpenAiJsonRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(OpenAiJson(value)),
            Err(rejection) => Err(OpenAiJsonRejection(rejection)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_error_invalid_request() {
        let error = OpenAiError::invalid_request("temperature must be between 0.0 and 2.0");
        let json = serde_json::to_value(&error).unwrap();

        assert!(json.get("error").is_some());
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(
            json["error"]["message"],
            "temperature must be between 0.0 and 2.0"
        );
        assert!(json["error"]["param"].is_null());
        assert!(json["error"]["code"].is_null());
    }

    #[test]
    fn test_openai_error_server_error() {
        let error = OpenAiError::server_error("internal server error");
        let json = serde_json::to_value(&error).unwrap();

        assert_eq!(json["error"]["type"], "server_error");
    }

    #[test]
    fn test_openai_error_invalid_param() {
        let error = OpenAiError::invalid_param("invalid value", "temperature");
        let json = serde_json::to_value(&error).unwrap();

        assert_eq!(json["error"]["param"], "temperature");
    }
}
