//! Signing in from the browser without gh: GitHub's device flow. Bombadil
//! shows a one-time code, the user approves it at github.com/login/device,
//! and the token that comes back is kept in the keychain like a pasted one.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Bombadil's OAuth App on GitHub. Public by design: the device flow has no
/// secret.
pub const CLIENT_ID: &str = "Ov23liauT8q3T4x1S4ui";

/// One form POST answered with JSON. Behind a trait so no test reaches the
/// network.
pub trait Post: Send + Sync {
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<String, String>;
}

impl Post for super::rest::UreqFetch {
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<String, String> {
        self.agent()
            .post(url)
            .header("Accept", "application/json")
            .header("User-Agent", "bombadil")
            .send_form(form.iter().copied())
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())
    }
}

/// Shows the code through `on_code`, then polls until the user approves.
/// `wait` sleeps between polls and returns false to cancel. `repo` is the
/// scope private repositories need; Bombadil only ever reads with it.
pub fn login(
    post: &dyn Post,
    client_id: &str,
    on_code: &mut dyn FnMut(String),
    wait: &mut dyn FnMut(Duration) -> bool,
) -> Result<String, String> {
    let start = answer(post.post(
        "https://github.com/login/device/code",
        &[("client_id", client_id), ("scope", "repo")],
    )?)?;
    let (Some(device_code), Some(user_code)) =
        (start["device_code"].as_str(), start["user_code"].as_str())
    else {
        return Err("GitHub sent an unexpected answer".into());
    };
    let mut interval = start["interval"].as_u64().unwrap_or(5);
    on_code(user_code.to_string());
    loop {
        if !wait(Duration::from_secs(interval)) {
            return Err("cancelled".into());
        }
        let poll = answer(post.post(
            "https://github.com/login/oauth/access_token",
            &[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?);
        match poll {
            Ok(body) => match body["access_token"].as_str() {
                Some(token) => return Ok(token.to_string()),
                None => return Err("GitHub sent an unexpected answer".into()),
            },
            Err(e) if e == "GitHub refused: authorization_pending" => {}
            Err(e) if e == "GitHub refused: slow_down" => interval += 5,
            Err(e) => return Err(e),
        }
    }
}

/// The JSON body, or its `error` as a reason. Never echoes the body: a
/// success carries the token.
fn answer(body: String) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(&body).map_err(|_| "GitHub sent an unexpected answer".to_string())?;
    match value["error"].as_str() {
        None => Ok(value),
        Some("access_denied") => Err("the sign-in was declined".into()),
        Some("expired_token") => Err("the sign-in code expired".into()),
        // An unknown client ID, or one without device flow enabled.
        Some("Not Found") => Err("GitHub does not know Bombadil's sign-in app".into()),
        Some(other) => Err(format!("GitHub refused: {other}")),
    }
}

/// Sleeps `total`, waking early when `cancel` is set. False when cancelled.
pub fn sleep_unless(cancel: &AtomicBool, total: Duration) -> bool {
    let step = Duration::from_millis(200);
    let deadline = std::time::Instant::now() + total;
    while !cancel.load(Ordering::SeqCst) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(step));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Asked = (String, Vec<(String, String)>);

    /// Answers each POST from a script, in order, and records the forms.
    struct Script {
        answers: Mutex<Vec<Result<String, String>>>,
        asked: Mutex<Vec<Asked>>,
    }
    impl Script {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: Mutex::new(answers.iter().rev().map(|a| Ok(a.to_string())).collect()),
                asked: Mutex::default(),
            }
        }
    }
    impl Post for Script {
        fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<String, String> {
            let form = form.iter().map(|(k, v)| (k.to_string(), v.to_string()));
            self.asked
                .lock()
                .unwrap()
                .push((url.into(), form.collect()));
            self.answers
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Err("no more".into()))
        }
    }

    const CODE: &str = r#"{"device_code":"dev","user_code":"ABCD-1234",
        "verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
    const PENDING: &str = r#"{"error":"authorization_pending"}"#;

    fn run(
        post: &Script,
        wait: &mut dyn FnMut(Duration) -> bool,
    ) -> (Result<String, String>, Vec<String>) {
        let mut codes = Vec::new();
        let result = login(post, "cid", &mut |c| codes.push(c), wait);
        (result, codes)
    }

    #[test]
    fn the_code_is_shown_and_the_token_returned_once_approved() {
        let post = Script::new(&[
            CODE,
            PENDING,
            r#"{"access_token":"gho_x","token_type":"bearer"}"#,
        ]);
        let mut waits = Vec::new();
        let (result, codes) = run(&post, &mut |d| {
            waits.push(d);
            true
        });
        assert_eq!(result, Ok("gho_x".into()));
        assert_eq!(codes, ["ABCD-1234"]);
        assert_eq!(waits, [Duration::from_secs(5); 2]);
        let asked = post.asked.lock().unwrap();
        assert_eq!(asked[0].0, "https://github.com/login/device/code");
        assert!(asked[0].1.contains(&("client_id".into(), "cid".into())));
        assert!(asked[0].1.contains(&("scope".into(), "repo".into())));
        assert_eq!(asked[1].0, "https://github.com/login/oauth/access_token");
        assert!(asked[1].1.contains(&("device_code".into(), "dev".into())));
        assert!(asked[1].1.contains(&(
            "grant_type".into(),
            "urn:ietf:params:oauth:grant-type:device_code".into()
        )));
    }

    #[test]
    fn slow_down_waits_longer_from_then_on() {
        let post = Script::new(&[
            CODE,
            r#"{"error":"slow_down","interval":10}"#,
            PENDING,
            r#"{"access_token":"t"}"#,
        ]);
        let mut waits = Vec::new();
        let (result, _) = run(&post, &mut |d| {
            waits.push(d.as_secs());
            true
        });
        assert_eq!(result, Ok("t".into()));
        assert_eq!(waits, [5, 10, 10]);
    }

    #[test]
    fn refusals_and_expiry_end_with_a_reason() {
        for (answer, want) in [
            (r#"{"error":"access_denied"}"#, "the sign-in was declined"),
            (r#"{"error":"expired_token"}"#, "the sign-in code expired"),
            (
                r#"{"error":"device_flow_disabled"}"#,
                "GitHub refused: device_flow_disabled",
            ),
        ] {
            let post = Script::new(&[CODE, answer]);
            assert_eq!(run(&post, &mut |_| true).0, Err(want.into()), "{answer}");
        }
    }

    #[test]
    fn cancelling_stops_before_the_next_poll() {
        let post = Script::new(&[CODE, PENDING]);
        assert_eq!(run(&post, &mut |_| false).0, Err("cancelled".into()));
        assert_eq!(post.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_bad_first_answer_never_shows_a_code() {
        let post = Script::new(&[r#"{"error":"unauthorized_client"}"#]);
        let (result, codes) = run(&post, &mut |_| true);
        assert_eq!(result, Err("GitHub refused: unauthorized_client".into()));
        assert!(codes.is_empty());
        let post = Script::new(&[r#"{"error":"Not Found"}"#]);
        assert_eq!(
            run(&post, &mut |_| true).0,
            Err("GitHub does not know Bombadil's sign-in app".into())
        );
        let post = Script::new(&["<html>"]);
        assert_eq!(
            run(&post, &mut |_| true).0,
            Err("GitHub sent an unexpected answer".into())
        );
    }

    #[test]
    fn network_failures_pass_through() {
        let post = Script::new(&[CODE]);
        assert_eq!(run(&post, &mut |_| true).0, Err("no more".into()));
    }

    #[test]
    fn a_cancelled_sleep_returns_early() {
        let cancel = AtomicBool::new(true);
        let start = std::time::Instant::now();
        assert!(!sleep_unless(&cancel, Duration::from_secs(5)));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(sleep_unless(
            &AtomicBool::new(false),
            Duration::from_millis(10)
        ));
    }
}
