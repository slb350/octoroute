//! Provider credential cache invalidation.
//!
//! A resolved credential is reused for five minutes. When the provider answers
//! 401 or 403 the cached value has to be discarded, or a rotated key is not
//! picked up until the gateway is restarted.

use super::*;

/// An environment that hands out a stale key first and a rotated one after.
///
/// The rotation is what a credential store does when an operator rolls the key;
/// the gateway sees it only if it asks again.
#[derive(Clone)]
struct RotatingEnvironment {
    keys: Arc<Mutex<Vec<String>>>,
    reads: Arc<Mutex<Vec<String>>>,
}

impl RotatingEnvironment {
    fn new(keys: [&str; 2]) -> Self {
        Self {
            keys: Arc::new(Mutex::new(keys.map(str::to_string).to_vec())),
            reads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// How many times the provider credential was resolved from the source.
    fn resolutions(&self) -> usize {
        self.reads
            .lock()
            .expect("reads mutex")
            .iter()
            .filter(|name| *name == "ZAI_API_KEY")
            .count()
    }
}

impl Environment for RotatingEnvironment {
    fn get(&self, name: &str) -> Option<SecretString> {
        self.reads
            .lock()
            .expect("reads mutex")
            .push(name.to_string());
        match name {
            "OCTOROUTE_API_KEY" => Some(SecretString::from("inbound-test-key")),
            "ZAI_API_KEY" => {
                let mut keys = self.keys.lock().expect("keys mutex");
                let value = if keys.len() > 1 {
                    keys.remove(0)
                } else {
                    keys.first().cloned()?
                };
                Some(SecretString::from(value))
            }
            _ => None,
        }
    }
}

#[tokio::test]
async fn a_rejected_credential_is_discarded_so_the_next_request_re_resolves_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer stale-key"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"error": {"message": "credential rotated"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer rotated-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "glm-5.3", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let environment = RotatingEnvironment::new(["stale-key", "rotated-key"]);
    let audit = environment.clone();
    let service =
        FabricGatewayService::from_config(single_provider_config(&server, "zai"), environment)
            .expect("service");

    // The stale key is resolved, sent, and rejected. The route has one step, so
    // the rejection commits rather than falling forward.
    let rejected = service
        .handle_chat(&headers(), portable_cloud_request())
        .await;
    assert_eq!(rejected.status(), 502);
    let body: Value = serde_json::from_slice(&response_body(rejected).await).expect("error JSON");
    assert_eq!(body["error"]["code"], "provider_credential_rejected");
    assert_eq!(audit.resolutions(), 1);

    // The next request must ask the source again rather than replay the key the
    // provider just refused for the rest of the five-minute window.
    let accepted = service
        .handle_chat(&headers(), portable_cloud_request())
        .await;
    assert_eq!(accepted.status(), 200);
    assert_eq!(
        audit.resolutions(),
        2,
        "a credential the provider rejected must be re-resolved, not reused"
    );
}
