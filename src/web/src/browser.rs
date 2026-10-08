//! The browser app. A Service Worker answers the UI from IndexedDB with
//! `respond`, and a dedicated worker keeps the relay connection with `Engine`.
//! JavaScript only moves bytes: records are sealed here with a key derived from
//! the Identity, as the CLI encrypts its database.
use crate::{
    Backend, Mode, Page, Status, Ui, failure, harden, html, i18n::Strings, missing, redirect,
    verify,
};
use ahrara_core::{
    auth::{self, Identity},
    mail,
    model::{self, Address, CustomDomain, Email},
    protocol::solve_work,
    session::{Session, Step},
};
use anyhow::{Context, Result, anyhow, bail};
use askama::Template;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, aead::Aead};
use chrono::Utc;
use http::{Method, Request, Response, StatusCode};
use js_sys::{Array, Object, Reflect, Uint8Array};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

const RELAY_URL: &str = match option_env!("AHRARA_WEB_RELAY_URL") {
    Some(url) => url,
    None => "wss://relay.ahrara.dev",
};

/// IndexedDB, through www/app/store.js. Every call is one transaction.
mod idb {
    use js_sys::Array;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn get(store: &str, key: &JsValue) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn all(store: &str) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn count(store: &str) -> Result<JsValue, JsValue>;
        /// Resolves to the record's key.
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn put(store: &str, value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
        /// Rejects when the key exists.
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn add(store: &str, value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn remove(stores: Array, key: &JsValue) -> Result<JsValue, JsValue>;
        /// Stores new deliveries; resolves to their ids, null for ones stored before.
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn deliver(entries: Array) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_namespace = ahraraStore, catch)]
        pub async fn erase() -> Result<JsValue, JsValue>;
        /// Tells every page and worker of this origin, as the CLI's events do.
        #[wasm_bindgen(js_namespace = ahraraStore)]
        pub fn emit(kind: &str, data: &str);
    }
}

fn js(error: JsValue) -> anyhow::Error {
    match error.dyn_ref::<js_sys::Error>() {
        Some(error) => anyhow!("storage: {}", String::from(error.message())),
        None => anyhow!("storage: {error:?}"),
    }
}

fn field(record: &JsValue, name: &str) -> Result<JsValue> {
    Reflect::get(record, &name.into()).map_err(js)
}

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> Object {
    let object = Object::new();
    for (name, value) in fields {
        Reflect::set(&object, &name.into(), &value).expect("plain objects accept fields");
    }
    object
}

async fn kv(key: &str) -> Result<JsValue> {
    idb::get("kv", &key.into()).await.map_err(js)
}

async fn set_kv(key: &str, value: &JsValue) -> Result<()> {
    idb::put("kv", value, &key.into()).await.map_err(js)?;
    Ok(())
}

async fn csrf() -> Result<String> {
    if let Some(token) = kv("csrf").await?.as_string() {
        return Ok(token);
    }

    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    set_kv("csrf", &token.as_str().into()).await?;
    Ok(token)
}

async fn set_state(state: &str, error: Option<String>) -> Result<()> {
    let value = serde_json::json!({
        "connected": state == "connected",
        "state": state,
        "last_error": error,
    })
    .to_string();
    set_kv("state", &value.as_str().into()).await?;
    idb::emit("connection_state_changed", &value);
    Ok(())
}

struct Browser {
    identity: Identity,
    key: Zeroizing<[u8; 32]>,
    csrf: String,
    backed_up: bool,
}

impl Browser {
    async fn load() -> Result<Option<Self>> {
        let Some(recovery) = kv("identity").await?.as_string().map(Zeroizing::new) else {
            return Ok(None);
        };
        let identity = Identity::from_recovery(&recovery)?;
        Ok(Some(Self {
            key: identity.browser_store_key(),
            identity,
            csrf: String::new(),
            backed_up: kv("backup").await?.is_truthy(),
        }))
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&*self.key))
    }

    fn seal(&self, plain: &[u8]) -> Uint8Array {
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let mut sealed = nonce.to_vec();
        sealed.extend(
            self.cipher()
                .encrypt(Nonce::from_slice(&nonce), plain)
                .expect("encryption in memory cannot fail"),
        );
        Uint8Array::from(sealed.as_slice())
    }

    fn open(&self, sealed: &JsValue) -> Result<Vec<u8>> {
        let sealed = sealed
            .dyn_ref::<Uint8Array>()
            .context("missing record")?
            .to_vec();
        anyhow::ensure!(sealed.len() > 12, "corrupt record");
        let (nonce, data) = sealed.split_at(12);
        self.cipher()
            .decrypt(Nonce::from_slice(nonce), data)
            .map_err(|_| anyhow!("record sealed by another Identity"))
    }

    fn seal_json(&self, value: &impl Serialize) -> Result<Uint8Array> {
        Ok(self.seal(&serde_json::to_vec(value)?))
    }

    fn open_json<T: DeserializeOwned>(&self, sealed: &JsValue) -> Result<T> {
        Ok(serde_json::from_slice(&self.open(sealed)?)?)
    }

    /// Each record of an id-keyed store, oldest first. A delivery that commits
    /// after "Delete local data" leaves a record sealed by the previous Identity;
    /// it can never be read, so it is skipped.
    async fn records<T: DeserializeOwned>(&self, store: &str) -> Result<Vec<(i64, T)>> {
        let records: Array = idb::all(store).await.map_err(js)?.unchecked_into();
        Ok(records
            .iter()
            .filter_map(|record| {
                let id = field(&record, "id").ok()?.as_f64()? as i64;
                Some((id, self.open_json(&field(&record, "sealed").ok()?).ok()?))
            })
            .collect())
    }

    async fn all_addresses(&self) -> Result<Vec<Address>> {
        Ok(self
            .records("addresses")
            .await?
            .into_iter()
            .map(|(id, address)| Address { id, ..address })
            .collect())
    }

    async fn save_address(&self, address: &mut Address, kind: &str) -> Result<()> {
        let sealed = self.seal_json(address)?.into();
        let record = if address.id > 0 {
            object([("id", (address.id as f64).into()), ("sealed", sealed)])
        } else {
            object([("sealed", sealed)])
        };
        let id = idb::put("addresses", &record, &JsValue::UNDEFINED)
            .await
            .map_err(js)?;
        address.id = id.as_f64().context("address without id")? as i64;
        idb::emit(kind, &serde_json::to_string(address)?);
        Ok(())
    }

    /// An address by local id, canonical address or custom-domain address.
    async fn find(&self, selector: &str) -> Result<Address> {
        let id = selector
            .parse::<i64>()
            .ok()
            .filter(|id| id.to_string() == selector);
        self.all_addresses()
            .await?
            .into_iter()
            .find(|a| match id {
                Some(id) => a.id == id,
                None => a.routing_address == selector || a.address == selector,
            })
            .context("address_not_found: address does not exist")
    }

    /// Mail for a custom-domain address is stored under its forwarding address.
    async fn routing(&self, address: &str) -> Result<String> {
        if let Ok((_, _, canonical)) = auth::decode_address(address) {
            return Ok(canonical);
        }

        let (token, domain) = address.split_once('@').context("invalid address")?;
        let public = format!("{token}@{}", model::normalize_domain(domain)?);
        Ok(self.find(&public).await?.forwarding_address)
    }

    fn address(&self, alias: [u8; 8], label: Option<String>) -> Address {
        let route = self.identity.route();
        let canonical = auth::address(&route, &alias);
        Address {
            id: 0,
            identity_fingerprint: self.identity.fingerprint(),
            route_id: hex::encode(route),
            alias_id: hex::encode(alias),
            address: canonical.clone(),
            forwarding_address: canonical.clone(),
            routing_address: canonical,
            label,
            created_at: Utc::now().to_rfc3339(),
            enabled: true,
        }
    }

    /// Receives here an address created by the CLI or another browser with this Identity.
    async fn restore_address(&self, address: &str) -> Result<()> {
        let (route, alias, canonical) = auth::decode_address(address.trim())?;
        if route != self.identity.route() {
            bail!("address belongs to another Identity")
        }

        match self.find(&canonical).await {
            Ok(mut existing) => {
                existing.enabled = true;
                self.save_address(&mut existing, "address_updated").await
            }
            Err(_) => {
                self.save_address(&mut self.address(alias, None), "address_created")
                    .await
            }
        }
    }

    async fn accepts(&self, recipient: &str) -> Result<bool> {
        Ok(self
            .all_addresses()
            .await?
            .iter()
            .any(|a| a.routing_address == recipient && a.enabled))
    }

    /// Stores one message per recipient. Repeated transfers are stored once.
    async fn deliver(
        &self,
        transfer: String,
        recipients: Vec<String>,
        sender: String,
        raw: Vec<u8>,
    ) -> Result<()> {
        let summary = mail::summary(&raw);
        let received = Utc::now().to_rfc3339();
        let fingerprint = self.identity.fingerprint();
        let sealed = JsValue::from(self.seal(&raw));
        let entries = Array::new();
        let mut emails = Vec::new();

        for recipient in recipients {
            // A keyed digest finds repeated transfers without storing recipients in clear.
            let delivery = Sha256::new()
                .chain_update(self.key.as_slice())
                .chain_update(transfer.as_bytes())
                .chain_update([0])
                .chain_update(recipient.as_bytes())
                .finalize();
            let email = Email {
                id: 0,
                transfer_id: transfer.clone(),
                identity_fingerprint: fingerprint.clone(),
                recipient,
                envelope_sender: sender.clone(),
                received_at: received.clone(),
                from_header: summary.from.clone(),
                to_header: summary.to.clone(),
                subject: summary.subject.clone(),
                message_id: summary.message_id.clone(),
                size_bytes: raw.len() as i64,
            };
            entries.push(&object([
                ("delivery", hex::encode(delivery).into()),
                ("sealed", self.seal_json(&email)?.into()),
                ("raw", sealed.clone()),
            ]));
            emails.push(email);
        }

        let ids: Array = idb::deliver(entries).await.map_err(js)?.unchecked_into();
        for (email, id) in emails.into_iter().zip(ids.iter()) {
            if let Some(id) = id.as_f64() {
                let email = Email {
                    id: id as i64,
                    ..email
                };
                idb::emit("email_received", &serde_json::to_string(&email)?);
            }
        }

        Ok(())
    }
}

impl Backend for Browser {
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn mode(&self) -> Mode {
        Mode::Browser
    }

    fn csrf(&self) -> &str {
        &self.csrf
    }

    async fn status(&self) -> Result<Status> {
        let state = kv("state").await?.as_string().unwrap_or_default();
        Ok(Status {
            connected: serde_json::from_str::<serde_json::Value>(&state)
                .is_ok_and(|state| state["connected"] == true),
            emails: idb::count("messages")
                .await
                .map_err(js)?
                .as_f64()
                .unwrap_or(0.) as i64,
            fingerprint: self.identity.fingerprint(),
            route: hex::encode(self.identity.route()),
        })
    }

    async fn latest_enabled_address(&self) -> Result<Option<Address>> {
        Ok(self.all_addresses().await?.into_iter().rfind(|a| a.enabled))
    }

    async fn inbox_page(
        &self,
        filters: model::Filters,
        before: Option<i64>,
        search: String,
    ) -> Result<Vec<Email>> {
        filters.validate()?;
        let recipient = match &filters.address {
            Some(address) => Some(self.routing(address).await?),
            None => None,
        };
        let search = search.to_lowercase();
        // ponytail: decrypts every summary per page; an index needs plaintext fields or a cache.
        Ok(self
            .records::<Email>("messages")
            .await?
            .into_iter()
            .rev()
            .map(|(id, email)| Email { id, ..email })
            .filter(|e| {
                recipient.as_ref().is_none_or(|r| &e.recipient == r)
                    && before.is_none_or(|before| e.id < before)
                    && (search.is_empty()
                        || format!("{} {} {}", e.from_header, e.envelope_sender, e.subject)
                            .to_lowercase()
                            .contains(&search))
            })
            .take(filters.limit.unwrap_or(20) as usize)
            .collect())
    }

    async fn addresses(&self, after_id: i64, limit: u32) -> Result<Vec<Address>> {
        Ok(self
            .all_addresses()
            .await?
            .into_iter()
            .filter(|a| a.id > after_id)
            .take(limit as usize)
            .collect())
    }

    async fn email(&self, id: i64) -> Result<(Email, Vec<u8>)> {
        let key = JsValue::from(id as f64);
        let record = idb::get("messages", &key).await.map_err(js)?;
        if record.is_undefined() {
            bail!("email_not_found: email does not exist")
        }

        let email: Email = self.open_json(&field(&record, "sealed")?)?;
        let raw = self.open(&idb::get("raw", &key).await.map_err(js)?)?;
        Ok((Email { id, ..email }, raw))
    }

    async fn create_address(&self, label: Option<String>) -> Result<()> {
        if label.as_ref().is_some_and(|s| s.len() > 256) {
            bail!("invalid_argument: label must not exceed 256 UTF-8 bytes")
        }

        let existing = self.all_addresses().await?;
        let custom = self.custom_domain().await?;
        if let Some(domain) = &custom
            && !existing
                .iter()
                .any(|a| a.routing_address == domain.forwarding_address && a.enabled)
        {
            bail!("invalid_argument: custom domain forwarding destination is disabled")
        }

        let mut alias = [0; 8];
        let mut address = loop {
            OsRng.fill_bytes(&mut alias);
            let address = self.address(alias, label.clone());
            if !existing
                .iter()
                .any(|a| a.routing_address == address.routing_address)
            {
                break address;
            }
        };
        if let Some(domain) = custom {
            let (token, _) = address.routing_address.split_once('@').expect("an address");
            address.address = format!("{token}@{}", domain.domain);
            address.forwarding_address = domain.forwarding_address;
        }

        self.save_address(&mut address, "address_created").await
    }

    async fn change_address(&self, id: i64, enabled: Option<bool>) -> Result<()> {
        let mut address = self.find(&id.to_string()).await?;
        let Some(enabled) = enabled else {
            idb::remove(Array::of1(&"addresses".into()), &(id as f64).into())
                .await
                .map_err(js)?;
            idb::emit("address_deleted", &serde_json::to_string(&address)?);
            return Ok(());
        };

        address.enabled = enabled;
        self.save_address(&mut address, "address_updated").await
    }

    async fn delete_email(&self, id: i64) -> Result<()> {
        idb::remove(
            Array::of2(&"messages".into(), &"raw".into()),
            &(id as f64).into(),
        )
        .await
        .map_err(js)?;
        idb::emit("email_deleted", &serde_json::json!({"id": id}).to_string());
        Ok(())
    }

    async fn custom_domain(&self) -> Result<Option<CustomDomain>> {
        let sealed = kv("domain").await?;
        if sealed.is_undefined() {
            return Ok(None);
        }

        self.open_json(&sealed).map(Some)
    }

    async fn set_custom_domain(&self, domain: String, forwarding_address: String) -> Result<()> {
        let domain = model::normalize_domain(&domain)?;
        let destination = self.find(&forwarding_address).await?;
        if !destination.enabled {
            bail!("forwarding destination must be an enabled local Ahrara address")
        }

        let value = CustomDomain {
            domain,
            forwarding_address: destination.routing_address,
        };
        set_kv("domain", &self.seal_json(&value)?.into()).await
    }

    async fn remove_custom_domain(&self) -> Result<()> {
        idb::remove(Array::of1(&"kv".into()), &"domain".into())
            .await
            .map_err(js)?;
        Ok(())
    }

    async fn language(&self) -> Option<String> {
        kv("language").await.ok()?.as_string()
    }

    async fn set_language(&self, language: Option<String>) -> Result<()> {
        match language {
            Some(language) => set_kv("language", &language.into()).await,
            None => idb::remove(Array::of1(&"kv".into()), &"language".into())
                .await
                .map(drop)
                .map_err(js),
        }
    }
}

#[derive(Template)]
#[template(path = "welcome.html")]
struct Welcome {
    csrf: String,
    error: bool,
    t: &'static Strings,
}

#[derive(Template)]
#[template(path = "backup.html")]
struct Backup {
    csrf: String,
    recovery: Zeroizing<String>,
    t: &'static Strings,
}

#[derive(Deserialize)]
struct Form {
    csrf: String,
    #[serde(default)]
    action: String,
    recovery_key: Option<String>,
    address: Option<String>,
    #[serde(default)]
    confirm: bool,
}

fn page(template: impl Template, status: StatusCode) -> Result<Response<Vec<u8>>> {
    let mut response = html(template.render()?);
    *response.status_mut() = status;
    Ok(response)
}

/// Answers one request from the UI frame, as the CLI's local server would.
/// `languages` is the browser's preference list. Resolves to
/// `{status, headers: [[name, value]], body}`.
#[wasm_bindgen]
pub async fn respond(
    method: String,
    url: String,
    body: Vec<u8>,
    languages: String,
) -> Result<Object, JsError> {
    let mut request = Request::builder()
        .method(method.as_str())
        .uri(url)
        .body(body)
        .map_err(|error| JsError::new(&error.to_string()))?;
    if let Ok(value) = http::HeaderValue::from_str(&languages) {
        request
            .headers_mut()
            .insert(http::header::ACCEPT_LANGUAGE, value);
    }

    let language = kv("language")
        .await
        .ok()
        .and_then(|value| value.as_string());
    let t = crate::strings(language.as_deref(), &request);
    let mut response = answer(request, t, language)
        .await
        .unwrap_or_else(|error| failure(missing(&error), t));
    harden(response.headers_mut(), true);
    let headers = Array::new();
    for (name, value) in response.headers() {
        headers.push(&Array::of2(
            &name.as_str().into(),
            &value.to_str().unwrap_or_default().into(),
        ));
    }

    Ok(object([
        ("status", response.status().as_u16().into()),
        ("headers", headers.into()),
        ("body", Uint8Array::from(response.body().as_slice()).into()),
    ]))
}

async fn answer(
    request: Request<Vec<u8>>,
    t: &'static Strings,
    language: Option<String>,
) -> Result<Response<Vec<u8>>> {
    let csrf = csrf().await?;
    let post = request.method() == Method::POST;
    let path = request.uri().path().to_owned();
    let form = || -> Result<Form> {
        let form: Form = serde_urlencoded::from_bytes(request.body())?;
        verify(&form.csrf, &csrf)?;
        Ok(form)
    };

    let Some(mut browser) = Browser::load().await? else {
        return match (post, path.as_str()) {
            (false, "/welcome") => page(
                Welcome {
                    csrf,
                    error: false,
                    t,
                },
                StatusCode::OK,
            ),
            (true, "/welcome") => {
                let form = form()?;
                let identity = match form.action.as_str() {
                    "create" => Identity::generate(),
                    "import" => {
                        let recovery = Zeroizing::new(form.recovery_key.unwrap_or_default());
                        let Ok(identity) = Identity::from_recovery(&recovery) else {
                            return page(
                                Welcome {
                                    csrf,
                                    error: true,
                                    t,
                                },
                                StatusCode::BAD_REQUEST,
                            );
                        };
                        identity
                    }
                    _ => bail!("invalid action"),
                };
                // Another tab may have stored an Identity meanwhile; never replace it.
                let key = identity.export();
                if idb::add("kv", &key.as_str().into(), &"identity".into())
                    .await
                    .is_err()
                {
                    return Ok(redirect("/"));
                }
                if form.action == "import" {
                    // An imported key is already saved elsewhere.
                    set_kv("backup", &true.into()).await?;
                }

                idb::emit("identity_changed", "{}");
                Ok(redirect(if form.action == "import" {
                    "/addresses"
                } else {
                    "/backup"
                }))
            }
            _ => Ok(redirect("/welcome")),
        };
    };
    browser.csrf = csrf.clone();

    match (post, path.as_str()) {
        (_, "/welcome") => Ok(redirect("/")),
        // After the first backup, Settings shows the key behind a confirmation.
        (false, "/backup") if browser.backed_up => Ok(redirect("/settings")),
        (false, "/backup") => page(
            Backup {
                csrf: csrf.clone(),
                recovery: browser.identity.export(),
                t,
            },
            StatusCode::OK,
        ),
        (true, "/backup") => {
            if !form()?.confirm {
                bail!("confirmation required")
            }

            set_kv("backup", &true.into()).await?;
            Ok(redirect("/addresses#create-address"))
        }
        _ if !browser.backed_up => Ok(redirect("/backup")),
        (true, "/addresses/existing") => {
            browser
                .restore_address(&form()?.address.unwrap_or_default())
                .await?;
            Ok(redirect("/addresses"))
        }
        (true, "/settings/recovery-key") => {
            if !form()?.confirm {
                bail!("confirmation required")
            }

            let ui = Ui {
                mode: Mode::Browser,
                t,
                language,
            };
            let mut page = Page::new(&browser, ui, "settings", t.settings).await?;
            page.recovery = Some(browser.identity.export());
            page.response()
        }
        (true, "/settings/erase") => {
            if !form()?.confirm {
                bail!("confirmation required")
            }

            idb::erase().await.map_err(js)?;
            idb::emit("identity_changed", "{}");
            Ok(redirect("/welcome"))
        }
        _ => Ok(crate::respond(&browser, request).await),
    }
}

#[wasm_bindgen]
pub fn relay_url() -> String {
    RELAY_URL.into()
}

/// Records the relay connection state for every page.
#[wasm_bindgen]
pub async fn connection(state: String, error: Option<String>) -> Result<(), JsError> {
    set_state(&state, error).await.map_err(fail)
}

fn fail(error: anyhow::Error) -> JsError {
    JsError::new(&error.to_string())
}

/// One relay session of the stored Identity. Feed it every message in order.
#[wasm_bindgen]
pub struct Engine {
    browser: Rc<Browser>,
    session: Rc<RefCell<Session>>,
    ready: Rc<Cell<bool>>,
    hello: Vec<u8>,
}

#[wasm_bindgen]
impl Engine {
    /// Resolves to undefined until this browser has an Identity.
    pub async fn open() -> Result<Option<Engine>, JsError> {
        let Some(browser) = Browser::load().await.map_err(fail)? else {
            return Ok(None);
        };
        let key = option_env!("AHRARA_SERVER_PUBLIC_KEY")
            .context("this build has no relay key")
            .and_then(ahrara_core::secure::parse_public_key)
            .map_err(fail)?;
        let (session, hello) = Session::open(browser.identity.clone(), &key).map_err(fail)?;
        Ok(Some(Engine {
            browser: Rc::new(browser),
            session: Rc::new(RefCell::new(session)),
            ready: Rc::default(),
            hello,
        }))
    }

    /// The handshake's first message.
    pub fn hello(&self) -> Vec<u8> {
        self.hello.clone()
    }

    pub fn ready(&self) -> bool {
        self.ready.get()
    }

    /// Resolves to the bytes to send back, if any.
    pub fn receive(&self, packet: Vec<u8>) -> js_sys::Promise {
        let (browser, session, ready) = (
            self.browser.clone(),
            self.session.clone(),
            self.ready.clone(),
        );
        wasm_bindgen_futures::future_to_promise(async move {
            match step(&browser, &session, &ready, &packet).await {
                Ok(Some(reply)) => Ok(Uint8Array::from(reply.as_slice()).into()),
                Ok(None) => Ok(JsValue::UNDEFINED),
                Err(error) => Err(fail(error).into()),
            }
        })
    }

    pub fn keepalive(&self) -> Result<Vec<u8>, JsError> {
        self.session.borrow_mut().keepalive().map_err(fail)
    }
}

async fn step(
    browser: &Browser,
    session: &RefCell<Session>,
    ready: &Cell<bool>,
    packet: &[u8],
) -> Result<Option<Vec<u8>>> {
    let step = session.borrow_mut().receive(packet)?;
    Ok(match step {
        Step::Send(reply) => Some(reply),
        // The worker may block: no page runs on its thread.
        Step::Work { challenge, bits } => {
            Some(session.borrow_mut().work(solve_work(&challenge, bits))?)
        }
        Step::Ready => {
            ready.set(true);
            set_state("connected", None).await?;
            None
        }
        Step::Offer { recipient } => {
            let accept = browser.accepts(&recipient).await?;
            Some(session.borrow_mut().offer(accept)?)
        }
        Step::Deliver {
            transfer_id,
            recipients,
            sender,
            raw,
        } => {
            let saved = browser.deliver(transfer_id, recipients, sender, raw).await;
            Some(session.borrow_mut().stored(saved.is_ok())?)
        }
        Step::Idle => None,
    })
}
