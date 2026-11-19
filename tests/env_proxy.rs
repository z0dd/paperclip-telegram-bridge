//! Proxy variables in the environment must not affect the HTTP clients
//! (architecture §5.3 п. 3). A separate test binary: it changes process env.

mod support;

use std::time::Duration;

use bridge::paperclip::{HttpPaperclipClient, PaperclipApi};
use secrecy::SecretString;
use serde_json::json;
use support::proxy::CountingListener;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn env_proxy_variables_are_ignored() {
    let trap = CountingListener::start_blocking();
    let trap_url = format!("http://{}", trap.addr());
    // SAFETY: set before the runtime and any other thread of this binary start.
    unsafe {
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(var, &trap_url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let paperclip = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
            .expect(1)
            .mount(&paperclip)
            .await;
        let client = HttpPaperclipClient::new(
            &paperclip.uri().parse().unwrap(),
            SecretString::from("pcp_board_placeholder"),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        client.health().await.unwrap();

        support::env_proxy_telegram_checks(&trap).await;
    });
    assert_eq!(trap.connections(), 0, "trap proxy must not be used");
}
