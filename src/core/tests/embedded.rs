//! Real receiver integration; run in Docker with the development relay.
use ahrara_core::{EmailFilter, Inbox, InboxOptions, WaitOptions};
use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

fn options() -> InboxOptions {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();

    InboxOptions {
        server_url: std::env::var("AHRARA_TEST_SERVER_URL")
            .unwrap_or_else(|_| "wss://localhost:8443".into()),
        server_public_key: Some(
            std::env::var("AHRARA_TEST_SERVER_PUBLIC_KEY").unwrap_or_else(|_| {
                std::fs::read_to_string(root.join(".dev/certs/transport-public.txt"))
                    .expect("Run make dev-certs first")
                    .trim()
                    .to_owned()
            }),
        ),
        tls_ca_path: Some(root.join(".dev/certs/ca.pem")),
        ..Default::default()
    }
}

async fn deliver(address: &str, subject: &str, text: &str) {
    let raw = format!(
        "From: sender@example.test\r\nTo: {address}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{text}\r\n"
    );

    deliver_raw(address, &raw).await;
}

async fn deliver_raw(address: &str, raw: &str) {
    let address = address.to_owned();
    let raw = raw.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut eml = tempfile::NamedTempFile::new().unwrap();
        eml.write_all(raw.as_bytes()).unwrap();
        eml.flush().unwrap();
        let smtp =
            std::env::var("AHRARA_TEST_SMTP_ADDRESS").unwrap_or_else(|_| "localhost:2525".into());
        let result = std::process::Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "20",
                "--url",
                &format!("smtp://{smtp}"),
                "--ssl-reqd",
                "--cacert",
            ])
            .arg(options().tls_ca_path.unwrap())
            .args([
                "--mail-from",
                "sender@example.test",
                "--mail-rcpt",
                &address,
                "--upload-file",
            ])
            .arg(eml.path())
            .output()
            .unwrap();

        assert!(result.status.success(), "Synthetic SMTP delivery failed");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn receive_without_cli_and_preserve_unmatched_messages() {
    let first = Inbox::create(options()).await.unwrap();
    let second = Inbox::create(options()).await.unwrap();

    assert!(
        first.address() != second.address(),
        "Inboxes must have distinct recipients"
    );
    deliver(first.address(), "first", "buffered before wait").await;
    let body = format!(
        "Código de confirmação: 123456\r\n{}",
        format!("{}\r\n", "x".repeat(80)).repeat(65)
    );
    deliver(first.address(), "second", &body).await;
    let email = first
        .wait_for_email(WaitOptions {
            subject: Some("second".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert!(email.text_body.contains(&body));
    assert!(!email.text_body_truncated);
    let email = first
        .wait_for_email(WaitOptions::default())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(email.metadata.subject, "first");

    for inbox in [&first, &second] {
        assert!(
            inbox
                .wait_for_email(WaitOptions {
                    timeout_ms: Some(50),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_none()
        );
    }

    first.close().await;
    second.close().await;

    assert!(first.wait_for_email(WaitOptions::default()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn receipt_during_wait_and_close_interrupts_waiters() {
    let inbox = Arc::new(Inbox::create(options()).await.unwrap());
    let pending = tokio::spawn({
        let inbox = inbox.clone();
        async move { inbox.wait_for_email(WaitOptions::default()).await }
    });
    deliver(inbox.address(), "during wait", "received").await;

    assert_eq!(
        pending.await.unwrap().unwrap().unwrap().metadata.subject,
        "during wait"
    );
    let waiting = tokio::spawn({
        let inbox = inbox.clone();
        async move { inbox.wait_for_email(WaitOptions::default()).await }
    });
    inbox.close().await;

    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );

    inbox.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn short_wait_skips_consumed_large_bodies_and_preserves_unmatched_mail() {
    let inbox = Inbox::create(options()).await.unwrap();
    deliver(
        inbox.address(),
        "retained",
        "unmatched mail must remain available",
    )
    .await;
    let body = format!("{}\r\n", "x".repeat(98)).repeat(5120);

    for index in 0..128 {
        let subject = format!("stream {index:04} end");
        deliver(inbox.address(), &subject, &body).await;
        let email = inbox
            .wait_for_email(WaitOptions {
                subject: Some(subject.clone()),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(email.metadata.subject, subject);
    }

    deliver(inbox.address(), "stream tail", "already buffered").await;
    let email = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(100),
            subject: Some("stream".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .expect("consumed bodies must not exhaust the SDK polling interval");

    assert_eq!(email.metadata.subject, "stream tail");
    let retained = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(100),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(retained.metadata.subject, "retained");
    inbox.close().await;
}

#[tokio::test]
async fn configuration_rejects_insecure_transport() {
    let result = Inbox::create(InboxOptions {
        server_url: "ws://localhost:8443".into(),
        server_public_key: Some(format!("ahrara_server_v1_{}", "A".repeat(43))),
        ..Default::default()
    })
    .await;

    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("server_url must use wss")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn persistent_history_identity_import_and_attachment_crud() {
    use ahrara_core::{auth, storage::Filters};
    let directory = tempfile::tempdir().unwrap();
    let configured = InboxOptions {
        profile_path: Some(directory.path().join("profile")),
        ..options()
    };
    let inbox = Inbox::create(configured.clone()).await.unwrap();
    let address = inbox.address().to_owned();
    let credentials = inbox.export_credentials().unwrap();

    assert!(!format!("{credentials:?}").contains(&credentials.recovery_key));
    let raw = format!(
        "From: sender@example.test\r\nTo: {address}\r\nSubject: multipart history\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=parts\r\n\r\n--parts\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<p>Keep history</p><script>alert(1)</script><img src=\"https://remote.example.test/image\">\r\n--parts\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"../../payload.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAAECA/8=\r\n--parts--\r\n"
    );
    deliver_raw(&address, &raw).await;
    let email = inbox
        .wait_for_email(WaitOptions::default())
        .await
        .unwrap()
        .unwrap();
    let id = email.metadata.id;

    assert_eq!(email.attachments.len(), 1);
    let attachment = &email.attachments[0];

    assert_eq!(attachment.filename, "payload.bin");
    assert_eq!(
        inbox
            .attachment(id, attachment.attachment_id.clone())
            .await
            .unwrap()
            .1,
        [0, 1, 2, 3, 255]
    );
    let saved = inbox
        .save_attachment(id, attachment.attachment_id.clone(), directory.path())
        .await
        .unwrap();

    assert_eq!(std::fs::read(&saved).unwrap(), [0, 1, 2, 3, 255]);
    assert!(
        inbox
            .save_attachment(id, attachment.attachment_id.clone(), &saved)
            .await
            .is_err()
    );
    let html = inbox.html_email(id, false).await.unwrap();

    assert!(!html.html.contains("<script"));
    assert!(!html.html.contains(" src=\"https://remote.example.test"));
    assert_eq!(inbox.raw_email(id).await.unwrap(), raw.as_bytes());
    assert_eq!(
        inbox
            .list_emails(Filters::default())
            .await
            .unwrap()
            .emails
            .len(),
        1
    );
    inbox.close().await;
    drop(inbox);

    let reopened = Inbox::create(configured.clone()).await.unwrap();

    assert_eq!(reopened.address(), address);
    assert_eq!(
        reopened.get_email(id).await.unwrap().metadata.subject,
        "multipart history"
    );
    assert!(
        reopened
            .wait_for_email(WaitOptions {
                timeout_ms: Some(30),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_none()
    );
    reopened.close().await;
    drop(reopened);

    // Import identity/address into an independent profile without copying history.
    let imported = Inbox::create(InboxOptions {
        credentials: Some(credentials.clone()),
        ..options()
    })
    .await
    .unwrap();

    assert_eq!(imported.address(), address);
    assert!(
        imported
            .list_emails(Filters::default())
            .await
            .unwrap()
            .emails
            .is_empty()
    );
    imported.close().await;

    let restored = Inbox::create(InboxOptions {
        recovery_key: Some(credentials.recovery_key.clone()),
        address: Some(address.clone()),
        ..configured.clone()
    })
    .await
    .unwrap();

    assert_eq!(
        restored
            .list_emails(Filters::default())
            .await
            .unwrap()
            .emails
            .len(),
        1
    );
    restored.close().await;
    drop(restored);

    // Another alias in the same profile cannot read or clear the original alias.
    let (route, _, _) = auth::decode_address(&address).unwrap();
    let second = Inbox::create(InboxOptions {
        address: Some(auth::address(&route, &[42; 8])),
        ..configured.clone()
    })
    .await
    .unwrap();

    assert!(second.get_email(id).await.is_err());
    assert_eq!(second.delete_email(id).await.unwrap(), 0);
    assert_eq!(second.clear_emails().await.unwrap(), 0);
    second.close().await;
    drop(second);
    let original = Inbox::create(InboxOptions {
        address: Some(address),
        ..configured
    })
    .await
    .unwrap();

    assert_eq!(original.clear_emails().await.unwrap(), 1);
    assert!(original.get_email(id).await.is_err());
    original.close().await;

    let unrelated = Inbox::create(options()).await.unwrap();
    let mismatched = ahrara_core::InboxCredentials {
        address: unrelated.address().to_owned(),
        recovery_key: credentials.recovery_key.clone(),
    };

    assert!(
        Inbox::create(InboxOptions {
            credentials: Some(mismatched),
            ..options()
        })
        .await
        .is_err()
    );

    unrelated.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn custom_domain_profile_reuses_canonical_sdk_alias_and_credentials() {
    use ahrara_core::{config::Config, core::Core, storage::Filters};

    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile");
    let (core, _) = Core::open(Config {
        database_path: profile.join("inbox.db"),
        identity_path: profile.join("identity.key"),
        ..Config::default()
    })
    .unwrap();
    let forwarding = core.create_address(None).await.unwrap();
    core.storage
        .set_custom_domain(
            core.identity.fingerprint(),
            "example.test".into(),
            forwarding.id.to_string(),
        )
        .await
        .unwrap();
    let alias = core.create_address(None).await.unwrap();

    assert!(alias.address.ends_with("@example.test"));
    let canonical = alias.routing_address;
    drop(core);

    let configured = InboxOptions {
        profile_path: Some(profile),
        ..options()
    };
    let inbox = Inbox::create(configured.clone()).await.unwrap();

    assert!(
        inbox.address() == canonical,
        "Embedded inbox must select the canonical relay alias"
    );
    deliver(inbox.address(), "canonical profile alias", "stored").await;
    let email = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    let id = email.metadata.id;

    assert_eq!(
        inbox
            .list_emails(Filters::default())
            .await
            .unwrap()
            .emails
            .len(),
        1
    );
    let credentials = inbox.export_credentials().unwrap();
    inbox.close().await;
    drop(inbox);

    let restored = Inbox::create(InboxOptions {
        credentials: Some(credentials),
        ..configured
    })
    .await
    .unwrap();

    assert!(
        restored.address() == canonical,
        "Restored credentials must retain the canonical relay alias"
    );
    assert_eq!(
        restored.get_email(id).await.unwrap().metadata.subject,
        "canonical profile alias"
    );
    assert_eq!(restored.delete_email(id).await.unwrap(), 1);
    assert_eq!(restored.clear_emails().await.unwrap(), 0);
    restored.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn indefinite_wait_stream_deadline_and_callback_errors() {
    use futures_util::StreamExt;
    use tokio_util::sync::CancellationToken;

    assert_eq!(WaitOptions::default().timeout_ms, None);
    let inbox = Inbox::create(options()).await.unwrap();

    assert!(
        tokio::time::timeout(
            Duration::from_millis(120),
            inbox.wait_for_email(WaitOptions::default())
        )
        .await
        .is_err()
    );
    deliver(inbox.address(), "first streamed", "1").await;
    deliver(inbox.address(), "second streamed", "2").await;
    let received = inbox
        .wait_for_emails(
            WaitOptions {
                timeout_ms: Some(200),
                ..Default::default()
            },
            CancellationToken::new(),
        )
        .collect::<Vec<_>>()
        .await;

    assert_eq!(received.len(), 2);
    assert!(received.iter().all(Result::is_ok));

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = inbox
        .wait_for_emails(WaitOptions::default(), cancel)
        .collect::<Vec<_>>()
        .await;

    assert_eq!(cancelled.len(), 1);
    assert!(cancelled[0].is_err());
    deliver(inbox.address(), "callback", "still connected").await;
    let failure = inbox
        .receive_emails(
            WaitOptions::default(),
            CancellationToken::new(),
            |_| async { anyhow::bail!("synthetic handler failure") },
        )
        .await;

    assert!(
        failure
            .unwrap_err()
            .to_string()
            .contains("synthetic handler failure")
    );
    deliver(inbox.address(), "after callback", "usable").await;

    assert_eq!(
        inbox
            .wait_for_email(WaitOptions::default())
            .await
            .unwrap()
            .unwrap()
            .metadata
            .subject,
        "after callback"
    );

    inbox.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn mail_message_projection_preserves_history_and_receipt_state() {
    let inbox = Inbox::create(options()).await.unwrap();
    let body = format!("{}\r\n", "complete body ".repeat(8)).repeat(3000);
    let raw = format!(
        "From: Sender <sender@example.test>\r\nTo: {}\r\nSubject: Complete message\r\nX-Trace: first\r\nX-Trace: second\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}",
        inbox.address(),
        body
    );
    deliver_raw(inbox.address(), &raw).await;
    let message = inbox
        .wait_for_mail_message(WaitOptions {
            timeout_ms: Some(2000),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(message.text_body.as_deref(), Some(body.as_str()));
    assert_eq!(
        message
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("X-Trace"))
            .count(),
        2
    );
    let id = message.metadata.id;

    assert_eq!(
        inbox.get_mail_message(id).await.unwrap().text_body,
        message.text_body
    );
    assert!(
        inbox
            .wait_for_email(WaitOptions {
                timeout_ms: Some(30),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(inbox.delete_email(id).await.unwrap(), 1);
    assert_eq!(message.text_body.as_deref(), Some(body.as_str()));

    let invalid = format!(
        "From: broken <\r\nTo: {}\r\nSubject: Invalid address\r\n\r\nStored for recovery",
        inbox.address()
    );
    deliver_raw(inbox.address(), &invalid).await;
    let failure = inbox
        .wait_for_mail_message(WaitOptions {
            timeout_ms: Some(2000),
            ..Default::default()
        })
        .await;

    assert!(matches!(failure, Err(ref error) if error.to_string().starts_with("invalid_message:")));
    // Failed projection must not commit the receive cursor or erase the MIME.
    let legacy = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(2000),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(legacy.metadata.subject, "Invalid address");
    assert!(
        !inbox
            .raw_email(legacy.metadata.id)
            .await
            .unwrap()
            .is_empty()
    );
    inbox.close().await;

    assert_eq!(message.subject, "Complete message");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn admission_filters_drop_before_storage_and_queries_preserve_nonmatches() {
    use futures_util::StreamExt;
    use std::collections::BTreeMap;
    let inbox = Inbox::create(InboxOptions {
        filter: Some(EmailFilter {
            headers: BTreeMap::from([("X-Project".into(), "wanted".into())]),
            ..Default::default()
        }),
        ..options()
    })
    .await
    .unwrap();

    for index in 0..10 {
        let raw = format!(
            "From: Sender <sender@example.test>\r\nSubject: {}\r\nX-Project: {}\r\nContent-Type: text/plain\r\n\r\n{}\r\n",
            if index == 3 { "other" } else { "selected" },
            if index == 2 { "drop" } else { "wanted" },
            match index {
                3 => "code=000",
                4 => "code=444",
                7 => "code=777 code=778",
                9 => "code=999",
                _ => "no code",
            }
        );

        deliver_raw(inbox.address(), &raw).await;
    }

    let wait = WaitOptions {
        timeout_ms: Some(3000),
        filter: Some(EmailFilter {
            subject: Some("selected".into()),
            sender: Some("SENDER".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let first = inbox
        .wait_for_regex_match("code=(?P<code>\\d+)", wait.clone())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.value, "code=444");
    assert_eq!(first.groups[1].name.as_deref(), Some("code"));
    assert_eq!(first.groups[1].value.as_deref(), Some("444"));
    let stream = inbox.wait_for_regex_matches(
        "code=\\d+",
        wait,
        tokio_util::sync::CancellationToken::new(),
    );
    futures_util::pin_mut!(stream);
    let mut matches = Vec::new();

    while let Some(found) = stream.next().await {
        matches.push(found.unwrap().value);
    }

    assert_eq!(matches, ["code=777", "code=778", "code=999"]);
    let page = inbox.list_emails(Default::default()).await.unwrap();

    assert_eq!(
        page.emails.len(),
        9,
        "Inbox rejection must not persist the dropped delivery"
    );
    let other = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(1000),
            subject: Some("other".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert!(
        other.text_body.contains("code=000"),
        "Method filter must leave mail available"
    );
    let unmatched = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert!(
        unmatched.text_body.contains("no code"),
        "Regex nonmatches must remain available"
    );

    inbox.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires development relay; run make sdk-rust-e2e"]
async fn xpath_streams_across_emails_and_invalid_queries_do_not_consume() {
    use futures_util::StreamExt;
    let inbox = Inbox::create(options()).await.unwrap();

    for (subject, html) in [
        ("unmatched", "<p>No link"),
        (
            "first",
            "<p><a href='/one?x=1&amp;y=2'>First<br>link</a><p><a href='/two'>Two",
        ),
        ("second", "<a href='/three'>Three"),
    ] {
        deliver_raw(
            inbox.address(),
            &format!("From: sender@example.test\r\nSubject: {subject}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n")
        ).await;
    }

    assert!(
        inbox
            .wait_for_xpath("//*[", WaitOptions::default())
            .await
            .is_err()
    );
    let stream = inbox.wait_for_xpath_values(
        "//a/@href",
        WaitOptions {
            timeout_ms: Some(2000),
            ..Default::default()
        },
        tokio_util::sync::CancellationToken::new(),
    );
    futures_util::pin_mut!(stream);
    let mut values = Vec::new();

    while let Some(value) = stream.next().await {
        values.push(value.unwrap());
    }

    assert_eq!(values, ["/one?x=1&y=2", "/two", "/three"]);
    let unmatched = inbox
        .wait_for_email(WaitOptions {
            timeout_ms: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(unmatched.metadata.subject, "unmatched");
    inbox.close().await;
}
