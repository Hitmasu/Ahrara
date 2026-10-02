//! Small versioned C ABI; all networking and delivery live in ahrara-core.
use ahrara_core::storage::Filters;
use ahrara_core::{EmailFilter, Inbox, InboxOptions, WaitOptions, query::Query};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static INBOXES: OnceLock<Mutex<HashMap<u64, Arc<Inbox>>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum MessageFormat {
    #[default]
    Parsed,
    MailMessage,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Create {
        #[serde(default)]
        options: InboxOptions,
    },
    Wait {
        handle: u64,
        timeout_ms: Option<u64>,
        subject: Option<String>,
        from: Option<String>,
        filter: Option<EmailFilter>,
        #[serde(default)]
        format: MessageFormat,
    },
    Search {
        handle: u64,
        timeout_ms: Option<u64>,
        subject: Option<String>,
        from: Option<String>,
        filter: Option<EmailFilter>,
        query: Query,
    },
    Close {
        handle: u64,
    },
    ExportCredentials {
        handle: u64,
    },
    List {
        handle: u64,
        after_id: Option<i64>,
        limit: Option<u32>,
        subject: Option<String>,
        from: Option<String>,
    },
    Get {
        handle: u64,
        id: i64,
        #[serde(default)]
        format: MessageFormat,
    },
    Delete {
        handle: u64,
        id: i64,
    },
    Clear {
        handle: u64,
    },
    Attachment {
        handle: u64,
        id: i64,
        attachment_id: String,
    },
    SaveAttachment {
        handle: u64,
        id: i64,
        attachment_id: String,
        destination: std::path::PathBuf,
    },
    Raw {
        handle: u64,
        id: i64,
    },
    Html {
        handle: u64,
        id: i64,
        #[serde(default)]
        allow_remote: bool,
    },
}

fn inbox(handle: u64) -> anyhow::Result<Arc<Inbox>> {
    inboxes()
        .lock()
        .map_err(|_| anyhow::anyhow!("internal_error: registry unavailable"))?
        .get(&handle)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("inbox_closed: unknown or closed inbox"))
}

fn inboxes() -> &'static Mutex<HashMap<u64, Arc<Inbox>>> {
    INBOXES.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn execute(request: Request) -> anyhow::Result<Value> {
    match request {
        Request::Create { options } => {
            let inbox = Arc::new(Inbox::create(options).await?);
            // JSON numbers remain exactly representable in JavaScript.
            let id = NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                    (id < (1 << 53) - 1).then_some(id + 1)
                })
                .map_err(|_| anyhow::anyhow!("resource_limit: inbox handles exhausted"))?;
            let result = json!({"handle": id, "address": inbox.address()});
            inboxes()
                .lock()
                .map_err(|_| anyhow::anyhow!("internal_error: registry unavailable"))?
                .insert(id, inbox);

            Ok(result)
        }
        Request::Wait {
            handle,
            timeout_ms,
            subject,
            from,
            filter,
            format,
        } => {
            let inbox = inbox(handle)?;
            let options = WaitOptions {
                timeout_ms,
                subject,
                from,
                filter,
            };
            let email = match format {
                MessageFormat::Parsed => inbox
                    .wait_for_email(options)
                    .await?
                    .map(serde_json::to_value)
                    .transpose()?,
                MessageFormat::MailMessage => inbox
                    .wait_for_mail_message(options)
                    .await?
                    .map(serde_json::to_value)
                    .transpose()?,
            };

            Ok(match email {
                Some(email) => json!({
                    "status": "matched",
                    "email": email
                }),
                None => json!({"status": "timeout"}),
            })
        }
        Request::Search {
            handle,
            timeout_ms,
            subject,
            from,
            filter,
            query,
        } => {
            let found = inbox(handle)?
                .wait_for_query(
                    query,
                    WaitOptions {
                        timeout_ms,
                        subject,
                        from,
                        filter,
                    },
                )
                .await?;

            Ok(match found {
                Some(found) => {
                    json!({
                        "status": "matched",
                        "matches": found.matches,
                        "values": found.values
                    })
                }
                None => json!({"status": "timeout"}),
            })
        }
        Request::Close { handle } => {
            let inbox = inboxes()
                .lock()
                .map_err(|_| anyhow::anyhow!("internal_error: registry unavailable"))?
                .remove(&handle);
            if let Some(inbox) = inbox {
                inbox.close().await;
            }

            Ok(json!({}))
        }
        Request::ExportCredentials { handle } => {
            Ok(serde_json::to_value(inbox(handle)?.export_credentials()?)?)
        }
        Request::List {
            handle,
            after_id,
            limit,
            subject,
            from,
        } => Ok(serde_json::to_value(
            inbox(handle)?
                .list_emails(Filters {
                    after_id,
                    limit,
                    subject_contains: subject,
                    from_contains: from,
                    ..Default::default()
                })
                .await?,
        )?),
        Request::Get { handle, id, format } => match format {
            MessageFormat::Parsed => Ok(serde_json::to_value(inbox(handle)?.get_email(id).await?)?),
            MessageFormat::MailMessage => Ok(serde_json::to_value(
                inbox(handle)?.get_mail_message(id).await?,
            )?),
        },
        Request::Delete { handle, id } => {
            Ok(json!({"deleted": inbox(handle)?.delete_email(id).await?}))
        }
        Request::Clear { handle } => Ok(json!({"deleted": inbox(handle)?.clear_emails().await?})),
        Request::Attachment {
            handle,
            id,
            attachment_id,
        } => {
            let (attachment, content) = inbox(handle)?.attachment(id, attachment_id).await?;
            Ok(json!({
                "attachment": attachment,
                "content_base64": STANDARD.encode(content)
            }))
        }
        Request::SaveAttachment {
            handle,
            id,
            attachment_id,
            destination,
        } => Ok(
            json!({"path": inbox(handle)?.save_attachment(id, attachment_id, &destination).await?}),
        ),
        Request::Raw { handle, id } => {
            Ok(json!({"content_base64": STANDARD.encode(inbox(handle)?.raw_email(id).await?)}))
        }
        Request::Html {
            handle,
            id,
            allow_remote,
        } => {
            let content = inbox(handle)?.html_email(id, allow_remote).await?;
            Ok(json!({
                "html": content.html,
                "remote_resources": content.remote_resources
            }))
        }
    }
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok": false, "error": {"code": code, "message": message}})
}

fn call_json(input: &str) -> Value {
    if input.len() > 64 * 1024 {
        return failure("invalid_argument", "Request exceeds 64 KiB");
    }

    let request = match serde_json::from_str::<Request>(input) {
        Ok(value) => value,
        // Never include supplied configuration or message values in errors.
        Err(_) => return failure("invalid_argument", "Invalid request or unknown fields"),
    };
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Ahrara runtime unavailable")
    });
    // Foreign callers may provide a small stack (Koffi async workers use
    // 128 KiB). Run parsing/query work on our workers instead of that stack.
    let result = runtime
        .block_on(runtime.spawn(execute(request)))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("internal_error: Native worker failed")));

    match result {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(error) => {
            let message = error.to_string();
            let code = message
                .split_once(':')
                .map(|(code, _)| code)
                .filter(|code| {
                    matches!(
                        *code,
                        "invalid_argument"
                            | "inbox_closed"
                            | "connection_failed"
                            | "resource_limit"
                            | "internal_error"
                            | "email_not_found"
                            | "attachment_not_found"
                            | "invalid_message"
                    )
                })
                .unwrap_or("request_failed");

            failure(code, &message)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ahrara_abi_version() -> u32 {
    1
}

/// # Safety
/// `request_json` must be null or a valid NUL-terminated UTF-8 string whose
/// memory remains readable for this call. The caller owns the returned string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ahrara_call(request_json: *const c_char) -> *mut c_char {
    let response = catch_unwind(AssertUnwindSafe(|| {
        if request_json.is_null() {
            return failure("invalid_argument", "Request must not be null");
        }

        // SAFETY: the C caller guarantees a readable NUL-terminated string.
        match unsafe { CStr::from_ptr(request_json) }.to_str() {
            Ok(input) => call_json(input),
            Err(_) => failure("invalid_argument", "Request must be UTF-8"),
        }
    }))
    .unwrap_or_else(|_| failure("internal_error", "Native call failed"));

    CString::new(response.to_string())
        .expect("JSON escapes NUL")
        .into_raw()
}

/// # Safety
/// `response_json` must be null or an unfreed pointer returned by `ahrara_call`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ahrara_string_free(response_json: *mut c_char) {
    if !response_json.is_null() {
        // SAFETY: allocation originated from CString::into_raw above.
        drop(unsafe { CString::from_raw(response_json) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_errors_and_owned_utf8_responses() {
        assert_eq!(ahrara_abi_version(), 1);
        for request in ["{", r#"{"op":"missing"}"#, r#"{"op":"wait","handle":42}"#] {
            let input = CString::new(request).unwrap();
            // SAFETY: both allocations are owned and retained through the call.
            unsafe {
                let pointer = ahrara_call(input.as_ptr());
                let response: Value =
                    serde_json::from_slice(CStr::from_ptr(pointer).to_bytes()).unwrap();

                assert_eq!(response["ok"], false);
                assert!(response["error"]["code"].is_string());
                ahrara_string_free(pointer);
            }
        }

        unsafe {
            let pointer = ahrara_call(std::ptr::null());
            assert!(!pointer.is_null());
            ahrara_string_free(pointer);
            ahrara_string_free(std::ptr::null_mut());
        }
    }
}
