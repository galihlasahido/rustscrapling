//! The sessions a crawl fetches through, a port of `scrapling/spiders/session.py`.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::response::{Response, META_SID};

use super::request::Request;

/// One way of fetching a request.
#[derive(Debug, Clone)]
pub enum Session {
    /// A reusable HTTP session.
    Http(crate::http::FetcherSession),
    /// A browser session.
    #[cfg(feature = "browser")]
    Browser(crate::browser::DynamicSession),
}

impl Session {
    /// Start the session if it needs starting; HTTP sessions are ready straight away.
    pub async fn start(&self) -> Result<()> {
        match self {
            Session::Http(_) => Ok(()),
            #[cfg(feature = "browser")]
            Session::Browser(session) => session.start().await,
        }
    }

    /// Close the session and release whatever it holds.
    pub async fn close(&self) -> Result<()> {
        match self {
            Session::Http(_) => Ok(()),
            #[cfg(feature = "browser")]
            Session::Browser(session) => session.close().await,
        }
    }

    /// Fetch one request through this session.
    pub async fn fetch(&self, request: &Request) -> Result<Response> {
        match self {
            Session::Http(session) => {
                let options = &request.options;
                let mut builder = session.request(options.method_or_get(), &request.url);
                for (name, value) in &options.headers {
                    builder = builder.header(name, value);
                }
                if let Some(form) = &options.form {
                    builder = builder.form(form.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                if let Some(json) = &options.json {
                    builder = builder.json(json)?;
                }
                if let Some(body) = &options.body {
                    builder = builder.body(body.clone());
                }
                if let Some(proxy) = &options.proxy {
                    builder = builder.proxy(proxy);
                }
                if let Some(timeout) = options.timeout {
                    builder = builder.timeout(timeout);
                }
                builder.send().await
            }
            #[cfg(feature = "browser")]
            Session::Browser(session) => session.fetch(&request.url).await,
        }
    }
}

/// The sessions a spider may fetch with, keyed by id.
#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: HashMap<String, Session>,
    order: Vec<String>,
    default_id: Option<String>,
}

impl SessionManager {
    /// An empty manager.
    pub fn new() -> SessionManager {
        SessionManager::default()
    }

    /// Register a session; the first one registered becomes the default.
    pub fn add(&mut self, id: impl Into<String>, session: Session) -> Result<()> {
        self.insert(id.into(), session, false)
    }

    /// Register a session and make it the default.
    pub fn add_default(&mut self, id: impl Into<String>, session: Session) -> Result<()> {
        self.insert(id.into(), session, true)
    }

    fn insert(&mut self, id: String, session: Session, default: bool) -> Result<()> {
        if self.sessions.contains_key(&id) {
            return Err(Error::Spider(format!("session '{id}' already registered")));
        }
        if default || self.default_id.is_none() {
            self.default_id = Some(id.clone());
        }
        self.order.push(id.clone());
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Remove a session and return it.
    pub fn remove(&mut self, id: &str) -> Option<Session> {
        let session = self.sessions.remove(id)?;
        self.order.retain(|known| known != id);
        if self.default_id.as_deref() == Some(id) {
            self.default_id = self.order.first().cloned();
        }
        Some(session)
    }

    /// The id requests fall back to when they name no session.
    pub fn default_session_id(&self) -> Result<&str> {
        self.default_id
            .as_deref()
            .ok_or_else(|| Error::Spider("no sessions registered".to_string()))
    }

    /// Every registered session id, in registration order.
    pub fn session_ids(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Look a session up by id.
    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    /// Number of registered sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Start every session that needs starting (browsers launch here).
    pub async fn start(&self) -> Result<()> {
        for id in &self.order {
            if let Some(session) = self.sessions.get(id) {
                session.start().await?;
            }
        }
        Ok(())
    }

    /// Close every session; the first failure is reported once they have all been tried.
    pub async fn close(&self) -> Result<()> {
        let mut first_error: Option<Error> = None;
        for id in &self.order {
            if let Some(session) = self.sessions.get(id) {
                if let Err(error) = session.close().await {
                    tracing::warn!(session = %id, %error, "failed to close a session");
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Fetch a request with the session it names, or the default one.
    ///
    /// The request's `meta` is merged into the response's, with the response winning, and the
    /// session id the request went through is recorded under [`META_SID`], which is where
    /// [`Response::follow`](crate::response::Response::follow) reads it from.
    pub async fn fetch(&self, request: &Request) -> Result<Response> {
        let sid = if request.sid.is_empty() {
            self.default_session_id()?.to_string()
        } else {
            request.sid.clone()
        };

        let session = self
            .get(&sid)
            .ok_or_else(|| Error::Spider(format!("no session registered under '{sid}'")))?;

        let mut response = session.fetch(request).await?;
        for (key, value) in &request.meta {
            response
                .meta
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        response
            .meta
            .insert(META_SID.to_string(), serde_json::Value::String(sid));
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_session() -> Result<Session> {
        Ok(Session::Http(crate::http::FetcherSession::new()?))
    }

    #[test]
    fn the_first_session_becomes_the_default() {
        let mut manager = SessionManager::new();
        assert!(manager.is_empty());
        assert!(manager.default_session_id().is_err());

        manager
            .add("default", http_session().expect("a session"))
            .expect("add");
        manager
            .add("second", http_session().expect("a session"))
            .expect("add");
        assert_eq!(manager.len(), 2);
        assert_eq!(manager.default_session_id().ok(), Some("default"));
        assert_eq!(
            manager.session_ids(),
            vec!["default".to_string(), "second".to_string()]
        );
        assert!(manager.get("second").is_some());
        assert!(manager.get("missing").is_none());
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let mut manager = SessionManager::new();
        manager
            .add("default", http_session().expect("a session"))
            .expect("add");
        assert!(manager
            .add("default", http_session().expect("a session"))
            .is_err());
    }

    #[test]
    fn add_default_and_remove_move_the_default_along() {
        let mut manager = SessionManager::new();
        manager
            .add("first", http_session().expect("a session"))
            .expect("add");
        manager
            .add_default("second", http_session().expect("a session"))
            .expect("add");
        assert_eq!(manager.default_session_id().ok(), Some("second"));

        assert!(manager.remove("second").is_some());
        assert_eq!(manager.default_session_id().ok(), Some("first"));
        assert!(manager.remove("missing").is_none());
    }

    #[tokio::test]
    async fn fetching_through_an_unknown_session_is_an_error() {
        let mut manager = SessionManager::new();
        manager
            .add("default", http_session().expect("a session"))
            .expect("add");
        let request = Request::new("http://127.0.0.1:1/").sid("nope");
        assert!(manager.fetch(&request).await.is_err());
    }
}
